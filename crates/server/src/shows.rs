//! Podcasts you follow: shows from their RSS feeds (found through Apple's podcast directory, or
//! added by address or from another app's OPML export), where you are in each episode, and the
//! ads in them, found so the player can skip them.
//!
//! Finding ads: the episode is downloaded (ads that are stitched in when it's fetched differ from
//! one download to the next, so the copy that's checked is the one that's played), the voice
//! server's Whisper writes down what's said a few minutes at a time, and the chat model (or the
//! local one) reads each stretch and says which lines are ads. The player skips what's been found
//! as it goes, so an episode can be played while the rest is still being checked.
//!
//! Everything but the list of shows lives under `data/shows/`: `settings.json`, and a folder per
//! downloaded episode with the recording, `scan.json` (the ads) and `transcript.json`.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, Request, State as AxState},
    http::{StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post, put},
};
use chrono::{DateTime, Utc};
use duet_common::shows::{AdScan, AdSpan, Chart, GENRES, Outline, OutlineSection, Show, Speaker, ShowEpisode, ShowHit, ShowPage, ShowPosition, ShowSettings, ShowsHome};
use futures_util::StreamExt;
use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::{
    State,
    error::{ApiError, ApiResult},
    podcast::feeds::{clip, strip_html},
};

/// The first stretch listened to is short, so the opening ads are known quickly.
const FIRST_WINDOW: f64 = 240.0;
const WINDOW: f64 = 900.0;
/// Each stretch is read with the end of the one before it, so an ad across the join is whole.
const OVERLAP: f64 = 120.0;
/// The way ads are found now: 2 cuts inside a transcript line. Episodes checked an older way are
/// read again (from the transcript they have) when they're played.
const CUTS: u32 = 2;

pub fn routes() -> Router<State> {
    Router::new()
        .route("/shows", get(home).post(follow))
        .route("/shows/search", get(search))
        .route("/shows/top", get(top))
        .route("/shows/charts", get(charts))
        .route("/shows/preview", get(preview))
        .route("/shows/refresh", post(refresh_all))
        .route("/shows/settings", get(get_settings).put(put_settings))
        .route("/shows/opml", get(export_opml).post(import_opml))
        .route("/shows/{id}", get(show_page).patch(edit_show).delete(unfollow))
        .route("/shows/{id}/played", post(mark_all_played))
        .route("/shows/episodes/{id}", get(episode))
        .route("/shows/episodes/{id}/audio", get(audio))
        .route("/shows/episodes/{id}/position", put(put_position))
        .route("/shows/episodes/{id}/prepare", post(prepare))
        .route("/shows/episodes/{id}/ads", get(ads))
        .route("/shows/episodes/{id}/ads/{n}/remove", post(not_an_ad))
        .route("/shows/episodes/{id}/rescan", post(rescan))
        .route("/shows/episodes/{id}/transcript", get(transcript))
        .route("/shows/episodes/{id}/outline", get(get_outline).post(remake_outline))
}

fn base(state: &State) -> PathBuf {
    state.data_dir.join("shows")
}

fn episode_dir(state: &State, id: i64) -> PathBuf {
    base(state).join("episodes").join(id.to_string())
}

async fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    serde_json::from_slice(&tokio::fs::read(path).await.ok()?).ok()
}

async fn write_json<T: Serialize>(path: &std::path::Path, v: &T) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec(v)?).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

pub async fn settings(state: &State) -> ShowSettings {
    read_json(&base(state).join("settings.json")).await.unwrap_or_default()
}

// ---- Feeds ----

#[derive(Debug, Default, Clone, PartialEq)]
struct FeedShow {
    title: String,
    author: String,
    description: String,
    image: Option<String>,
    link: String,
    episodes: Vec<FeedEpisode>,
}

#[derive(Debug, Default, Clone, PartialEq)]
struct FeedEpisode {
    guid: String,
    title: String,
    notes: String,
    published: Option<DateTime<Utc>>,
    duration: f64,
    audio_url: String,
    image: Option<String>,
    /// (Atom: the entry's page.)
    link: String,
}

/// "1:02:03", "62:03", "3723" or "3723.5" → seconds.
fn parse_duration(s: &str) -> f64 {
    let s = s.trim();
    if s.contains(':') {
        s.split(':').fold(0.0, |acc, part| acc * 60.0 + part.trim().parse::<f64>().unwrap_or(0.0))
    } else {
        s.parse().unwrap_or(0.0)
    }
}

fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    DateTime::parse_from_rfc2822(s)
        .or_else(|_| DateTime::parse_from_rfc3339(s))
        // Some feeds leave the day name off, or put a wrong one on.
        .or_else(|_| DateTime::parse_from_rfc2822(s.split_once(", ").map(|(_, r)| r).unwrap_or(s)))
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn attr(e: &quick_xml::events::BytesStart, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key.as_bytes())
        .and_then(|a| a.unescape_value().ok().map(|v| v.to_string()))
}

fn entity(name: &str) -> String {
    match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32).map(String::from).unwrap_or_default(),
        n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32).map(String::from).unwrap_or_default(),
        _ => String::new(),
    }
}

/// A podcast's RSS feed: the show and its episodes (only those with a recording).
fn parse_feed(xml: &str) -> FeedShow {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut show = FeedShow::default();
    let mut item: Option<FeedEpisode> = None;
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut long_notes = String::new();
    let take_attrs = |name: &str, e: &quick_xml::events::BytesStart, show: &mut FeedShow, item: &mut Option<FeedEpisode>| match (name, item.as_mut()) {
        ("enclosure", Some(i)) => {
            let kind = attr(e, "type").unwrap_or_default();
            if i.audio_url.is_empty() && (kind.is_empty() || kind.starts_with("audio") || kind.starts_with("video")) {
                i.audio_url = attr(e, "url").unwrap_or_default();
            }
        }
        ("itunes:image", Some(i)) => i.image = attr(e, "href").or(i.image.take()),
        // (Atom: a podcast's recording, or a YouTube video's page.)
        ("link", Some(i)) => match attr(e, "rel").as_deref() {
            Some("enclosure") if i.audio_url.is_empty() => i.audio_url = attr(e, "href").unwrap_or_default(),
            None | Some("alternate") => i.link = attr(e, "href").unwrap_or_default(),
            _ => {}
        },
        ("link", None) if show.link.is_empty() && attr(e, "rel").is_none_or(|r| r == "alternate") => show.link = attr(e, "href").unwrap_or_default(),
        ("media:thumbnail", Some(i)) if i.image.is_none() => i.image = attr(e, "url"),
        ("itunes:image", None) => {
            if let Some(h) = attr(e, "href") {
                show.image = Some(h);
            }
        }
        _ => {}
    };
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" || name == "entry" {
                    item = Some(FeedEpisode::default());
                    long_notes.clear();
                }
                take_attrs(&name, &e, &mut show, &mut item);
                path.push(name);
                text.clear();
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                take_attrs(&name, &e, &mut show, &mut item);
            }
            Ok(Event::Text(t)) => text.push_str(&t.decode().map(|c| c.to_string()).unwrap_or_default()),
            Ok(Event::GeneralRef(r)) => text.push_str(&entity(&String::from_utf8_lossy(&r))),
            Ok(Event::CData(t)) => text.push_str(&String::from_utf8_lossy(&t)),
            Ok(Event::End(_)) => {
                let name = path.pop().unwrap_or_default();
                let parent = path.last().map(String::as_str).unwrap_or("");
                let value = text.trim().to_string();
                text.clear();
                if name == "item" || name == "entry" {
                    if let Some(mut i) = item.take() {
                        // (YouTube Shorts aren't episodes.)
                        if i.link.contains("/shorts/") {
                            continue;
                        }
                        if long_notes.len() > i.notes.len() {
                            i.notes = std::mem::take(&mut long_notes);
                        }
                        if i.guid.is_empty() {
                            i.guid = i.audio_url.clone();
                        }
                        if !i.audio_url.is_empty() {
                            show.episodes.push(i);
                        }
                    }
                    continue;
                }
                if let Some(i) = item.as_mut() {
                    if name == "media:description" && i.notes.is_empty() {
                        i.notes = value.replace('\n', "<br>");
                        continue;
                    }
                    if parent != "item" && parent != "entry" {
                        continue;
                    }
                    match name.as_str() {
                        "yt:videoId" => {
                            i.guid = value.clone();
                            i.audio_url = format!("https://www.youtube.com/watch?v={value}");
                        }
                        "published" | "updated" if i.published.is_none() => i.published = parse_date(&value),
                        "summary" | "content" if value.len() > i.notes.len() => i.notes = value,
                        "title" => i.title = strip_html(&value),
                        "guid" => i.guid = value,
                        "pubDate" => i.published = parse_date(&value),
                        "itunes:duration" => i.duration = parse_duration(&value),
                        "description" | "itunes:summary" if value.len() > i.notes.len() => i.notes = value,
                        "content:encoded" => long_notes = value,
                        _ => {}
                    }
                } else if parent == "feed" && name == "title" {
                    show.title = strip_html(&value);
                } else if parent == "author" && name == "name" && show.author.is_empty() {
                    show.author = strip_html(&value);
                } else if parent == "channel" {
                    match name.as_str() {
                        "title" => show.title = strip_html(&value),
                        "itunes:author" => show.author = strip_html(&value),
                        "link" if show.link.is_empty() => show.link = value,
                        "description" | "itunes:summary" if value.len() > show.description.len() => show.description = strip_html(&value),
                        _ => {}
                    }
                } else if parent == "image" && name == "url" && show.image.is_none() {
                    show.image = Some(value);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    // (A YouTube channel's feed has no picture of its own: its newest video's stands in.)
    if show.image.is_none() {
        show.image = show.episodes.iter().find_map(|e| e.image.clone());
    }
    show
}

pub fn is_youtube(url: &str) -> bool {
    url.contains("youtube.com/") || url.contains("youtu.be/")
}

async fn fetch_feed(state: &State, url: &str) -> anyhow::Result<FeedShow> {
    let xml = state
        .http
        .get(url)
        .timeout(Duration::from_secs(40))
        .send()
        .await
        .with_context(|| format!("couldn't reach {url}"))?
        .error_for_status()
        .context("the feed's site refused")?
        .text()
        .await?;
    let show = parse_feed(&xml);
    if show.title.is_empty() && show.episodes.is_empty() {
        bail!("That address isn't a podcast feed");
    }
    Ok(show)
}

/// A feed address from what was pasted: the feed itself, or an Apple Podcasts page.
async fn feed_address(state: &State, input: &str) -> anyhow::Result<String> {
    let input = input.trim();
    if input.contains("podcasts.apple.com") {
        let id = input
            .split(['/', '?', '&'])
            .find_map(|p| p.strip_prefix("id").filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
            .context("couldn't find the show's id in that Apple Podcasts link")?;
        let hits = lookup(state, &[id.to_string()]).await?;
        return hits.into_iter().next().map(|h| h.feed_url).context("Apple doesn't list a feed for that show");
    }
    // A YouTube channel (link or @handle) or playlist.
    if input.starts_with('@') || (is_youtube(input) && !input.contains("feeds/videos.xml")) {
        if let Some(list) = input.split(['?', '&']).find_map(|p| p.strip_prefix("list=")) {
            return Ok(format!("https://www.youtube.com/feeds/videos.xml?playlist_id={list}"));
        }
        anyhow::ensure!(!input.contains("watch?v="), "That's one video: paste the channel's or playlist's link");
        return crate::podcast::feeds::youtube_feed(&state.http, input).await;
    }
    if input.starts_with("http://") || input.starts_with("https://") {
        Ok(input.to_string())
    } else if input.contains('.') && !input.contains(' ') {
        Ok(format!("https://{input}"))
    } else {
        bail!("Paste the show's feed address, its Apple Podcasts link, or a YouTube channel or playlist")
    }
}

/// Adds the feed's new episodes and brings the show's details up to date. Returns the ids of
/// the episodes that are new.
async fn store_feed(state: &State, show_id: i64, feed: &FeedShow) -> anyhow::Result<Vec<i64>> {
    sqlx::query("UPDATE shows SET title = ?, author = ?, description = ?, image = COALESCE(?, image), link = ?, refreshed = ? WHERE id = ?")
        .bind(&feed.title)
        .bind(&feed.author)
        .bind(&feed.description)
        .bind(&feed.image)
        .bind(&feed.link)
        .bind(Utc::now().to_rfc3339())
        .bind(show_id)
        .execute(&state.db)
        .await?;
    let mut new = Vec::new();
    let mut tx = state.db.begin().await?;
    for e in &feed.episodes {
        let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM show_episodes WHERE show_id = ? AND guid = ?")
            .bind(show_id)
            .bind(&e.guid)
            .fetch_optional(&mut *tx)
            .await?;
        let published = e.published.map(|d| d.to_rfc3339());
        match existing {
            Some(id) => {
                sqlx::query(
                    "UPDATE show_episodes SET title = ?, notes = ?, audio_url = ?, image = ?, published = COALESCE(?, published),
                     duration = CASE WHEN ? > 0 THEN ? ELSE duration END WHERE id = ?",
                )
                .bind(&e.title)
                .bind(&e.notes)
                .bind(&e.audio_url)
                .bind(&e.image)
                .bind(&published)
                .bind(e.duration)
                .bind(e.duration)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            None => {
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO show_episodes (show_id, guid, title, notes, published, duration, audio_url, image) VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
                )
                .bind(show_id)
                .bind(&e.guid)
                .bind(&e.title)
                .bind(&e.notes)
                .bind(&published)
                .bind(e.duration)
                .bind(&e.audio_url)
                .bind(&e.image)
                .fetch_one(&mut *tx)
                .await?;
                new.push(id);
            }
        }
    }
    tx.commit().await?;
    Ok(new)
}

/// Follows a show (or returns it if it's already followed).
async fn follow_feed(state: &State, input: &str) -> anyhow::Result<Show> {
    let url = feed_address(state, input).await?;
    if let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM shows WHERE feed_url = ?").bind(&url).fetch_optional(&state.db).await? {
        return show(state, id).await?.context("show vanished");
    }
    let feed = fetch_feed(state, &url).await?;
    let id: i64 = sqlx::query_scalar("INSERT INTO shows (feed_url, title, kind) VALUES (?, ?, ?) RETURNING id")
        .bind(&url)
        .bind(&feed.title)
        .bind(if is_youtube(&url) { "youtube" } else { "rss" })
        .fetch_one(&state.db)
        .await?;
    store_feed(state, id, &feed).await?;
    // The newest episode, ready ad-free by the time it's played.
    if settings(state).await.prepare_new {
        if let Some(newest) = newest_episodes(state, id, 1).await?.first() {
            queue(state, *newest, false);
        }
    }
    show(state, id).await?.context("show vanished")
}

async fn newest_episodes(state: &State, show_id: i64, n: i64) -> anyhow::Result<Vec<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM show_episodes WHERE show_id = ? AND finished = 0 ORDER BY published DESC LIMIT ?")
        .bind(show_id)
        .bind(n)
        .fetch_all(&state.db)
        .await?)
}

/// Checks every show for new episodes; new ones are downloaded and checked for ads (when that's
/// on), so they're ready when you get to them.
async fn refresh(state: &State) -> anyhow::Result<usize> {
    let shows: Vec<(i64, String, bool)> = sqlx::query_as("SELECT id, feed_url, skip_ads FROM shows").fetch_all(&state.db).await?;
    let prefs = settings(state).await;
    let found = futures_util::stream::iter(shows)
        .map(|(id, url, skip)| async move {
            match fetch_feed(state, &url).await {
                Ok(feed) => match store_feed(state, id, &feed).await {
                    Ok(new) => (new, skip),
                    Err(e) => {
                        tracing::warn!("storing {url}: {e:#}");
                        (Vec::new(), skip)
                    }
                },
                Err(e) => {
                    tracing::warn!("refreshing {url}: {e:#}");
                    (Vec::new(), skip)
                }
            }
        })
        .buffer_unordered(6)
        .collect::<Vec<_>>()
        .await;
    let mut count = 0;
    for (new, skip) in found {
        count += new.len();
        if prefs.prepare_new && prefs.skip_ads && skip {
            // Only the newest few of a show that came back with many (a first fetch of a
            // back catalogue isn't "new").
            for id in new.into_iter().take(2) {
                queue(state, id, false);
            }
        }
    }
    Ok(count)
}

/// Every hour: new episodes, and old downloads cleared away.
pub async fn hourly(state: State) {
    tokio::spawn(scanner(state.clone()));
    reread_old(&state).await;
    // (Not in the first moments after starting, while everything else gets going.)
    tokio::time::sleep(Duration::from_secs(90)).await;
    loop {
        if let Err(e) = refresh(&state).await {
            tracing::warn!("refreshing podcasts: {e:#}");
        }
        if let Err(e) = tidy(&state).await {
            tracing::warn!("tidying podcast downloads: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// Episodes whose ads were found an older way are read again from their transcripts (no
/// listening), most recently played first.
async fn reread_old(state: &State) {
    let Ok(mut dir) = tokio::fs::read_dir(base(state).join("episodes")).await else { return };
    let mut old = Vec::new();
    while let Ok(Some(e)) = dir.next_entry().await {
        let Some(id) = e.file_name().to_str().and_then(|n| n.parse::<i64>().ok()) else { continue };
        let scan: AdScan = read_json(&e.path().join("scan.json")).await.unwrap_or_default();
        if scan.state == "done" && scan.version < CUTS && e.path().join("transcript.json").exists() {
            let played = e.metadata().await.and_then(|m| m.modified()).ok();
            old.push((played, id));
        }
    }
    old.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, id) in old {
        tracing::info!("episode {id}: reading its ads again (they were found an older way)");
        set_scan(state, id, |x| *x = AdScan::default()).await;
        queue(state, id, false);
    }
}

/// Deletes the downloads of episodes finished more than `keep_days` ago.
async fn tidy(state: &State) -> anyhow::Result<()> {
    let days = settings(state).await.keep_days;
    if days == 0 {
        return Ok(());
    }
    let cutoff = (Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();
    let old: Vec<i64> = sqlx::query_scalar("SELECT id FROM show_episodes WHERE finished = 1 AND played_at < ?")
        .bind(cutoff)
        .fetch_all(&state.db)
        .await?;
    for id in old {
        let dir = episode_dir(state, id);
        if dir.exists() && !SCANS.lock().unwrap().get(&id).is_some_and(AdScan::busy) {
            let _ = tokio::fs::remove_dir_all(dir).await;
        }
    }
    Ok(())
}

/// Show notes as plain text (scripts and styles dropped whole, not just their tags).
fn plain(html: &str) -> String {
    let mut out = html.to_string();
    for tag in ["script", "style"] {
        while let Some(a) = out.to_ascii_lowercase().find(&format!("<{tag}")) {
            let lower = out.to_ascii_lowercase();
            let end = lower[a..].find(&format!("</{tag}>")).map(|e| a + e + tag.len() + 3).unwrap_or(out.len());
            out.replace_range(a..end, " ");
        }
    }
    strip_html(&out)
}

// ---- Reading the database ----

const SHOW_COLUMNS: &str = "s.id, s.feed_url, s.title, s.author, s.description, s.image, s.link, s.skip_ads, s.kind,
    (SELECT COUNT(*) FROM show_episodes e WHERE e.show_id = s.id AND e.finished = 0) AS unplayed,
    (SELECT MAX(published) FROM show_episodes e WHERE e.show_id = s.id) AS latest,
    (SELECT COUNT(*) FROM show_episodes e WHERE e.show_id = s.id) AS episodes";

fn show_from(r: &sqlx::sqlite::SqliteRow) -> Show {
    Show {
        id: r.get("id"),
        feed_url: r.get("feed_url"),
        title: r.get("title"),
        author: r.get("author"),
        description: r.get("description"),
        image: r.get("image"),
        link: r.get("link"),
        kind: r.get("kind"),
        skip_ads: r.get::<i64, _>("skip_ads") != 0,
        unplayed: r.get("unplayed"),
        latest: r.get("latest"),
        episodes: r.get("episodes"),
    }
}

async fn show(state: &State, id: i64) -> anyhow::Result<Option<Show>> {
    Ok(sqlx::query(&format!("SELECT {SHOW_COLUMNS} FROM shows s WHERE s.id = ?")).bind(id).fetch_optional(&state.db).await?.as_ref().map(show_from))
}

async fn shows(state: &State) -> anyhow::Result<Vec<Show>> {
    let rows = sqlx::query(&format!("SELECT {SHOW_COLUMNS} FROM shows s ORDER BY latest DESC")).fetch_all(&state.db).await?;
    Ok(rows.iter().map(show_from).collect())
}

const EPISODE_COLUMNS: &str = "e.id, e.show_id, s.title AS show_title, s.skip_ads AS show_skip_ads, e.title, e.notes, e.published, e.duration, e.audio_url,
    COALESCE(e.image, s.image) AS image, e.position, e.finished";

fn episode_from(state: &State, r: &sqlx::sqlite::SqliteRow) -> ShowEpisode {
    let id: i64 = r.get("id");
    let notes: String = r.get("notes");
    ShowEpisode {
        id,
        show_id: r.get("show_id"),
        show_title: r.get("show_title"),
        show_skip_ads: r.get::<i64, _>("show_skip_ads") != 0,
        title: r.get("title"),
        description: clip(&plain(&notes), 400),
        notes_html: String::new(),
        published: r.get("published"),
        duration: r.get("duration"),
        image: r.get("image"),
        needs_download: is_youtube(&r.get::<String, _>("audio_url")),
        audio_url: r.get("audio_url"),
        position: r.get("position"),
        finished: r.get::<i64, _>("finished") != 0,
        downloaded: audio_file(state, id).is_some(),
        ads: SCANS.lock().unwrap().get(&id).cloned().unwrap_or_default(),
    }
}

/// An episode with everything: its show notes, and its ad scan from disk when it isn't live.
async fn full_episode(state: &State, id: i64) -> anyhow::Result<Option<ShowEpisode>> {
    let Some(r) = sqlx::query(&format!("SELECT {EPISODE_COLUMNS} FROM show_episodes e JOIN shows s ON s.id = e.show_id WHERE e.id = ?"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
    else {
        return Ok(None);
    };
    let mut e = episode_from(state, &r);
    e.notes_html = r.get("notes");
    e.ads = scan_of(state, id).await;
    Ok(Some(e))
}

async fn episodes_where(state: &State, clause: &str, bind: Option<i64>, limit: i64) -> anyhow::Result<Vec<ShowEpisode>> {
    let sql = format!("SELECT {EPISODE_COLUMNS} FROM show_episodes e JOIN shows s ON s.id = e.show_id WHERE {clause} LIMIT {limit}");
    let mut q = sqlx::query(&sql);
    if let Some(b) = bind {
        q = q.bind(b);
    }
    let rows = q.fetch_all(&state.db).await?;
    let mut out: Vec<ShowEpisode> = rows.iter().map(|r| episode_from(state, r)).collect();
    for e in &mut out {
        if e.ads.state.is_empty() && e.downloaded {
            e.ads = scan_of(state, e.id).await;
        }
    }
    Ok(out)
}

// ---- Downloading and finding the ads ----

static SCANS: LazyLock<Mutex<HashMap<i64, AdScan>>> = LazyLock::new(Default::default);
static QUEUE: LazyLock<Mutex<VecDeque<i64>>> = LazyLock::new(Default::default);
static WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);
/// An episode someone's playing: the scanner turns to it before carrying on with the others.
static WANTED: AtomicI64 = AtomicI64::new(0);
static DOWNLOADS: LazyLock<tokio::sync::Mutex<HashSet<i64>>> = LazyLock::new(Default::default);

/// The downloaded recording, if there is one.
fn audio_file(state: &State, id: i64) -> Option<PathBuf> {
    let dir = episode_dir(state, id);
    std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_stem().is_some_and(|s| s == "audio") && p.extension().is_some_and(|x| x != "part"))
}

async fn scan_of(state: &State, id: i64) -> AdScan {
    if let Some(s) = SCANS.lock().unwrap().get(&id) {
        return s.clone();
    }
    read_json(&episode_dir(state, id).join("scan.json")).await.unwrap_or_default()
}

async fn set_scan(state: &State, id: i64, f: impl FnOnce(&mut AdScan)) -> AdScan {
    let mut s = scan_of(state, id).await;
    f(&mut s);
    SCANS.lock().unwrap().insert(id, s.clone());
    if !matches!(s.state.as_str(), "" | "queued") || episode_dir(state, id).exists() {
        let _ = write_json(&episode_dir(state, id).join("scan.json"), &s).await;
    }
    s
}

/// Puts an episode in line to be downloaded and checked; `now` (it's being played) puts it first.
fn queue(state: &State, id: i64, now: bool) {
    let mut q = QUEUE.lock().unwrap();
    if now {
        WANTED.store(id, Ordering::SeqCst);
        q.retain(|x| *x != id);
        q.push_front(id);
    } else if !q.contains(&id) {
        q.push_back(id);
    }
    drop(q);
    let state = state.clone();
    tokio::spawn(async move {
        set_scan(&state, id, |s| {
            if !s.busy() && s.state != "done" {
                s.state = "queued".into();
                s.doing = "Waiting its turn".into();
                s.error = None;
            }
        })
        .await;
        WAKE.notify_one();
    });
}

/// Works through the queue, one episode at a time (the graphics card does one thing at a time).
async fn scanner(state: State) {
    loop {
        let next = QUEUE.lock().unwrap().pop_front();
        let Some(id) = next else {
            WAKE.notified().await;
            continue;
        };
        match scan(&state, id).await {
            // The whole transcript is in: its outline next (on the same model, so in turn).
            Ok(true) => {
                let o = outline_of(&state, id).await;
                if (o.state != "done" || o.version < OUTLINE_VERSION) && episode_dir(&state, id).join("transcript.json").exists() {
                    let _ = make_outline(&state, id).await;
                }
            }
            // Set aside for one being played: back in line after it.
            Ok(false) => QUEUE.lock().unwrap().push_back(id),
            Err(e) => {
                tracing::warn!("finding ads in episode {id}: {e:#}");
                set_scan(&state, id, |s| {
                    s.state = "failed".into();
                    s.doing.clear();
                    s.error = Some(format!("{e:#}"));
                })
                .await;
            }
        }
        if WANTED.load(Ordering::SeqCst) == id {
            WANTED.store(0, Ordering::SeqCst);
        }
    }
}

/// Downloads the episode (once, however many ask).
async fn download(state: &State, id: i64) -> anyhow::Result<PathBuf> {
    if let Some(p) = audio_file(state, id) {
        return Ok(p);
    }
    // (One download per episode: a second caller waits for the first.)
    loop {
        if DOWNLOADS.lock().await.insert(id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Some(p) = audio_file(state, id) {
            return Ok(p);
        }
    }
    let r = download_inner(state, id).await;
    DOWNLOADS.lock().await.remove(&id);
    r
}

async fn download_inner(state: &State, id: i64) -> anyhow::Result<PathBuf> {
    if let Some(p) = audio_file(state, id) {
        return Ok(p);
    }
    let url: String = sqlx::query_scalar("SELECT audio_url FROM show_episodes WHERE id = ?").bind(id).fetch_one(&state.db).await?;
    if is_youtube(&url) {
        return youtube_audio(state, id, &url).await;
    }
    let ext = url
        .split(['?', '#'])
        .next()
        .and_then(|p| p.rsplit('/').next())
        .and_then(|f| f.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase()))
        .filter(|x| ["mp3", "m4a", "aac", "ogg", "oga", "opus", "mp4", "wav", "flac"].contains(&x.as_str()))
        .unwrap_or_else(|| "mp3".into());
    let dir = episode_dir(state, id);
    tokio::fs::create_dir_all(&dir).await?;
    let part = dir.join("audio.part");
    let resp = state
        .http
        .get(&url)
        .timeout(Duration::from_secs(1800))
        .send()
        .await
        .context("couldn't reach the show's server")?
        .error_for_status()
        .context("the show's server refused the download")?;
    let total = resp.content_length().unwrap_or(0) as f64;
    let mut file = tokio::fs::File::create(&part).await?;
    let mut got = 0f64;
    let mut last = std::time::Instant::now();
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.context("the download was cut off")?;
        got += chunk.len() as f64;
        file.write_all(&chunk).await?;
        if last.elapsed() > Duration::from_millis(500) {
            last = std::time::Instant::now();
            let p = if total > 0.0 { got / total } else { 0.0 };
            set_scan(state, id, |s| {
                s.state = "downloading".into();
                s.progress = p;
                s.doing = if total > 0.0 { format!("Downloading {:.0}%", p * 100.0) } else { format!("Downloading {:.0} MB", got / 1e6) };
            })
            .await;
        }
    }
    file.flush().await?;
    drop(file);
    let path = dir.join(format!("audio.{ext}"));
    tokio::fs::rename(&part, &path).await?;
    Ok(path)
}

/// A YouTube video's sound, pulled out with yt-dlp (no re-encoding: the audio stream as it is).
async fn youtube_audio(state: &State, id: i64, url: &str) -> anyhow::Result<PathBuf> {
    use tokio::io::AsyncBufReadExt;
    let dir = episode_dir(state, id);
    tokio::fs::create_dir_all(&dir).await?;
    let mut child = tokio::process::Command::new("yt-dlp")
        .args(["-f", "bestaudio[ext=m4a]/bestaudio", "--no-playlist", "--newline", "--no-part", "-o"])
        .arg(dir.join("yt.%(ext)s"))
        .arg(url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("yt-dlp isn't installed (it's needed for YouTube shows)")?;
    let mut lines = tokio::io::BufReader::new(child.stdout.take().context("yt-dlp output")?).lines();
    let mut last = std::time::Instant::now();
    while let Some(line) = lines.next_line().await? {
        // "[download]  42.3% of   31.20MiB at ..."
        let pct = line.strip_prefix("[download]").and_then(|r| r.trim().split('%').next()).and_then(|p| p.trim().parse::<f64>().ok());
        if let Some(p) = pct.filter(|_| last.elapsed() > Duration::from_millis(500)) {
            last = std::time::Instant::now();
            set_scan(state, id, |s| {
                s.state = "downloading".into();
                s.progress = p / 100.0;
                s.doing = format!("Getting the sound from YouTube {p:.0}%");
            })
            .await;
        }
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!("yt-dlp: {}", String::from_utf8_lossy(&out.stderr).lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("failed"));
    }
    let got = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_stem().is_some_and(|s| s == "yt"))
        .context("yt-dlp finished without a file")?;
    let ext = got.extension().and_then(|x| x.to_str()).unwrap_or("m4a").to_string();
    let path = dir.join(format!("audio.{ext}"));
    tokio::fs::rename(&got, &path).await?;
    Ok(path)
}

/// The sponsor segments SponsorBlock's community has marked in a YouTube video (none if it
/// has none, or isn't reachable).
async fn sponsorblock(state: &State, url: &str, promos: bool) -> Vec<AdSpan> {
    let Some(video) = url.split(['?', '&']).find_map(|p| p.strip_prefix("v=")).or_else(|| url.split("youtu.be/").nth(1).map(|r| r.split(['?', '&']).next().unwrap_or(r))) else {
        return Vec::new();
    };
    let cats = if promos { r#"["sponsor","selfpromo","interaction"]"# } else { r#"["sponsor"]"# };
    let Ok(resp) = state
        .http
        .get("https://sponsor.ajay.app/api/skipSegments")
        .query(&[("videoID", video), ("categories", cats)])
        .timeout(Duration::from_secs(15))
        .send()
        .await
    else {
        return Vec::new();
    };
    #[derive(Deserialize)]
    struct Seg {
        segment: (f64, f64),
        category: String,
    }
    let segs: Vec<Seg> = if resp.status().is_success() { resp.json().await.unwrap_or_default() } else { Vec::new() };
    let mut ads = Vec::new();
    for s in segs {
        let label = match s.category.as_str() {
            "sponsor" => "Sponsor (SponsorBlock)",
            "selfpromo" => "Self-promotion (SponsorBlock)",
            _ => "Like and subscribe (SponsorBlock)",
        };
        add_span(&mut ads, AdSpan { start: s.segment.0, end: s.segment.1, label: label.into() });
    }
    ads
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Heard {
    start: f64,
    end: f64,
    text: String,
    /// Each word's (start, end, word), for lighting the transcript up as it plays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    words: Vec<(f64, f64, String)>,
    /// Whose voice it is (the same number for the same voice through the episode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speaker: Option<u32>,
}

fn mmss(t: f64) -> String {
    let t = t.max(0.0) as u64;
    if t >= 3600 { format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60) } else { format!("{}:{:02}", t / 60, t % 60) }
}

/// How long a recording runs, from ffprobe (when the feed didn't say).
async fn probe_duration(path: &std::path::Path) -> Option<f64> {
    let out = tokio::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Downloads an episode and finds its ads, a stretch at a time. Returns false when it was set
/// aside for an episode someone's playing (it picks up where it left off later).
async fn scan(state: &State, id: i64) -> anyhow::Result<bool> {
    let current = scan_of(state, id).await;
    if current.state == "done" {
        return Ok(true);
    }
    set_scan(state, id, |s| {
        s.state = "downloading".into();
        s.doing = "Downloading".into();
        s.progress = 0.0;
        s.error = None;
    })
    .await;
    let path = download(state, id).await?;
    let mut duration: f64 = sqlx::query_scalar("SELECT duration FROM show_episodes WHERE id = ?").bind(id).fetch_one(&state.db).await?;
    // (Feeds' durations are often a little off, or missing; the file's own is what's played.)
    if let Some(d) = probe_duration(&path).await {
        duration = d;
        sqlx::query("UPDATE show_episodes SET duration = ? WHERE id = ?").bind(d).bind(id).execute(&state.db).await?;
    }
    let transcript_path = episode_dir(state, id).join("transcript.json");
    let mut heard: Vec<Heard> = read_json(&transcript_path).await.unwrap_or_default();
    let (show_title, title, notes, skip_promos) = {
        let r = sqlx::query("SELECT s.title AS show_title, e.title, e.notes FROM show_episodes e JOIN shows s ON s.id = e.show_id WHERE e.id = ?")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
        (r.get::<String, _>("show_title"), r.get::<String, _>("title"), clip(&plain(&r.get::<String, _>("notes")), 1500), settings(state).await.skip_promos)
    };
    // A YouTube video SponsorBlock's community has already marked: no need to listen.
    let audio_url: String = sqlx::query_scalar("SELECT audio_url FROM show_episodes WHERE id = ?").bind(id).fetch_one(&state.db).await?;
    // A YouTube video SponsorBlock's community has already marked: its ads are known, so the
    // listening only writes the transcript (for reading along and the outline).
    let mut marked = false;
    if is_youtube(&audio_url) && settings(state).await.sponsorblock && current.checked == 0.0 {
        let found = sponsorblock(state, &audio_url, skip_promos).await;
        if !found.is_empty() {
            marked = true;
            set_scan(state, id, |s| {
                s.ads = found;
                s.checked = duration;
                s.duration = duration;
            })
            .await;
        }
    }
    marked |= is_youtube(&audio_url) && current.checked >= duration - 1.0 && !current.ads.is_empty();
    let tts = state.settings.read().await.ai.tts_url.clone();
    // How far the transcript (and, unless SponsorBlock marked them, the ads) has got.
    let mut checked = if marked { heard.last().map(|h| h.end).unwrap_or(0.0) } else { scan_of(state, id).await.checked };
    // The model reads one stretch while Whisper writes down the next: the stretch being read,
    // and where it ends.
    let mut reading: Option<(tokio::task::JoinHandle<anyhow::Result<Vec<AdSpan>>>, f64)> = None;
    // Puts what the model found in, and moves "checked" past it.
    let take = |reading: Option<(tokio::task::JoinHandle<anyhow::Result<Vec<AdSpan>>>, f64)>| async move {
        let Some((task, upto)) = reading else { return anyhow::Ok(()) };
        let found = task.await.context("reading the transcript")??;
        set_scan(state, id, |s| {
            for a in found {
                add_span(&mut s.ads, a);
            }
            s.checked = upto;
            s.duration = duration;
        })
        .await;
        Ok(())
    };
    loop {
        if checked >= duration - 1.0 {
            break;
        }
        let wanted = WANTED.load(Ordering::SeqCst);
        if wanted != 0 && wanted != id {
            take(reading.take()).await?;
            set_scan(state, id, |s| {
                s.state = "queued".into();
                s.doing = "Paused for the episode you're playing".into();
            })
            .await;
            return Ok(false);
        }
        let len = if checked == 0.0 { FIRST_WINDOW } else { WINDOW };
        set_scan(state, id, |s| {
            s.state = "listening".into();
            s.duration = duration;
            s.progress = checked / duration.max(1.0);
            s.doing = if marked {
                format!("Writing the transcript: {} of {}", mmss(checked), mmss(duration))
            } else {
                format!("Listening for ads: {} of {}", mmss(checked), mmss(duration))
            };
        })
        .await;
        #[derive(Deserialize)]
        struct Reply {
            segments: Vec<Heard>,
            end: f64,
        }
        let started = std::time::Instant::now();
        // Already written down (the ads are being read again): no need to listen.
        let want_to = (checked + len).min(duration);
        if heard.last().is_some_and(|h| h.end >= want_to - 15.0) && want_to < duration - 1.0 || heard.last().is_some_and(|h| h.end >= duration - 15.0) {
            let end = if want_to >= duration - 1.0 { duration } else { want_to };
            let new_checked = end;
            if !marked {
                take(reading.take()).await?;
                let from = (checked - OVERLAP).max(0.0);
                let lines: Vec<Heard> = heard.iter().filter(|h| h.end > from && h.start < end).cloned().collect();
                let (st, show_title, title, notes, audio) = (state.clone(), show_title.clone(), title.clone(), notes.clone(), path.clone());
                reading = Some((
                    tokio::spawn(async move {
                        let refs: Vec<&Heard> = lines.iter().collect();
                        find_ads(&st, &audio, &show_title, &title, &notes, &refs, skip_promos).await
                    }),
                    new_checked,
                ));
            }
            checked = new_checked;
            continue;
        }
        let resp = state
            .http
            .post(format!("{}/transcribe-window", tts.trim_end_matches('/')))
            .json(&json!({ "audio": path, "start": checked, "duration": len, "words": true }))
            .timeout(Duration::from_secs(900))
            .send()
            .await
            .context("the voice server isn't running (Settings → Voices); it does the listening")?;
        if !resp.status().is_success() {
            let v: serde_json::Value = resp.json().await.unwrap_or_default();
            bail!("listening: {}", v["error"].as_str().unwrap_or("the voice server failed (it may need restarting for podcasts)"));
        }
        let reply: Reply = resp.json().await?;
        let end = if reply.end > checked { reply.end } else { checked + len };
        tracing::info!("episode {id}: transcribed {}–{} in {:.0}s", mmss(checked), mmss(end), started.elapsed().as_secs_f64());
        heard.retain(|h| h.start < checked);
        heard.extend(reply.segments);
        write_json(&transcript_path, &heard).await?;
        let new_checked = end.min(duration);
        if !marked {
            // The stretch before is read by now (or soon): in, before this one goes to the model.
            take(reading.take()).await?;
            // This stretch, with the end of the last for context.
            let from = (checked - OVERLAP).max(0.0);
            let lines: Vec<Heard> = heard.iter().filter(|h| h.end > from && h.start < end).cloned().collect();
            let (st, show_title, title, notes, audio) = (state.clone(), show_title.clone(), title.clone(), notes.clone(), path.clone());
            let task = tokio::spawn(async move {
                let started = std::time::Instant::now();
                let refs: Vec<&Heard> = lines.iter().collect();
                let found = find_ads(&st, &audio, &show_title, &title, &notes, &refs, skip_promos).await;
                tracing::info!("episode {id}: read {} lines for ads in {:.0}s", refs.len(), started.elapsed().as_secs_f64());
                found
            });
            reading = Some((task, new_checked));
        }
        checked = new_checked;
    }
    take(reading.take()).await?;
    set_scan(state, id, |s| {
        s.version = CUTS;
        s.state = "done".into();
        s.doing.clear();
        s.progress = 1.0;
        s.checked = duration;
        s.duration = duration;
    })
    .await;
    SCANS.lock().unwrap().remove(&id);
    Ok(true)
}

/// Adds an ad, joined with any it overlaps or nearly touches.
fn add_span(ads: &mut Vec<AdSpan>, mut a: AdSpan) {
    let mut keep = Vec::new();
    for b in ads.drain(..) {
        if b.start <= a.end + 3.0 && a.start <= b.end + 3.0 {
            if b.end - b.start > a.end - a.start && !b.label.is_empty() {
                a.label = b.label.clone();
            }
            a.start = a.start.min(b.start);
            a.end = a.end.max(b.end);
        } else {
            keep.push(b);
        }
    }
    keep.push(a);
    keep.sort_by(|x, y| x.start.total_cmp(&y.start));
    *ads = keep;
}

/// The model reads a stretch of transcript and says which lines are ads.
async fn find_ads(state: &State, audio: &std::path::Path, show: &str, title: &str, notes: &str, lines: &[&Heard], promos: bool) -> anyhow::Result<Vec<AdSpan>> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let promo_rule = if promos {
        "promos for other podcasts or shows, and the show's own pitches for memberships, Patreon, merch, newsletters or donations"
    } else {
        "but NOT promos for other shows or the show's own membership/Patreon/donation pitches"
    };
    let system = format!(
        "You find the ads in podcast transcripts so a podcast player can skip them. You get numbered transcript lines, \
         each with its time. Mark: sponsor reads by the hosts (\"this episode is brought to you by\", promo codes, \
         \"go to example.com/show\", \"use code\"), commercials inserted into the show, {promo_rule}. \
         Do not mark the show's normal intro, theme or outro, listener mail, or content that is genuinely part of the \
         episode, even when it talks about products or companies. An ad starts at the line where the pitch begins \
         (include a lead-in like \"let's take a quick break\" or \"a word from our sponsors\") and ends at its last line, \
         before the show picks up again. Several ads back to back can be one entry. The transcript is speech recognition \
         so expect small errors, and one line can run the end of an ad into the show (\"...call 855-EXAMPLE. Hello again, \
         it's me...\"): when the ad starts partway through its first line, add \"starts_with\": the ad's first few words \
         exactly as written in that line; when it ends partway through its last line, add \"ends_with\": the ad's last few \
         words exactly as written there. Reply with only JSON, no prose: an array of {{\"from\": first line number, \"to\": \
         last line number, \"label\": a few words saying who or what it's for}} (plus starts_with / ends_with when \
         needed), or [] if there are no ads."
    );
    let mut text = format!("Show: {show}\nEpisode: {title}\n");
    if !notes.is_empty() {
        // (Show notes often list the sponsors.)
        text.push_str(&format!("Show notes: {notes}\n"));
    }
    text.push_str("\nTranscript:\n");
    for (i, h) in lines.iter().enumerate() {
        text.push_str(&format!("[{i}] {} {}\n", mmss(h.start), h.text));
    }
    let turns = vec![("user".to_string(), text)];
    let reply = if settings(state).await.ad_model == "local" {
        crate::llm::chat_local(state, &system, &turns).await?
    } else {
        crate::llm::chat(state, &system, &turns).await?
    };
    #[derive(Deserialize)]
    struct Found {
        from: serde_json::Value,
        to: serde_json::Value,
        #[serde(default)]
        label: String,
        #[serde(default)]
        starts_with: String,
        #[serde(default)]
        ends_with: String,
    }
    let json = match (reply.find('['), reply.rfind(']')) {
        (Some(a), Some(b)) if b > a => &reply[a..=b],
        _ => "[]",
    };
    let found: Vec<Found> = serde_json::from_str(json).with_context(|| format!("the model's answer wasn't a list of ads: {}", clip(&reply, 200)))?;
    let num = |v: &serde_json::Value| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim_matches(['[', ']']).parse().ok())).map(|n| n as usize);
    let mut out = Vec::new();
    for f in found {
        let (Some(a), Some(b)) = (num(&f.from), num(&f.to)) else { continue };
        let (a, b) = (a.min(b), a.max(b));
        let (Some(first), Some(last)) = (lines.get(a), lines.get(b.min(lines.len().saturating_sub(1)))) else { continue };
        // Where in a line an ad starts or ends, when the line runs the ad and the show together.
        let start = if f.starts_with.trim().is_empty() { first.start } else { cut_point(state, audio, first, &f.starts_with, false).await };
        let end = if f.ends_with.trim().is_empty() { last.end } else { cut_point(state, audio, last, &f.ends_with, true).await };
        // (Very short "ads" are usually a mention, not a break: left alone.)
        if end - start >= 8.0 {
            out.push(AdSpan { start, end, label: f.label.trim().to_string() });
        }
    }
    Ok(out)
}

/// Lowercase words without punctuation, for matching what the model quoted against what Whisper heard.
fn plain_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Where `words` (whole) best matches as a run in `hay`: the index of its first word.
fn find_run(hay: &[String], words: &[String], near: Option<usize>) -> Option<usize> {
    if words.is_empty() || words.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - words.len()).filter(|&i| hay[i..i + words.len()] == *words).min_by_key(|&i| near.map(|n| i.abs_diff(n)).unwrap_or(0))
}

/// The time in `line` where the ad starts (the first of `quote`'s words) or ends (`at_end`: just
/// after the last of them). Whisper listens to the line again for its words' times; failing that,
/// the line's time is shared out by the words' lengths.
async fn cut_point(state: &State, audio: &std::path::Path, line: &Heard, quote: &str, at_end: bool) -> f64 {
    let fallback = if at_end { line.end } else { line.start };
    let said = plain_words(&line.text);
    let quoted = plain_words(quote);
    // (The model may misquote a word: try fewer words, from the edge that matters.)
    let pick = |n: usize| -> Vec<String> {
        if at_end { quoted[quoted.len().saturating_sub(n)..].to_vec() } else { quoted[..n.min(quoted.len())].to_vec() }
    };
    let Some((k, n)) = (1..=quoted.len().min(4)).rev().find_map(|n| find_run(&said, &pick(n), None).map(|k| (k, n))) else {
        return fallback;
    };
    // The word the cut is at: the last of the quote (at the end) or the first.
    let w = if at_end { k + n - 1 } else { k };
    let weight: Vec<f64> = said.iter().map(|x| x.chars().count() as f64 + 1.0).collect();
    let total: f64 = weight.iter().sum();
    let before: f64 = weight[..w + usize::from(at_end)].iter().sum();
    let estimate = line.start + (line.end - line.start) * before / total.max(1.0);
    // Whisper's own word times for just this line.
    let tts = state.settings.read().await.ai.tts_url.clone();
    let from = (line.start - 0.5).max(0.0);
    let resp = state
        .http
        .post(format!("{}/transcribe-window", tts.trim_end_matches('/')))
        .json(&json!({ "audio": audio, "start": from, "duration": line.end - from + 1.0, "words": true }))
        .timeout(Duration::from_secs(120))
        .send()
        .await;
    #[derive(Deserialize)]
    struct Reply {
        segments: Vec<Heard>,
    }
    let timed: Vec<(f64, f64, String)> = match resp {
        // (Only the line's own words: the stretch listened to starts a little before it.)
        Ok(r) if r.status().is_success() => r
            .json::<Reply>()
            .await
            .map(|r| r.segments.into_iter().flat_map(|s| s.words).filter(|(a, b, _)| *b > line.start + 0.1 && *a < line.end + 0.3).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let heard: Vec<String> = timed.iter().map(|(_, _, w)| plain_words(w).join("")).collect();
    let target = pick(n);
    // (Near where the estimate puts it, if the words come up more than once.)
    let near = timed.iter().position(|(a, _, _)| *a >= estimate);
    match find_run(&heard, &target, near) {
        Some(i) if at_end => timed[i + target.len() - 1].1 + 0.15,
        Some(i) => (timed[i].0 - 0.1).max(0.0),
        None => estimate,
    }
    .clamp(line.start, line.end)
}

// ---- Outlines ----

/// How outlines are made now: 2 names who's talking, 3 splits lines where the voice changes,
/// 4 times every word first.
/// Older ones are made again when opened.
const OUTLINE_VERSION: u32 = 4;

/// Outlines being made right now.
static OUTLINING: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(Default::default);

async fn outline_of(state: &State, id: i64) -> Outline {
    if OUTLINING.lock().unwrap().contains(&id) {
        return Outline { state: "making".into(), ..Default::default() };
    }
    read_json(&episode_dir(state, id).join("outline.json")).await.unwrap_or_default()
}

/// Has the model read the whole transcript and write a summary and a table of contents
/// (sections, and subsections in the longer ones), saved beside the transcript.
async fn make_outline(state: &State, id: i64) -> anyhow::Result<Outline> {
    if !OUTLINING.lock().unwrap().insert(id) {
        bail!("It's being made right now");
    }
    let made = outline_inner(state, id).await;
    OUTLINING.lock().unwrap().remove(&id);
    let o = match made {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("outlining episode {id}: {e:#}");
            Outline { state: "failed".into(), error: Some(format!("{e:#}")), ..Default::default() }
        }
    };
    write_json(&episode_dir(state, id).join("outline.json"), &o).await?;
    Ok(o)
}

/// The forced aligner's times for each line's words (the voice server's /align-lines).
async fn time_lines(state: &State, audio: &std::path::Path, heard: &[Heard]) -> anyhow::Result<Vec<Heard>> {
    let tts = state.settings.read().await.ai.tts_url.clone();
    let segments: Vec<serde_json::Value> = heard.iter().map(|h| json!({ "start": h.start, "end": h.end, "text": h.text })).collect();
    let started = std::time::Instant::now();
    let resp = state
        .http
        .post(format!("{}/align-lines", tts.trim_end_matches('/')))
        .json(&json!({ "audio": audio, "segments": segments }))
        .timeout(Duration::from_secs(1800))
        .send()
        .await
        .context("the voice server isn't running")?;
    if !resp.status().is_success() {
        bail!("the voice server couldn't time the words (it may need restarting)");
    }
    #[derive(Deserialize)]
    struct Reply {
        segments: Vec<Heard>,
    }
    let r: Reply = resp.json().await?;
    tracing::info!("timed the words of {} lines in {:.0}s", r.segments.len(), started.elapsed().as_secs_f64());
    Ok(r.segments)
}

/// Each line's voice, from the voice server: numbers, the same for the same voice.
/// Also where the voice changes inside a line: (time, new voice) for each line.
async fn tell_voices(state: &State, audio: &std::path::Path, heard: &[Heard]) -> anyhow::Result<(Vec<Option<u32>>, Vec<Vec<(f64, u32)>>)> {
    let tts = state.settings.read().await.ai.tts_url.clone();
    let segments: Vec<serde_json::Value> = heard.iter().map(|h| json!({ "start": h.start, "end": h.end })).collect();
    let started = std::time::Instant::now();
    let resp = state
        .http
        .post(format!("{}/speakers", tts.trim_end_matches('/')))
        .json(&json!({ "audio": audio, "segments": segments }))
        .timeout(Duration::from_secs(1800))
        .send()
        .await
        .context("the voice server isn't running")?;
    if !resp.status().is_success() {
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        bail!("{}", v["error"].as_str().unwrap_or("the voice server failed (it may need restarting)"));
    }
    #[derive(Deserialize)]
    struct Reply {
        speakers: Vec<Option<u32>>,
        #[serde(default)]
        turns: Vec<Vec<(f64, u32)>>,
    }
    let r: Reply = resp.json().await?;
    tracing::info!(
        "told {} voices apart in {:.0}s, {} changes inside lines",
        r.speakers.iter().flatten().collect::<HashSet<_>>().len(),
        started.elapsed().as_secs_f64(),
        r.turns.iter().map(Vec::len).sum::<usize>()
    );
    Ok((r.speakers, r.turns))
}

/// Lines where the voice changes partway, split there: at the end of a sentence near the
/// change when there is one (that's usually where the other person comes in), otherwise at
/// the nearest word.
fn split_at_turns(heard: Vec<Heard>, turns: &[Vec<(f64, u32)>]) -> Vec<Heard> {
    let mut out = Vec::with_capacity(heard.len());
    for (i, h) in heard.into_iter().enumerate() {
        let Some(ts) = turns.get(i).filter(|t| !t.is_empty()) else {
            out.push(h);
            continue;
        };
        let timed = !h.words.is_empty();
        let words = if timed { h.words.clone() } else { spread(&h) };
        let mut cuts: Vec<(usize, u32)> = Vec::new();
        for &(t, n) in ts {
            // The word the new voice starts on.
            let near = words.iter().position(|w| (w.0 + w.1) / 2.0 >= t).unwrap_or(words.len());
            let sentence = (1..words.len())
                .filter(|&k| (words[k].0 - t).abs() <= 1.5 && words[k - 1].2.trim_end_matches(['"', '\'', ')']).ends_with(['.', '?', '!']))
                .min_by(|&a, &b| (words[a].0 - t).abs().total_cmp(&(words[b].0 - t).abs()));
            let k = sentence.unwrap_or(near);
            if k > 0 && k < words.len() && cuts.last().is_none_or(|c| c.0 < k) {
                cuts.push((k, n));
            }
        }
        let text = |ws: &[(f64, f64, String)]| ws.iter().map(|w| w.2.as_str()).collect::<Vec<_>>().join(" ");
        let mut from = 0;
        let mut who = h.speaker;
        let mut start = h.start;
        for (k, n) in cuts.into_iter().chain(std::iter::once((words.len(), 0))) {
            let part = &words[from..k];
            if !part.is_empty() {
                let end = if k < words.len() { words[k].0 } else { h.end };
                out.push(Heard { start, end, text: text(part), words: if timed { part.to_vec() } else { vec![] }, speaker: who });
                start = end;
            }
            from = k;
            who = Some(n);
        }
    }
    out
}

/// The transcript in stretches of about half a minute (what the outline points at), with the
/// ads left out.
fn passages(heard: &[Heard], ads: &[AdSpan]) -> Vec<(f64, f64, String)> {
    let mut out: Vec<(f64, f64, String)> = Vec::new();
    let mut who = None;
    for h in heard {
        let mid = (h.start + h.end) / 2.0;
        if ads.iter().any(|a| mid >= a.start && mid < a.end) {
            continue;
        }
        // (Who's talking, where it changes: "S2: ...".)
        let changed = h.speaker.is_some() && h.speaker != who;
        let text = match h.speaker {
            Some(n) if changed => format!("S{n}: {}", h.text.trim()),
            _ => h.text.trim().to_string(),
        };
        who = h.speaker.or(who);
        match out.last_mut() {
            Some(last) if h.start - last.0 < 30.0 => {
                last.1 = h.end;
                last.2.push(' ');
                last.2.push_str(&text);
            }
            _ => {
                // Each passage says who it starts with.
                let text = match h.speaker {
                    Some(n) if !changed => format!("S{n}: {text}"),
                    _ => text,
                };
                out.push((h.start, h.end, text))
            }
        }
    }
    out
}

async fn outline_inner(state: &State, id: i64) -> anyhow::Result<Outline> {
    let path = episode_dir(state, id).join("transcript.json");
    let mut heard: Vec<Heard> = read_json(&path).await.unwrap_or_default();
    if heard.is_empty() {
        bail!("there's no transcript yet");
    }
    // Who's talking, told apart by their voices (once).
    let before: Outline = read_json(&episode_dir(state, id).join("outline.json")).await.unwrap_or_default();
    // Word times, for a transcript written before they were kept (lighting up the word being
    // said, and lines that start where the words do, not in the music before them).
    if heard.iter().all(|h| h.words.is_empty())
        && let Some(audio) = audio_file(state, id)
    {
        match time_lines(state, &audio, &heard).await {
            Ok(timed) if timed.len() == heard.len() => {
                for (h, t) in heard.iter_mut().zip(timed) {
                    if !t.words.is_empty() {
                        h.start = t.start;
                        h.end = t.end;
                        h.words = t.words;
                    }
                }
                write_json(&path, &heard).await?;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("timing the words in episode {id}: {e:#}"),
        }
    }
    if (heard.iter().all(|h| h.speaker.is_none()) || before.version < 3)
        && let Some(audio) = audio_file(state, id)
    {
        match tell_voices(state, &audio, &heard).await {
            Ok((who, turns)) if who.len() == heard.len() => {
                for (h, n) in heard.iter_mut().zip(who) {
                    h.speaker = n;
                }
                heard = split_at_turns(heard, &turns);
                write_json(&path, &heard).await?;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("telling voices apart in episode {id}: {e:#}"),
        }
    }
    let scan = scan_of(state, id).await;
    let r = sqlx::query("SELECT s.title AS show_title, e.title, e.notes, e.duration FROM show_episodes e JOIN shows s ON s.id = e.show_id WHERE e.id = ?")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let (show_title, title, notes, duration) =
        (r.get::<String, _>("show_title"), r.get::<String, _>("title"), clip(&plain(&r.get::<String, _>("notes")), 1500), r.get::<f64, _>("duration"));
    let duration = duration.max(heard.last().map(|h| h.end).unwrap_or(0.0));
    let heard = split_at_ads(heard, &scan.ads);
    let parts = passages(&heard, &scan.ads);
    if parts.is_empty() {
        bail!("the transcript is all ads");
    }
    let system = "You write the outline of a podcast episode from its transcript, for a podcast player: a summary, and a \
        table of contents someone can skim and jump around in. You get the transcript in numbered passages, each with its \
        time (ads are left out). Write:\n\
        - summary: 2 to 4 plain sentences on what the episode is about: the subjects, guests, and the main points or \
        conclusions. No \"In this episode\" opener.\n\
        - sections: the episode's parts in order, as the topic changes: roughly one every 5 to 15 minutes (fewer for a \
        short episode). Each has a short, specific title (2 to 7 words, Title Case not needed, no numbering) that says \
        what's discussed, not \"Discussion\" or \"Part 2\"; a one-sentence summary; and \"at\": the number of the passage \
        it starts at. The first section starts at passage 0 (call it the intro if that's what it is).\n\
        - subsections: in sections longer than about 6 minutes that cover several things, the smaller topics within it, \
        same shape (title, summary, at), in order, each starting at or after its section. Otherwise an empty list.\n\
        - speakers: the transcript marks who's talking as S1, S2... (told apart by voice, so now and then a line is \
        put to the wrong one). For each S number, their name: from how they introduce themselves or each other, how \
        others address them, the show notes, or the show's known host; if it isn't said, a short description \
        (\"Guest\", \"Caller\", \"Clip\"). And their role: host, co-host, guest, caller, narrator or clip. Leave \
        speakers empty when there are no S marks.\n\
        The transcript is speech recognition, so spell names as they're most likely meant. Reply with only JSON, no \
        prose: {\"summary\": \"...\", \"sections\": [{\"title\": \"...\", \"summary\": \"...\", \"at\": 0, \"subsections\": \
        [{\"title\": \"...\", \"summary\": \"...\", \"at\": 3}]}], \"speakers\": [{\"n\": 1, \"name\": \"...\", \"role\": \"host\"}]}";
    let mut text = format!("Show: {show_title}\nEpisode: {title}\nLength: {}\n", mmss(duration));
    if !notes.is_empty() {
        text.push_str(&format!("Show notes: {notes}\n"));
    }
    text.push_str("\nTranscript:\n");
    for (i, (t, _, words)) in parts.iter().enumerate() {
        text.push_str(&format!("[{i}] {} {}\n", mmss(*t), words));
    }
    let turns = vec![("user".to_string(), text)];
    let reply = if settings(state).await.ad_model == "local" {
        crate::llm::chat_local(state, system, &turns).await?
    } else {
        crate::llm::chat(state, system, &turns).await?
    };
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Part {
        title: String,
        summary: String,
        at: serde_json::Value,
        subsections: Vec<Part>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Reply {
        summary: String,
        sections: Vec<Part>,
        speakers: Vec<Named>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Named {
        n: serde_json::Value,
        name: String,
        role: String,
    }
    let json = match (reply.find('{'), reply.rfind('}')) {
        (Some(a), Some(b)) if b > a => &reply[a..=b],
        _ => bail!("the model didn't answer with an outline: {}", clip(&reply, 200)),
    };
    let got: Reply = serde_json::from_str(json).with_context(|| format!("the model's outline wasn't readable: {}", clip(&reply, 200)))?;
    let start_of = |v: &serde_json::Value| {
        let n = v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim_matches(['[', ']']).parse().ok())).unwrap_or(0) as usize;
        parts[n.min(parts.len() - 1)].0
    };
    // In order, each running to where the next one starts; subsections within their section,
    // the first starting with it (and a lone one dropped: it says nothing the section doesn't).
    fn place(list: Vec<Part>, from: f64, to: f64, start_of: &dyn Fn(&serde_json::Value) -> f64, nested: bool) -> Vec<OutlineSection> {
        let mut found: Vec<(f64, Part)> = list.into_iter().filter(|p| !p.title.trim().is_empty()).map(|p| (start_of(&p.at).clamp(from, to), p)).collect();
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found.dedup_by(|b, a| (b.0 - a.0).abs() < 1.0);
        if let Some(first) = found.first_mut() {
            first.0 = from;
        }
        let ends: Vec<f64> = (0..found.len()).map(|i| found.get(i + 1).map(|n| n.0).unwrap_or(to)).collect();
        found
            .into_iter()
            .zip(ends)
            .map(|((start, p), end)| {
                let mut subsections = if nested { place(p.subsections, start, end, start_of, false) } else { vec![] };
                if subsections.len() < 2 {
                    subsections.clear();
                }
                OutlineSection { title: p.title.trim().to_string(), summary: p.summary.trim().to_string(), start, end, subsections }
            })
            .collect()
    }
    let sections = place(got.sections, 0.0, duration, &start_of, true);
    if got.summary.trim().is_empty() && sections.is_empty() {
        bail!("the model's outline was empty");
    }
    let speakers = got
        .speakers
        .into_iter()
        .filter_map(|x| {
            let n = x.n.as_u64().or_else(|| x.n.as_str().and_then(|s| s.trim_start_matches(['S', 's']).parse().ok()))? as u32;
            (!x.name.trim().is_empty()).then(|| Speaker { n, name: x.name.trim().to_string(), role: x.role.trim().to_string() })
        })
        .collect();
    Ok(Outline { state: "done".into(), summary: got.summary.trim().to_string(), sections, error: None, speakers, version: OUTLINE_VERSION })
}

// ---- Apple's directory ----

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ItunesHit {
    collection_id: i64,
    collection_name: String,
    artist_name: String,
    artwork_url600: Option<String>,
    artwork_url100: Option<String>,
    feed_url: Option<String>,
    primary_genre_name: String,
    track_count: i64,
}

fn hits_from(results: Vec<ItunesHit>) -> Vec<ShowHit> {
    hits_with_ids(results).into_iter().map(|(_, h)| h).collect()
}

/// Shows found in Apple's directory, each with Apple's id for it.
fn hits_with_ids(results: Vec<ItunesHit>) -> Vec<(i64, ShowHit)> {
    results
        .into_iter()
        .filter_map(|r| {
            let id = r.collection_id;
            Some((id, ShowHit {
                feed_url: r.feed_url.filter(|u| !u.is_empty())?,
                title: r.collection_name,
                author: r.artist_name,
                image: r.artwork_url600.or(r.artwork_url100),
                genre: r.primary_genre_name,
                episodes: r.track_count,
                followed: None,
                rank: 0,
                source: "apple".into(),
            }))
        })
        .collect()
}

async fn lookup(state: &State, ids: &[String]) -> anyhow::Result<Vec<ShowHit>> {
    Ok(lookup_ids(state, ids).await?.into_iter().map(|(_, h)| h).collect())
}

async fn lookup_ids(state: &State, ids: &[String]) -> anyhow::Result<Vec<(i64, ShowHit)>> {
    #[derive(Deserialize)]
    struct Results {
        results: Vec<ItunesHit>,
    }
    let r: Results = state
        .http
        .get("https://itunes.apple.com/lookup")
        .query(&[("id", ids.join(",")), ("entity", "podcast".into())])
        .timeout(Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    // (Lookup answers in its own order; the charts' order is kept.)
    Ok(hits_with_ids(r.results))
}

async fn mark_followed(state: &State, mut hits: Vec<ShowHit>) -> Vec<ShowHit> {
    let followed: Vec<(i64, String)> = sqlx::query_as("SELECT id, feed_url FROM shows").fetch_all(&state.db).await.unwrap_or_default();
    for h in &mut hits {
        h.followed = followed.iter().find(|(_, u)| *u == h.feed_url).map(|(id, _)| *id);
    }
    hits
}

type ChartKey = (String, Option<i64>);
static CHARTS: LazyLock<tokio::sync::Mutex<HashMap<ChartKey, (std::time::Instant, Vec<ShowHit>)>>> = LazyLock::new(Default::default);

/// Apple's top shows (all, or one category), ranked, for the country in settings.
async fn chart(state: &State, genre: Option<i64>) -> anyhow::Result<Vec<ShowHit>> {
    let country = { let c = settings(state).await.country; if c.len() == 2 { c.to_ascii_lowercase() } else { "us".into() } };
    let key = (country.clone(), genre);
    if let Some((at, hits)) = CHARTS.lock().await.get(&key) {
        if at.elapsed() < Duration::from_secs(6 * 3600) {
            return Ok(hits.clone());
        }
    }
    let ids: Vec<String> = match genre {
        None => {
            let v: serde_json::Value = state
                .http
                .get(format!("https://rss.marketingtools.apple.com/api/v2/{country}/podcasts/top/50/podcasts.json"))
                .timeout(Duration::from_secs(20))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            v["feed"]["results"].as_array().into_iter().flatten().filter_map(|r| r["id"].as_str().map(str::to_string)).collect()
        }
        Some(g) => {
            // (The older chart feed is the one that has categories.)
            let v: serde_json::Value = state
                .http
                .get(format!("https://itunes.apple.com/{country}/rss/toppodcasts/limit=50/genre={g}/json"))
                .timeout(Duration::from_secs(20))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            v["feed"]["entry"].as_array().into_iter().flatten().filter_map(|e| e["id"]["attributes"]["im:id"].as_str().map(str::to_string)).collect()
        }
    };
    let found = lookup_ids(state, &ids).await?;
    let mut hits: Vec<ShowHit> = found
        .into_iter()
        .filter_map(|(id, mut h)| {
            h.rank = ids.iter().position(|x| x.parse() == Ok(id))? as i64 + 1;
            Some(h)
        })
        .collect();
    hits.sort_by_key(|h| h.rank);
    CHARTS.lock().await.insert(key, (std::time::Instant::now(), hits.clone()));
    Ok(hits)
}

// ---- Podcast Index (podcastindex.org) ----

async fn podcastindex(state: &State, path: &str, query: &[(&str, String)]) -> anyhow::Result<Vec<ShowHit>> {
    let prefs = settings(state).await;
    let (key, secret) = (prefs.podcastindex_key.trim().to_string(), prefs.podcastindex_secret.trim().to_string());
    if key.is_empty() || secret.is_empty() {
        return Ok(Vec::new());
    }
    let date = Utc::now().timestamp().to_string();
    let hash = sha1_smol::Sha1::from(format!("{key}{secret}{date}")).digest().to_string();
    let v: serde_json::Value = state
        .http
        .get(format!("https://api.podcastindex.org/api/1.0/{path}"))
        .query(query)
        .header("X-Auth-Key", key)
        .header("X-Auth-Date", date)
        .header("Authorization", hash)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("couldn't reach Podcast Index")?
        .error_for_status()
        .context("Podcast Index refused (check the key and secret in Podcasts settings)")?
        .json()
        .await?;
    Ok(v["feeds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let url = f["url"].as_str().filter(|u| !u.is_empty())?.to_string();
            let image = [&f["artwork"], &f["image"]].iter().find_map(|x| x.as_str().filter(|u| !u.is_empty()).map(str::to_string));
            Some(ShowHit {
                title: f["title"].as_str().unwrap_or_default().trim().to_string(),
                author: f["author"].as_str().unwrap_or_default().trim().to_string(),
                image,
                feed_url: url,
                genre: f["categories"].as_object().and_then(|c| c.values().next()).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                episodes: f["episodeCount"].as_i64().unwrap_or(0),
                source: "podcastindex".into(),
                ..Default::default()
            })
        })
        .collect())
}

/// Podcast Index's category names for an Apple category ("Society & Culture" → "Society,Culture").
fn pi_categories(genre: i64) -> Option<String> {
    GENRES.iter().find(|(id, _)| *id == genre).map(|(_, name)| name.split(" & ").collect::<Vec<_>>().join(","))
}

/// The same show listed twice (http vs https, a trailing slash) counts once.
fn same_feed(a: &str, b: &str) -> bool {
    let norm = |u: &str| u.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_ascii_lowercase();
    norm(a) == norm(b)
}

// ---- OPML ----

fn opml(shows: &[Show]) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n<head><title>duet podcasts</title></head>\n<body>\n");
    for s in shows {
        out.push_str(&format!("  <outline type=\"rss\" text=\"{}\" title=\"{}\" xmlUrl=\"{}\"/>\n", esc(&s.title), esc(&s.title), esc(&s.feed_url)));
    }
    out.push_str("</body>\n</opml>\n");
    out
}

fn opml_feeds(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"outline" => {
                if let Some(u) = attr(&e, "xmlUrl").or_else(|| attr(&e, "xmlurl")) {
                    out.push(u);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

// ---- Routes ----

fn bad_gateway(e: anyhow::Error) -> ApiError {
    ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}"))
}

async fn home(AxState(s): AxState<State>) -> ApiResult<Json<ShowsHome>> {
    Ok(Json(ShowsHome {
        shows: shows(&s).await?,
        in_progress: episodes_where(&s, "e.finished = 0 AND e.position > 5 ORDER BY e.played_at DESC", None, 12).await?,
        latest: episodes_where(&s, "e.finished = 0 AND e.position <= 5 ORDER BY e.published DESC", None, 24).await?,
    }))
}

#[derive(Deserialize)]
struct FollowReq {
    url: String,
}

async fn follow(AxState(s): AxState<State>, Json(r): Json<FollowReq>) -> ApiResult<Json<Show>> {
    Ok(Json(follow_feed(&s, &r.url).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?))
}

#[derive(Deserialize)]
struct SearchQ {
    q: String,
}

async fn search(AxState(s): AxState<State>, Query(q): Query<SearchQ>) -> ApiResult<Json<Vec<ShowHit>>> {
    #[derive(Deserialize)]
    struct Results {
        results: Vec<ItunesHit>,
    }
    let term = q.q.trim().to_string();
    let apple = async {
        let r: Results = s
            .http
            .get("https://itunes.apple.com/search")
            .query(&[("media", "podcast"), ("entity", "podcast"), ("limit", "30"), ("term", term.as_str())])
            .timeout(Duration::from_secs(20))
            .send()
            .await?
            .json()
            .await?;
        anyhow::Ok(hits_from(r.results))
    };
    let pi_query = [("q", term.clone()), ("max", "30".into())];
    let index = podcastindex(&s, "search/byterm", &pi_query);
    let (apple, index) = tokio::join!(apple, index);
    if let Err(e) = &index {
        tracing::warn!("Podcast Index search: {e:#}");
    }
    let mut hits = match (apple, index.unwrap_or_default()) {
        (Ok(a), more) => {
            let mut a = a;
            for h in more {
                if !a.iter().any(|x| same_feed(&x.feed_url, &h.feed_url)) {
                    a.push(h);
                }
            }
            a
        }
        (Err(e), more) if more.is_empty() => return Err(bad_gateway(e)),
        (Err(_), more) => more,
    };
    hits.truncate(50);
    Ok(Json(mark_followed(&s, hits).await))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ChartQ {
    genre: Option<i64>,
}

/// Discover: Apple's chart (all, or a category), and what's trending on Podcast Index.
async fn charts(AxState(s): AxState<State>, Query(q): Query<ChartQ>) -> ApiResult<Json<Chart>> {
    let mut query = vec![("max", "40".to_string()), ("lang", "en".to_string())];
    if let Some(cat) = q.genre.and_then(pi_categories) {
        query.push(("cat", cat));
    }
    let (apple, trending) = tokio::join!(chart(&s, q.genre), podcastindex(&s, "podcasts/trending", &query));
    let trending = trending.unwrap_or_else(|e| {
        tracing::warn!("Podcast Index trending: {e:#}");
        Vec::new()
    });
    let shows = match apple {
        Ok(a) => a,
        Err(e) if trending.is_empty() => return Err(bad_gateway(e)),
        Err(_) => Vec::new(),
    };
    let mut trending = mark_followed(&s, trending).await;
    for (i, h) in trending.iter_mut().enumerate() {
        h.rank = i as i64 + 1;
    }
    Ok(Json(Chart { shows: mark_followed(&s, shows).await, trending }))
}

async fn top(AxState(s): AxState<State>) -> ApiResult<Json<Vec<ShowHit>>> {
    let hits = chart(&s, None).await.map_err(bad_gateway)?;
    Ok(Json(mark_followed(&s, hits).await))
}

#[derive(Deserialize)]
struct PreviewQ {
    url: String,
}

/// A show's page before it's followed (its episodes play straight from its own server).
async fn preview(AxState(s): AxState<State>, Query(q): Query<PreviewQ>) -> ApiResult<Json<ShowPage>> {
    let url = feed_address(&s, &q.url).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    if let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM shows WHERE feed_url = ?").bind(&url).fetch_optional(&s.db).await? {
        return show_page(AxState(s), Path(id)).await;
    }
    let feed = fetch_feed(&s, &url).await.map_err(bad_gateway)?;
    let mut episodes: Vec<ShowEpisode> = feed
        .episodes
        .iter()
        .take(100)
        .map(|e| ShowEpisode {
            show_title: feed.title.clone(),
            title: e.title.clone(),
            description: clip(&plain(&e.notes), 400),
            notes_html: e.notes.clone(),
            published: e.published.map(|d| d.to_rfc3339()),
            duration: e.duration,
            image: e.image.clone().or(feed.image.clone()),
            audio_url: e.audio_url.clone(),
            needs_download: is_youtube(&e.audio_url),
            ..Default::default()
        })
        .collect();
    episodes.sort_by(|a, b| b.published.cmp(&a.published));
    Ok(Json(ShowPage {
        show: Show {
            kind: if is_youtube(&url) { "youtube".into() } else { "rss".into() },
            feed_url: url,
            title: feed.title.clone(),
            author: feed.author.clone(),
            description: feed.description.clone(),
            image: feed.image.clone(),
            link: feed.link.clone(),
            skip_ads: true,

            episodes: feed.episodes.len() as i64,
            ..Default::default()
        },
        episodes,
    }))
}

async fn refresh_all(AxState(s): AxState<State>) -> ApiResult<Json<ShowsHome>> {
    refresh(&s).await?;
    home(AxState(s)).await
}

async fn get_settings(AxState(s): AxState<State>) -> Json<ShowSettings> {
    Json(settings(&s).await)
}

async fn put_settings(AxState(s): AxState<State>, Json(new): Json<ShowSettings>) -> ApiResult<Json<ShowSettings>> {
    write_json(&base(&s).join("settings.json"), &new).await?;
    Ok(Json(new))
}

async fn export_opml(AxState(s): AxState<State>) -> ApiResult<Response> {
    Ok((
        [(header::CONTENT_TYPE, "text/x-opml; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"duet-podcasts.opml\"")],
        opml(&shows(&s).await?),
    )
        .into_response())
}

/// Follows every show in an OPML file (another podcast app's export).
async fn import_opml(AxState(s): AxState<State>, body: String) -> ApiResult<Json<serde_json::Value>> {
    let feeds = opml_feeds(&body);
    if feeds.is_empty() {
        return Err(ApiError::bad_request("No podcasts found in that file (it should be an OPML export)"));
    }
    let results = futures_util::stream::iter(feeds.clone()).map(|u| { let s = s.clone(); async move { follow_feed(&s, &u).await } }).buffer_unordered(4).collect::<Vec<_>>().await;
    let added = results.iter().filter(|r| r.is_ok()).count();
    Ok(Json(json!({ "added": added, "failed": feeds.len() - added })))
}

async fn show_page(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<Json<ShowPage>> {
    let show = show(&s, id).await?.ok_or_else(|| ApiError::not_found("show"))?;
    let episodes = episodes_where(&s, "e.show_id = ? ORDER BY e.published DESC", Some(id), 500).await?;
    Ok(Json(ShowPage { show, episodes }))
}

#[derive(Deserialize)]
struct ShowEdit {
    skip_ads: Option<bool>,
}

async fn edit_show(AxState(s): AxState<State>, Path(id): Path<i64>, Json(e): Json<ShowEdit>) -> ApiResult<Json<Show>> {
    if let Some(v) = e.skip_ads {
        sqlx::query("UPDATE shows SET skip_ads = ? WHERE id = ?").bind(v).bind(id).execute(&s.db).await?;
    }
    show(&s, id).await?.map(Json).ok_or_else(|| ApiError::not_found("show"))
}

async fn unfollow(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<()> {
    let eps: Vec<i64> = sqlx::query_scalar("SELECT id FROM show_episodes WHERE show_id = ?").bind(id).fetch_all(&s.db).await?;
    QUEUE.lock().unwrap().retain(|x| !eps.contains(x));
    sqlx::query("DELETE FROM shows WHERE id = ?").bind(id).execute(&s.db).await?;
    for e in eps {
        if !SCANS.lock().unwrap().get(&e).is_some_and(AdScan::busy) {
            let _ = tokio::fs::remove_dir_all(episode_dir(&s, e)).await;
        }
    }
    Ok(())
}

async fn mark_all_played(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<()> {
    sqlx::query("UPDATE show_episodes SET finished = 1 WHERE show_id = ?").bind(id).execute(&s.db).await?;
    Ok(())
}

async fn episode(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<Json<ShowEpisode>> {
    full_episode(&s, id).await?.map(Json).ok_or_else(|| ApiError::not_found("episode"))
}

/// The recording: the downloaded copy (its ads are the ones found), or the show's own.
async fn audio(AxState(s): AxState<State>, Path(id): Path<i64>, req: Request) -> ApiResult<Response> {
    if let Some(path) = audio_file(&s, id) {
        let resp = tower_http::services::ServeFile::new(path).oneshot(req).await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(resp.map(Body::new).into_response());
    }
    let url: String = sqlx::query_scalar("SELECT audio_url FROM show_episodes WHERE id = ?")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or_else(|| ApiError::not_found("episode"))?;
    Ok(Redirect::temporary(&url).into_response())
}

async fn put_position(AxState(s): AxState<State>, Path(id): Path<i64>, Json(p): Json<ShowPosition>) -> ApiResult<()> {
    sqlx::query(
        "UPDATE show_episodes SET position = ?, finished = ?, played_at = ?, duration = CASE WHEN ? > 0 AND duration = 0 THEN ? ELSE duration END WHERE id = ?",
    )
    .bind(if p.finished { 0.0 } else { p.position })
    .bind(p.finished)
    .bind(Utc::now().to_rfc3339())
    .bind(p.duration)
    .bind(p.duration)
    .bind(id)
    .execute(&s.db)
    .await?;
    Ok(())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PrepareReq {
    /// It's about to be played: first in line.
    now: bool,
}

/// Downloads an episode and finds its ads (first, if it's about to be played).
async fn prepare(AxState(s): AxState<State>, Path(id): Path<i64>, Json(r): Json<PrepareReq>) -> ApiResult<Json<AdScan>> {
    let scan = scan_of(&s, id).await;
    // Checked the old way (ads cut only at whole transcript lines): read again, from the
    // transcript it already has, so it's quick.
    if scan.state == "done" && scan.version < CUTS && episode_dir(&s, id).join("transcript.json").exists() {
        set_scan(&s, id, |x| *x = AdScan::default()).await;
        queue(&s, id, true);
        let mut scan = scan_of(&s, id).await;
        scan.state = "queued".into();
        return Ok(Json(scan));
    }
    if scan.state == "done" || (scan.busy() && !r.now) {
        return Ok(Json(scan));
    }
    if r.now && audio_file(&s, id).is_none() {
        // The download starts straight away, whatever's being checked.
        let st = s.clone();
        tokio::spawn(async move {
            if let Err(e) = download(&st, id).await {
                set_scan(&st, id, |x| {
                    x.state = "failed".into();
                    x.error = Some(format!("{e:#}"));
                })
                .await;
            }
        });
    }
    queue(&s, id, r.now);
    let mut scan = scan_of(&s, id).await;
    if !scan.busy() {
        scan.state = "queued".into();
    }
    Ok(Json(scan))
}

async fn ads(AxState(s): AxState<State>, Path(id): Path<i64>) -> Json<AdScan> {
    Json(scan_of(&s, id).await)
}

/// The listener says that wasn't an ad: it plays from now on.
async fn not_an_ad(AxState(s): AxState<State>, Path((id, n)): Path<(i64, usize)>) -> Json<AdScan> {
    Json(
        set_scan(&s, id, |x| {
            if n < x.ads.len() {
                x.ads.remove(n);
            }
        })
        .await,
    )
}

async fn rescan(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<Json<AdScan>> {
    if SCANS.lock().unwrap().get(&id).is_some_and(AdScan::busy) {
        return Err(ApiError::bad_request("It's being checked right now"));
    }
    // (The transcript's kept: the ads are read from it again.)
    set_scan(&s, id, |x| *x = AdScan::default()).await;
    queue(&s, id, true);
    Ok(Json(scan_of(&s, id).await))
}

async fn transcript(AxState(s): AxState<State>, Path(id): Path<i64>) -> Json<Vec<Heard>> {
    let heard: Vec<Heard> = read_json(&episode_dir(&s, id).join("transcript.json")).await.unwrap_or_default();
    Json(split_at_ads(heard, &scan_of(&s, id).await.ads))
}

/// Lines that run an ad and the show together, split where the ad starts or ends, so each is
/// all ad or all show.
fn split_at_ads(heard: Vec<Heard>, ads: &[AdSpan]) -> Vec<Heard> {
    let cuts: Vec<f64> = ads.iter().flat_map(|a| [a.start, a.end]).collect();
    let mut out = Vec::with_capacity(heard.len());
    for h in heard {
        let mut rest = h;
        loop {
            let Some(&t) = cuts.iter().filter(|&&t| t > rest.start + 0.3 && t < rest.end - 0.3).min_by(|a, b| a.total_cmp(b)) else { break };
            let words = if rest.words.is_empty() { spread(&rest) } else { rest.words.clone() };
            // The first word that starts at (or after) the cut begins the second part.
            let k = words.iter().position(|w| (w.0 + w.1) / 2.0 >= t).unwrap_or(words.len());
            if k == 0 || k >= words.len() {
                break;
            }
            let (a, b) = words.split_at(k);
            let text = |ws: &[(f64, f64, String)]| ws.iter().map(|w| w.2.as_str()).collect::<Vec<_>>().join(" ");
            out.push(Heard { start: rest.start, end: t, text: text(a), words: if rest.words.is_empty() { vec![] } else { a.to_vec() }, speaker: rest.speaker });
            rest = Heard { start: t, end: rest.end, text: text(b), words: if rest.words.is_empty() { vec![] } else { b.to_vec() }, speaker: rest.speaker };
        }
        out.push(rest);
    }
    out
}

/// A line's words with its time shared out by their lengths (when Whisper didn't time them).
fn spread(h: &Heard) -> Vec<(f64, f64, String)> {
    let ws: Vec<&str> = h.text.split_whitespace().collect();
    let total: f64 = ws.iter().map(|w| w.chars().count() as f64 + 1.0).sum::<f64>().max(1.0);
    let mut t = h.start;
    ws.into_iter()
        .map(|w| {
            let d = (h.end - h.start) * (w.chars().count() as f64 + 1.0) / total;
            t += d;
            (t - d, t, w.to_string())
        })
        .collect()
}

/// The outline; one that was never made is started now, for an episode whose transcript is
/// whole (one checked before outlines existed, say).
async fn get_outline(AxState(s): AxState<State>, Path(id): Path<i64>) -> Json<Outline> {
    let o = outline_of(&s, id).await;
    let stale = o.state.is_empty() || (o.state == "done" && o.version < OUTLINE_VERSION);
    if stale && scan_of(&s, id).await.state == "done" && episode_dir(&s, id).join("transcript.json").exists() {
        let st = s.clone();
        OUTLINING.lock().unwrap().insert(id);
        tokio::spawn(async move {
            OUTLINING.lock().unwrap().remove(&id);
            let _ = make_outline(&st, id).await;
        });
        return Json(Outline { state: "making".into(), ..Default::default() });
    }
    Json(o)
}

/// Makes (or remakes) the outline: now if the transcript's whole, otherwise once it is.
async fn remake_outline(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<Json<Outline>> {
    if OUTLINING.lock().unwrap().contains(&id) {
        return Ok(Json(outline_of(&s, id).await));
    }
    let scan = scan_of(&s, id).await;
    let has_transcript = episode_dir(&s, id).join("transcript.json").exists();
    if scan.state == "done" && !has_transcript {
        // (Its ads came from SponsorBlock before transcripts were written for those: listen now.)
        set_scan(&s, id, |x| x.state = "queued".into()).await;
        queue(&s, id, true);
    } else if scan.state != "done" {
        queue(&s, id, true);
    } else {
        let st = s.clone();
        OUTLINING.lock().unwrap().insert(id);
        tokio::spawn(async move {
            OUTLINING.lock().unwrap().remove(&id);
            let _ = make_outline(&st, id).await;
        });
        return Ok(Json(Outline { state: "making".into(), ..Default::default() }));
    }
    Ok(Json(Outline { state: "waiting".into(), ..Default::default() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_podcast_feed() {
        let xml = r#"<?xml version="1.0"?><rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel>
            <title>Retro Hour</title><link>https://example.com</link><itunes:author>Dan &amp; Ravi</itunes:author>
            <description>Old games.</description><itunes:image href="https://example.com/art.jpg"/>
            <image><url>https://example.com/small.jpg</url><title>Retro Hour</title></image>
            <item><title>Episode 2</title><guid>ep2</guid><pubDate>Tue, 05 Nov 2024 10:00:00 GMT</pubDate>
              <itunes:duration>1:02:03</itunes:duration><description><![CDATA[<p>Notes</p>]]></description>
              <content:encoded><![CDATA[<p>Longer notes with <a href="x">links</a></p>]]></content:encoded>
              <enclosure url="https://example.com/2.mp3" type="audio/mpeg" length="1"/></item>
            <item><title>No audio</title><guid>x</guid></item>
            <item><title>Episode 1</title><pubDate>Mon, 04 Nov 2024 10:00:00 +0000</pubDate><itunes:duration>3723</itunes:duration>
              <enclosure url="https://example.com/1.m4a" type="audio/x-m4a"/><itunes:image href="https://example.com/1.jpg"/></item>
        </channel></rss>"#;
        let f = parse_feed(xml);
        assert_eq!(f.title, "Retro Hour");
        assert_eq!(f.author, "Dan & Ravi");
        assert_eq!(f.image.as_deref(), Some("https://example.com/art.jpg"));
        assert_eq!(f.episodes.len(), 2);
        assert_eq!(f.episodes[0].duration, 3723.0);
        assert!(f.episodes[0].notes.contains("Longer notes"));
        assert_eq!(f.episodes[1].guid, "https://example.com/1.m4a");
        assert_eq!(f.episodes[1].image.as_deref(), Some("https://example.com/1.jpg"));
        assert!(f.episodes[1].published.is_some());
    }

    #[test]
    fn reads_a_youtube_channel_feed() {
        let xml = r#"<?xml version="1.0"?><feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/" xmlns="http://www.w3.org/2005/Atom">
            <link rel="self" href="https://www.youtube.com/feeds/videos.xml?channel_id=UC1"/><title>Retro Tech</title>
            <link rel="alternate" href="https://www.youtube.com/channel/UC1"/><author><name>Retro Tech</name><uri>x</uri></author>
            <entry><id>yt:video:abc</id><yt:videoId>abc</yt:videoId><title>Restoring a Jaguar</title>
              <link rel="alternate" href="https://www.youtube.com/watch?v=abc"/><published>2026-11-10T15:00:00+00:00</published>
              <media:group><media:title>Restoring a Jaguar</media:title><media:thumbnail url="https://i.ytimg.com/vi/abc/hqdefault.jpg" width="480" height="360"/>
              <media:description>Line one
Line two</media:description></media:group></entry>
            <entry><yt:videoId>s1</yt:videoId><title>A short</title><link rel="alternate" href="https://www.youtube.com/shorts/s1"/></entry>
        </feed>"#;
        let f = parse_feed(xml);
        assert_eq!(f.title, "Retro Tech");
        assert_eq!(f.author, "Retro Tech");
        assert_eq!(f.link, "https://www.youtube.com/channel/UC1");
        assert_eq!(f.episodes.len(), 1);
        let e = &f.episodes[0];
        assert_eq!(e.audio_url, "https://www.youtube.com/watch?v=abc");
        assert_eq!(e.guid, "abc");
        assert!(e.published.is_some());
        assert_eq!(e.notes, "Line one<br>Line two");
        assert_eq!(f.image.as_deref(), Some("https://i.ytimg.com/vi/abc/hqdefault.jpg"));
    }

    #[test]
    fn splits_lines_where_the_voice_changes() {
        let h = Heard { start: 0.0, end: 10.0, text: "So what happened next? Well, it was a long night.".into(), words: vec![], speaker: Some(1) };
        // The change is heard a little after the question ends.
        let out = split_at_turns(vec![h], &[vec![(4.8, 2)]]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "So what happened next?");
        assert_eq!(out[0].speaker, Some(1));
        assert_eq!(out[1].text, "Well, it was a long night.");
        assert_eq!(out[1].speaker, Some(2));
    }

    #[test]
    fn splits_lines_at_ads() {
        let h = Heard { start: 0.0, end: 10.0, text: "call 855-KESEMTA. Hello again. It's me.".into(), words: vec![], speaker: None };
        let out = split_at_ads(vec![h], &[AdSpan { start: -20.0, end: 4.0, label: String::new() }]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "call 855-KESEMTA.");
        assert!(out[1].text.starts_with("Hello again."));
    }

    #[test]
    fn finds_quoted_words() {
        let said = plain_words("That's Kesimpta. Call 855-KESEMTA. Hello again. It's me.");
        let q = plain_words("call 855-KESEMTA.");
        assert_eq!(find_run(&said, &q, None), Some(2));
        assert_eq!(find_run(&said, &plain_words("nope"), None), None);
    }

    #[test]
    fn joins_overlapping_ads() {
        let mut ads = Vec::new();
        add_span(&mut ads, AdSpan { start: 100.0, end: 160.0, label: "Squarespace".into() });
        add_span(&mut ads, AdSpan { start: 500.0, end: 530.0, label: "Promo".into() });
        add_span(&mut ads, AdSpan { start: 158.0, end: 200.0, label: "x".into() });
        assert_eq!(ads.len(), 2);
        assert_eq!((ads[0].start, ads[0].end), (100.0, 200.0));
        assert_eq!(ads[0].label, "Squarespace");
    }

    #[test]
    fn reads_opml() {
        let xml = r#"<opml><body><outline text="A" xmlUrl="https://a/feed"/><outline text="group"><outline xmlUrl="https://b/rss"></outline></outline></body></opml>"#;
        assert_eq!(opml_feeds(xml), vec!["https://a/feed".to_string(), "https://b/rss".to_string()]);
    }

    #[test]
    fn notes_without_scripts() {
        assert_eq!(plain("<p>Hi</p><script>alert(1)</script><STYLE>x{}</STYLE> there"), "Hi there");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1:00:01"), 3601.0);
        assert_eq!(parse_duration("90"), 90.0);
        assert_eq!(parse_duration("2:30"), 150.0);
    }
}
