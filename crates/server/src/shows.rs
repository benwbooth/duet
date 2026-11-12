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
use duet_common::shows::{AdScan, AdSpan, Show, ShowEpisode, ShowHit, ShowPage, ShowPosition, ShowSettings, ShowsHome};
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
const WINDOW: f64 = 600.0;
/// Each stretch is read with the end of the one before it, so an ad across the join is whole.
const OVERLAP: f64 = 120.0;

pub fn routes() -> Router<State> {
    Router::new()
        .route("/shows", get(home).post(follow))
        .route("/shows/search", get(search))
        .route("/shows/top", get(top))
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
                if name == "item" {
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
                if name == "item" {
                    if let Some(mut i) = item.take() {
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
                    if parent != "item" {
                        continue;
                    }
                    match name.as_str() {
                        "title" => i.title = strip_html(&value),
                        "guid" => i.guid = value,
                        "pubDate" => i.published = parse_date(&value),
                        "itunes:duration" => i.duration = parse_duration(&value),
                        "description" | "itunes:summary" if value.len() > i.notes.len() => i.notes = value,
                        "content:encoded" => long_notes = value,
                        _ => {}
                    }
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
    show
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
    if input.starts_with("http://") || input.starts_with("https://") {
        Ok(input.to_string())
    } else if input.contains('.') && !input.contains(' ') {
        Ok(format!("https://{input}"))
    } else {
        bail!("Paste the show's feed address (or its Apple Podcasts link)")
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
    let id: i64 = sqlx::query_scalar("INSERT INTO shows (feed_url, title) VALUES (?, ?) RETURNING id")
        .bind(&url)
        .bind(&feed.title)
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
    tokio::time::sleep(Duration::from_secs(90)).await;
    tokio::spawn(scanner(state.clone()));
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

// ---- Reading the database ----

const SHOW_COLUMNS: &str = "s.id, s.feed_url, s.title, s.author, s.description, s.image, s.link, s.skip_ads,
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

const EPISODE_COLUMNS: &str = "e.id, e.show_id, s.title AS show_title, e.title, e.notes, e.published, e.duration, e.audio_url,
    COALESCE(e.image, s.image) AS image, e.position, e.finished";

fn episode_from(state: &State, r: &sqlx::sqlite::SqliteRow) -> ShowEpisode {
    let id: i64 = r.get("id");
    let notes: String = r.get("notes");
    ShowEpisode {
        id,
        show_id: r.get("show_id"),
        show_title: r.get("show_title"),
        title: r.get("title"),
        description: clip(&strip_html(&notes), 400),
        notes_html: String::new(),
        published: r.get("published"),
        duration: r.get("duration"),
        image: r.get("image"),
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
            Ok(true) => {}
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Heard {
    start: f64,
    end: f64,
    text: String,
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
        (r.get::<String, _>("show_title"), r.get::<String, _>("title"), clip(&strip_html(&r.get::<String, _>("notes")), 1500), settings(state).await.skip_promos)
    };
    let tts = state.settings.read().await.ai.tts_url.clone();
    let mut checked = scan_of(state, id).await.checked;
    loop {
        if checked >= duration - 1.0 {
            break;
        }
        let wanted = WANTED.load(Ordering::SeqCst);
        if wanted != 0 && wanted != id {
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
            s.doing = format!("Listening for ads: {} of {}", mmss(checked), mmss(duration));
        })
        .await;
        #[derive(Deserialize)]
        struct Reply {
            segments: Vec<Heard>,
            end: f64,
        }
        let resp = state
            .http
            .post(format!("{}/transcribe-window", tts.trim_end_matches('/')))
            .json(&json!({ "audio": path, "start": checked, "duration": len }))
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
        heard.retain(|h| h.start < checked);
        heard.extend(reply.segments);
        write_json(&transcript_path, &heard).await?;
        // This stretch, with the end of the last for context.
        let from = (checked - OVERLAP).max(0.0);
        let lines: Vec<&Heard> = heard.iter().filter(|h| h.end > from && h.start < end).collect();
        let found = find_ads(state, &show_title, &title, &notes, &lines, skip_promos).await?;
        let new_checked = end.min(duration);
        set_scan(state, id, |s| {
            for a in found {
                add_span(&mut s.ads, a);
            }
            s.checked = new_checked;
            s.duration = duration;
        })
        .await;
        checked = new_checked;
    }
    set_scan(state, id, |s| {
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
async fn find_ads(state: &State, show: &str, title: &str, notes: &str, lines: &[&Heard], promos: bool) -> anyhow::Result<Vec<AdSpan>> {
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
         so expect small errors. Reply with only JSON, no prose: an array of {{\"from\": first line number, \"to\": last \
         line number, \"label\": a few words saying who or what it's for}}, or [] if there are no ads."
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
    }
    let json = match (reply.find('['), reply.rfind(']')) {
        (Some(a), Some(b)) if b > a => &reply[a..=b],
        _ => "[]",
    };
    let found: Vec<Found> = serde_json::from_str(json).with_context(|| format!("the model's answer wasn't a list of ads: {}", clip(&reply, 200)))?;
    let num = |v: &serde_json::Value| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim_matches(['[', ']']).parse().ok())).map(|n| n as usize);
    Ok(found
        .into_iter()
        .filter_map(|f| {
            let (a, b) = (num(&f.from)?, num(&f.to)?);
            let (a, b) = (a.min(b), a.max(b));
            let (first, last) = (lines.get(a)?, lines.get(b.min(lines.len() - 1))?);
            // (Very short "ads" are usually a mention, not a break: left alone.)
            (last.end - first.start >= 8.0).then(|| AdSpan { start: first.start, end: last.end, label: f.label.trim().to_string() })
        })
        .collect())
}

// ---- Apple's directory ----

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ItunesHit {
    collection_name: String,
    artist_name: String,
    artwork_url600: Option<String>,
    artwork_url100: Option<String>,
    feed_url: Option<String>,
    primary_genre_name: String,
    track_count: i64,
}

fn hits_from(results: Vec<ItunesHit>) -> Vec<ShowHit> {
    results
        .into_iter()
        .filter_map(|r| {
            Some(ShowHit {
                feed_url: r.feed_url.filter(|u| !u.is_empty())?,
                title: r.collection_name,
                author: r.artist_name,
                image: r.artwork_url600.or(r.artwork_url100),
                genre: r.primary_genre_name,
                episodes: r.track_count,
                followed: None,
            })
        })
        .collect()
}

async fn lookup(state: &State, ids: &[String]) -> anyhow::Result<Vec<ShowHit>> {
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
    let hits = hits_from(r.results);
    Ok(hits)
}

async fn mark_followed(state: &State, mut hits: Vec<ShowHit>) -> Vec<ShowHit> {
    let followed: Vec<(i64, String)> = sqlx::query_as("SELECT id, feed_url FROM shows").fetch_all(&state.db).await.unwrap_or_default();
    for h in &mut hits {
        h.followed = followed.iter().find(|(_, u)| *u == h.feed_url).map(|(id, _)| *id);
    }
    hits
}

static TOP: LazyLock<tokio::sync::Mutex<Option<(std::time::Instant, Vec<ShowHit>)>>> = LazyLock::new(Default::default);

async fn top_shows(state: &State) -> anyhow::Result<Vec<ShowHit>> {
    let mut cache = TOP.lock().await;
    if let Some((at, hits)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(6 * 3600) {
            return Ok(hits.clone());
        }
    }
    let v: serde_json::Value = state
        .http
        .get("https://rss.marketingtools.apple.com/api/v2/us/podcasts/top/40/podcasts.json")
        .timeout(Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ids: Vec<String> = v["feed"]["results"].as_array().into_iter().flatten().filter_map(|r| r["id"].as_str().map(str::to_string)).collect();
    let mut hits = lookup(state, &ids).await?;
    let order: HashMap<String, usize> = v["feed"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, r)| Some((r["name"].as_str()?.to_string(), i)))
        .collect();
    hits.sort_by_key(|h| order.get(&h.title).copied().unwrap_or(usize::MAX));
    *cache = Some((std::time::Instant::now(), hits.clone()));
    Ok(hits)
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
    let r: Results = s
        .http
        .get("https://itunes.apple.com/search")
        .query(&[("media", "podcast"), ("entity", "podcast"), ("limit", "30"), ("term", q.q.trim())])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| bad_gateway(e.into()))?
        .json()
        .await
        .map_err(|e| bad_gateway(e.into()))?;
    Ok(Json(mark_followed(&s, hits_from(r.results)).await))
}

async fn top(AxState(s): AxState<State>) -> ApiResult<Json<Vec<ShowHit>>> {
    let hits = top_shows(&s).await.map_err(bad_gateway)?;
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
            description: clip(&strip_html(&e.notes), 400),
            notes_html: e.notes.clone(),
            published: e.published.map(|d| d.to_rfc3339()),
            duration: e.duration,
            image: e.image.clone().or(feed.image.clone()),
            audio_url: e.audio_url.clone(),
            ..Default::default()
        })
        .collect();
    episodes.sort_by(|a, b| b.published.cmp(&a.published));
    Ok(Json(ShowPage {
        show: Show {
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
    set_scan(&s, id, |x| *x = AdScan::default()).await;
    let _ = tokio::fs::remove_file(episode_dir(&s, id).join("transcript.json")).await;
    queue(&s, id, true);
    Ok(Json(scan_of(&s, id).await))
}

async fn transcript(AxState(s): AxState<State>, Path(id): Path<i64>) -> Json<Vec<Heard>> {
    Json(read_json(&episode_dir(&s, id).join("transcript.json")).await.unwrap_or_default())
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
    fn durations() {
        assert_eq!(parse_duration("1:00:01"), 3601.0);
        assert_eq!(parse_duration("90"), 90.0);
        assert_eq!(parse_duration("2:30"), 150.0);
    }
}
