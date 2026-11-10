//! Where the briefing's stories come from: feeds (RSS/Atom), Reddit, Hacker News, YouTube, and
//! X / Instagram through RSSHub. Then, for the stories picked, the full text: the article, the
//! post and its best comments, or what's said in a video.

use std::time::Duration;

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use duet_common::podcast::{PodcastSettings, PodcastSource};
use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};

use crate::State;

/// One thing found in a feed.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Item {
    /// Where it was found: "Reddit · r/worldnews", "X · @someone", "The Verge"...
    pub source: String,
    pub title: String,
    /// What it's about (the article, video or post).
    pub url: String,
    /// The conversation about it (a Reddit thread, Hacker News comments), when that's elsewhere.
    pub discussion: Option<String>,
    /// The post's own words or the feed's summary.
    pub text: String,
    pub score: Option<i64>,
    pub comments: Option<i64>,
    pub published: Option<DateTime<Utc>>,
    pub image: Option<String>,
    pub video: bool,
}

/// What a source is called in the list and in the script.
pub fn label(s: &PodcastSource) -> String {
    let v = s.value.trim();
    match s.kind.as_str() {
        "reddit" if v.starts_with("http") => "Reddit (your front page)".into(),
        "reddit" => format!("Reddit · r/{}", v.trim_start_matches("r/").trim_start_matches("/r/")),
        "x" if v == "home" => "X (your timeline)".into(),
        "x" => format!("X · @{}", v.trim_start_matches('@')),
        "instagram" => format!("Instagram · @{}", v.trim_start_matches('@')),
        "youtube" => format!("YouTube · {v}"),
        "hackernews" => "Hacker News".into(),
        "lobsters" => "Lobsters".into(),
        "bluesky" => format!("Bluesky · @{}", v.trim_start_matches('@')),
        "mastodon" => format!("Mastodon · {v}"),
        _ => duet_common::podcast::CATALOG.iter().find(|c| c.value == v).map(|c| c.name.to_string()).or_else(|| host_of(v)).unwrap_or_else(|| v.to_string()),
    }
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let host = rest.split(['/', '?', '#']).next()?;
    Some(host.trim_start_matches("www.").to_string())
}

/// How many items each source may put forward.
const PER_SOURCE: usize = 12;

/// Everything recent from every source the listener switched on, newest first. A source that
/// fails is skipped (and said so in the log), not the whole briefing.
pub async fn gather(state: &State, settings: &PodcastSettings) -> (Vec<Item>, Vec<String>) {
    let client = state.site_client().await;
    let jobs = settings.sources.iter().filter(|s| s.enabled && !s.value.trim().is_empty()).map(|s| {
        let client = client.clone();
        let rsshub = settings.rsshub_url.clone();
        let s = s.clone();
        async move {
            let r = tokio::time::timeout(Duration::from_secs(45), fetch_source(&client, &rsshub, &s)).await;
            (s, r.unwrap_or_else(|_| Err(anyhow::anyhow!("timed out"))))
        }
    });
    let mut items = Vec::new();
    let mut problems = Vec::new();
    for (s, r) in futures_util::future::join_all(jobs).await {
        match r {
            Ok(mut found) => {
                // The best of each source, so a busy one doesn't drown the rest.
                found.sort_by_key(|i| (std::cmp::Reverse(i.score.unwrap_or(0)), std::cmp::Reverse(i.published)));
                found.truncate(PER_SOURCE);
                items.extend(found)
            }
            Err(e) => {
                tracing::warn!("briefing source {}: {e:#}", label(&s));
                problems.push(format!("{}: {e:#}", label(&s)));
            }
        }
    }
    // Only what's new: the last day and a half (items without a date are kept).
    let cutoff = Utc::now() - chrono::Duration::hours(36);
    items.retain(|i| i.published.is_none_or(|p| p > cutoff));
    let mut seen = std::collections::HashSet::new();
    items.retain(|i| seen.insert(normal_url(&i.url)));
    items.sort_by_key(|i| std::cmp::Reverse(i.published));
    (items, problems)
}

/// For telling the same link apart from its tracking-parameter twins.
pub fn normal_url(url: &str) -> String {
    let u = url.split('#').next().unwrap_or(url);
    let u = match u.split_once('?') {
        Some((base, q)) => {
            let kept: Vec<&str> = q.split('&').filter(|p| !p.starts_with("utm_") && !p.starts_with("ref")).collect();
            if kept.is_empty() { base.to_string() } else { format!("{base}?{}", kept.join("&")) }
        }
        None => u.to_string(),
    };
    u.trim_end_matches('/').replace("://www.", "://").replace("http://", "https://")
}

async fn fetch_source(client: &reqwest::Client, rsshub: &str, s: &PodcastSource) -> anyhow::Result<Vec<Item>> {
    let v = s.value.trim();
    let name = label(s);
    let hub = || -> anyhow::Result<String> {
        let h = rsshub.trim().trim_end_matches('/');
        if h.is_empty() {
            bail!("X and Instagram need an RSSHub address (Briefing → Sources)");
        }
        Ok(h.to_string())
    };
    match s.kind.as_str() {
        "reddit" if !v.starts_with("http") => reddit(client, v.trim_start_matches("/r/").trim_start_matches("r/")).await,
        "reddit" => feed(client, v, &name).await,
        "hackernews" => {
            let which = if v.is_empty() { "best" } else { v };
            feed(client, &format!("https://hnrss.org/{which}?count=40"), &name).await
        }
        "lobsters" => {
            let path = match v {
                "" | "hottest" => "hottest".to_string(),
                "newest" | "active" => v.to_string(),
                tag => format!("t/{}", tag.trim_start_matches("t/")),
            };
            feed(client, &format!("https://lobste.rs/{path}.rss"), &name).await
        }
        "bluesky" => {
            let handle = v.trim_start_matches('@').trim_start_matches("https://bsky.app/profile/");
            feed(client, &format!("https://bsky.app/profile/{}/rss", handle.trim_end_matches('/')), &name).await
        }
        "mastodon" => feed(client, &mastodon_feed(v)?, &name).await,
        "x" => {
            let path = match v {
                "home" => "/twitter/home".to_string(),
                p if p.starts_with('/') => p.to_string(),
                p if p.starts_with("list/") => format!("/twitter/{p}"),
                p => format!("/twitter/user/{}", p.trim_start_matches('@')),
            };
            feed(client, &format!("{}{path}", hub()?), &name).await
        }
        "instagram" => {
            let path = if v.starts_with('/') { v.to_string() } else { format!("/picnob/user/{}", v.trim_start_matches('@')) };
            let mut items = feed(client, &format!("{}{path}", hub()?), &name).await?;
            for i in &mut items {
                i.video = true;
            }
            Ok(items)
        }
        "youtube" => {
            let url = youtube_feed(client, v).await?;
            let mut items = feed(client, &url, &name).await?;
            for i in &mut items {
                i.video = true;
            }
            Ok(items)
        }
        _ => feed(client, v, &name).await,
    }
}

/// "@user@instance", "#tag@instance" or a profile address, as its feed.
fn mastodon_feed(v: &str) -> anyhow::Result<String> {
    if v.starts_with("http") {
        return Ok(if v.ends_with(".rss") { v.to_string() } else { format!("{}.rss", v.trim_end_matches('/')) });
    }
    let (who, instance) = v.trim_start_matches('@').rsplit_once('@').context("write it as @name@instance or #tag@instance")?;
    Ok(match who.strip_prefix('#') {
        Some(tag) => format!("https://{instance}/tags/{tag}.rss"),
        None => format!("https://{instance}/@{who}.rss"),
    })
}

/// A subreddit's top posts of the day, with their scores.
async fn reddit(client: &reqwest::Client, sub: &str) -> anyhow::Result<Vec<Item>> {
    let url = format!("https://www.reddit.com/r/{sub}/top.json?t=day&limit=30&raw_json=1");
    let resp = client.get(&url).timeout(Duration::from_secs(20)).send().await?;
    if !resp.status().is_success() {
        // Reddit sometimes refuses its JSON to strangers but still serves the feed.
        return feed(client, &format!("https://www.reddit.com/r/{sub}/top/.rss?t=day"), &format!("Reddit · r/{sub}")).await;
    }
    let v: serde_json::Value = resp.json().await.context("Reddit sent something unexpected")?;
    let mut out = Vec::new();
    for c in v["data"]["children"].as_array().into_iter().flatten() {
        let d = &c["data"];
        if d["stickied"] == true || d["over_18"] == true {
            continue;
        }
        let permalink = format!("https://www.reddit.com{}", d["permalink"].as_str().unwrap_or(""));
        let url = d["url_overridden_by_dest"].as_str().or(d["url"].as_str()).unwrap_or(&permalink).to_string();
        let image = d["preview"]["images"][0]["source"]["url"].as_str().map(str::to_string);
        out.push(Item {
            source: format!("Reddit · r/{sub}"),
            title: d["title"].as_str().unwrap_or("").to_string(),
            video: d["is_video"] == true || is_video_url(&url),
            url,
            discussion: Some(permalink),
            text: clip(d["selftext"].as_str().unwrap_or(""), 600),
            score: d["score"].as_i64(),
            comments: d["num_comments"].as_i64(),
            published: d["created_utc"].as_f64().and_then(|t| DateTime::from_timestamp(t as i64, 0)),
            image,
        });
    }
    Ok(out)
}

/// A YouTube channel's feed address, from its id, address or @handle.
async fn youtube_feed(client: &reqwest::Client, v: &str) -> anyhow::Result<String> {
    if v.contains("feeds/videos.xml") {
        return Ok(v.to_string());
    }
    if v.starts_with("UC") && !v.contains('/') {
        return Ok(format!("https://www.youtube.com/feeds/videos.xml?channel_id={v}"));
    }
    let page = if v.starts_with("http") { v.to_string() } else { format!("https://www.youtube.com/{}", if v.starts_with('@') { v.to_string() } else { format!("@{v}") }) };
    let html = client.get(&page).timeout(Duration::from_secs(20)).send().await?.text().await?;
    let id = html
        .split("\"channelId\":\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .or_else(|| html.split("channel_id=").nth(1).and_then(|r| r.split(['"', '&']).next()))
        .context("couldn't find that YouTube channel")?;
    Ok(format!("https://www.youtube.com/feeds/videos.xml?channel_id={id}"))
}

/// Any RSS or Atom feed.
async fn feed(client: &reqwest::Client, url: &str, source: &str) -> anyhow::Result<Vec<Item>> {
    let resp = client.get(url).timeout(Duration::from_secs(25)).send().await.context("unreachable")?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let xml = resp.text().await?;
    let mut items = parse_feed(&xml);
    let site = channel_title(&xml);
    for i in &mut items {
        i.source = match (&site, source) {
            (Some(t), s) if !s.contains('·') && !["Hacker News", "Reddit", "Lobsters"].iter().any(|n| s.contains(n)) => t.clone(),
            _ => source.to_string(),
        };
        i.video = i.video || is_video_url(&i.url);
        // Hacker News feeds give the points and the thread in the summary.
        if source == "Hacker News" {
            if let Some(p) = i.text.split("Points: ").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse().ok()) {
                i.score = Some(p);
            }
            if let Some(c) = i.text.split("Comments URL: ").nth(1).and_then(|r| r.split_whitespace().next()) {
                i.discussion = Some(c.to_string());
            }
            if let Some(n) = i.text.split("# Comments: ").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse().ok()) {
                i.comments = Some(n);
            }
            i.text.clear();
        }
    }
    Ok(items)
}

fn channel_title(xml: &str) -> Option<String> {
    let start = xml.find("<title")?;
    let rest = &xml[start..];
    let body = &rest[rest.find('>')? + 1..];
    let t = strip_html(&body[..body.find("</title>")?]);
    (!t.is_empty()).then_some(t)
}

pub fn is_video_url(url: &str) -> bool {
    ["youtube.com/watch", "youtu.be/", "youtube.com/shorts", "v.redd.it", "instagram.com/reel", "instagram.com/p/", "tiktok.com/", "vimeo.com/"]
        .iter()
        .any(|p| url.contains(p))
}

/// Items from RSS 2.0, RSS 1.0 or Atom.
fn parse_feed(xml: &str) -> Vec<Item> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out = Vec::new();
    let mut cur: Option<Item> = None;
    let mut field = String::new();
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" || name == "entry" {
                    cur = Some(Item::default());
                } else if let Some(item) = cur.as_mut() {
                    if name == "link" {
                        if let Some(href) = attr(&e, "href") {
                            if attr(&e, "rel").is_none_or(|r| r == "alternate") && item.url.is_empty() {
                                item.url = href;
                            }
                        }
                    }
                    if matches!(name.as_str(), "media:thumbnail" | "media:content") && item.image.is_none() {
                        item.image = attr(&e, "url");
                    }
                    field = name;
                    text.clear();
                }
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if let Some(item) = cur.as_mut() {
                    match name.as_str() {
                        "link" => {
                            if let Some(href) = attr(&e, "href") {
                                if attr(&e, "rel").is_none_or(|r| r == "alternate") && item.url.is_empty() {
                                    item.url = href;
                                }
                            }
                        }
                        "media:thumbnail" | "media:content" | "enclosure" if item.image.is_none() => {
                            let t = attr(&e, "type").unwrap_or_default();
                            if name != "enclosure" || t.starts_with("image") {
                                item.image = attr(&e, "url");
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if cur.is_some() {
                    text.push_str(&t.decode().map(|c| c.to_string()).unwrap_or_default());
                    // Entities (&amp;) come as their own events in recent quick-xml.
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if cur.is_some() {
                    let name = String::from_utf8_lossy(&r).to_string();
                    text.push_str(&decode_entity(&name));
                }
            }
            Ok(Event::CData(t)) => {
                if cur.is_some() {
                    text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" || name == "entry" {
                    if let Some(i) = cur.take() {
                        if !i.title.is_empty() || !i.text.is_empty() {
                            out.push(i);
                        }
                    }
                } else if let Some(item) = cur.as_mut() {
                    if name == field {
                        let value = text.trim().to_string();
                        match name.as_str() {
                            "title" => item.title = strip_html(&value),
                            "link" if item.url.is_empty() => item.url = value,
                            "description" | "summary" | "content" | "content:encoded" | "media:description" => {
                                let plain = strip_html(&value);
                                if plain.len() > item.text.len() {
                                    if item.image.is_none() {
                                        item.image = first_image(&value);
                                    }
                                    item.text = clip(&plain, 1500);
                                }
                            }
                            "pubDate" | "published" | "updated" | "dc:date" if item.published.is_none() => {
                                item.published = DateTime::parse_from_rfc2822(&value)
                                    .or_else(|_| DateTime::parse_from_rfc3339(&value))
                                    .ok()
                                    .map(|d| d.with_timezone(&Utc));
                            }
                            "comments" => item.discussion = Some(value),
                            _ => {}
                        }
                        field.clear();
                    }
                    text.clear();
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    // Posts with no title (tweets) are titled by their first words.
    for i in &mut out {
        if i.title.is_empty() {
            i.title = clip(&i.text, 120);
        }
    }
    out
}

fn attr(e: &quick_xml::events::BytesStart, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key.as_bytes())
        .and_then(|a| a.unescape_value().ok().map(|v| v.to_string()))
}

fn decode_entity(name: &str) -> String {
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

fn first_image(html: &str) -> Option<String> {
    let i = html.find("<img")?;
    let rest = &html[i..];
    let src = rest.split("src=\"").nth(1)?.split('"').next()?;
    src.starts_with("http").then(|| src.replace("&amp;", "&"))
}

/// Text from a bit of HTML: tags out, entities decoded, whitespace tidied.
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            '&' if !in_tag => {
                let mut name = String::new();
                while let Some(&n) = chars.peek() {
                    if n == ';' || name.len() > 8 {
                        break;
                    }
                    name.push(n);
                    chars.next();
                }
                if chars.peek() == Some(&';') {
                    chars.next();
                    let d = decode_entity(&name);
                    if d.is_empty() && name == "nbsp" { out.push(' ') } else { out.push_str(&d) }
                } else {
                    out.push('&');
                    out.push_str(&name);
                }
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `n` bytes, cut at a word.
pub fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let cut = s[..end].rfind(' ').unwrap_or(end);
    format!("{}…", &s[..cut])
}

/// What the hosts read before they talk about a story: the article itself, the post and the
/// best of its comments, what's said in the video.
pub struct Reading {
    pub text: String,
    pub image: Option<String>,
    pub site: Option<String>,
}

pub async fn read(state: &State, item: &Item, watch_videos: bool) -> Reading {
    let client = state.site_client().await;
    let mut parts = vec![format!("{} ({})", item.title, item.source)];
    if !item.text.is_empty() {
        parts.push(item.text.clone());
    }
    let mut image = item.image.clone();
    let mut site = None;
    let discussion = item.discussion.clone().unwrap_or_default();
    // The thread.
    if discussion.contains("reddit.com/") || item.url.contains("reddit.com/r/") {
        let thread = if discussion.is_empty() { &item.url } else { &discussion };
        match reddit_thread(&client, thread).await {
            Ok(t) => parts.push(t),
            Err(e) => tracing::debug!("reddit thread {thread}: {e:#}"),
        }
    } else if discussion.contains("lobste.rs/s/") {
        match lobsters_thread(&client, &discussion).await {
            Ok(t) => parts.push(t),
            Err(e) => tracing::debug!("lobsters thread {discussion}: {e:#}"),
        }
    } else if let Some(id) = discussion.split("item?id=").nth(1) {
        match hn_thread(&client, id).await {
            Ok(t) => parts.push(t),
            Err(e) => tracing::debug!("hn thread {id}: {e:#}"),
        }
    }
    // The thing itself.
    let own = item.url.contains("reddit.com/r/") || item.url.contains("x.com/") || item.url.contains("twitter.com/");
    if item.video && watch_videos {
        match watch(state, &item.url).await {
            Ok(t) if !t.is_empty() => parts.push(format!("What's said in the video:\n{t}")),
            Ok(_) => {}
            Err(e) => tracing::info!("listening to {}: {e:#}", item.url),
        }
    } else if !own && item.url.starts_with("http") {
        match article(&client, &item.url).await {
            Ok(a) => {
                image = image.or(a.image);
                site = a.site;
                parts.push(a.text);
            }
            Err(e) => tracing::debug!("article {}: {e:#}", item.url),
        }
    }
    Reading { text: clip(&parts.join("\n\n"), 14000), image, site }
}

async fn reddit_thread(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let base = url.split('?').next().unwrap_or(url).trim_end_matches('/');
    let v: serde_json::Value = client
        .get(format!("{base}.json?limit=15&sort=top&raw_json=1"))
        .timeout(Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let post = &v[0]["data"]["children"][0]["data"];
    let mut out = String::new();
    if let Some(t) = post["selftext"].as_str().filter(|t| !t.is_empty()) {
        out.push_str(&format!("The post: {}\n", clip(t, 4000)));
    }
    out.push_str("Top comments:\n");
    for c in v[1]["data"]["children"].as_array().into_iter().flatten().take(10) {
        let d = &c["data"];
        if let Some(body) = d["body"].as_str() {
            out.push_str(&format!("- ({} points) {}\n", d["score"].as_i64().unwrap_or(0), clip(body, 500)));
        }
    }
    Ok(out)
}

async fn lobsters_thread(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let base = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/');
    let v: serde_json::Value = client.get(format!("{base}.json")).timeout(Duration::from_secs(20)).send().await?.error_for_status()?.json().await?;
    let mut out = String::new();
    if let Some(t) = v["description_plain"].as_str().filter(|t| !t.is_empty()) {
        out.push_str(&format!("The post: {}\n", clip(t, 3000)));
    }
    out.push_str("Comments on Lobsters:\n");
    let mut comments: Vec<&serde_json::Value> = v["comments"].as_array().into_iter().flatten().filter(|c| c["depth"].as_i64().unwrap_or(0) == 0).collect();
    comments.sort_by_key(|c| std::cmp::Reverse(c["score"].as_i64().unwrap_or(0)));
    for c in comments.into_iter().take(8) {
        if let Some(t) = c["comment_plain"].as_str() {
            out.push_str(&format!("- {}\n", clip(t, 500)));
        }
    }
    Ok(out)
}

async fn hn_thread(client: &reqwest::Client, id: &str) -> anyhow::Result<String> {
    let v: serde_json::Value = client
        .get(format!("https://hn.algolia.com/api/v1/items/{}", id.trim()))
        .timeout(Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mut out = String::new();
    if let Some(t) = v["text"].as_str().filter(|t| !t.is_empty()) {
        out.push_str(&format!("The post: {}\n", clip(&strip_html(t), 3000)));
    }
    out.push_str("Comments on Hacker News:\n");
    for c in v["children"].as_array().into_iter().flatten().take(8) {
        if let Some(t) = c["text"].as_str() {
            out.push_str(&format!("- {}\n", clip(&strip_html(t), 500)));
        }
    }
    Ok(out)
}

pub struct Article {
    pub text: String,
    pub image: Option<String>,
    pub site: Option<String>,
}

/// The readable part of a web page (Mozilla's Readability, ported).
pub async fn article(client: &reqwest::Client, url: &str) -> anyhow::Result<Article> {
    let resp = client.get(url).timeout(Duration::from_secs(20)).send().await?.error_for_status()?;
    let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if !ct.is_empty() && !ct.contains("html") {
        bail!("not a web page ({ct})");
    }
    let bytes = resp.bytes().await?;
    let html = String::from_utf8_lossy(&bytes[..bytes.len().min(4_000_000)]).to_string();
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let mut r = dom_smoothie::Readability::new(html, Some(&url), None).map_err(|e| anyhow::anyhow!("{e}"))?;
        let a = r.parse().map_err(|e| anyhow::anyhow!("{e}"))?;
        let text = a.text_content.to_string();
        let text = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
        Ok(Article { text: clip(&text, 9000), image: a.image, site: a.site_name })
    })
    .await?
}

/// What's said in a video or reel: the first ten minutes of its sound, transcribed.
async fn watch(state: &State, url: &str) -> anyhow::Result<String> {
    let dir = state.data_dir.join("podcast").join("tmp");
    tokio::fs::create_dir_all(&dir).await?;
    let stem = format!("v{}", Utc::now().timestamp_micros());
    let out = tokio::time::timeout(
        Duration::from_secs(240),
        tokio::process::Command::new("yt-dlp")
            .args(["-q", "--no-playlist", "-f", "bestaudio/best", "-x", "--audio-format", "mp3", "--audio-quality", "7"])
            .args(["--download-sections", "*0-600", "-o"])
            .arg(dir.join(format!("{stem}.%(ext)s")))
            .arg(url)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("the video took too long to fetch")?
    .context("yt-dlp isn't installed (it's needed to listen to videos)")?;
    let file = dir.join(format!("{stem}.mp3"));
    if !out.status.success() || !file.exists() {
        bail!("yt-dlp: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("failed"));
    }
    let audio = tokio::fs::read(&file).await?;
    let _ = tokio::fs::remove_file(&file).await;
    let text = crate::ai::transcribe(state, audio, format!("{stem}.mp3"), "audio/mpeg".into()).await?;
    Ok(clip(&text, 8000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rss_and_atom() {
        let rss = r#"<?xml version="1.0"?><rss><channel><title>Example News</title>
            <item><title>Big &amp; news</title><link>https://ex.com/a?utm_source=x</link>
            <description><![CDATA[<p>Hello <b>world</b></p><img src="https://ex.com/i.jpg">]]></description>
            <pubDate>Mon, 09 Nov 2026 10:00:00 +0000</pubDate></item></channel></rss>"#;
        let items = parse_feed(rss);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Big & news");
        assert_eq!(items[0].text, "Hello world");
        assert_eq!(items[0].image.as_deref(), Some("https://ex.com/i.jpg"));
        assert!(items[0].published.is_some());
        assert_eq!(normal_url(&items[0].url), "https://ex.com/a");
        assert_eq!(channel_title(rss).as_deref(), Some("Example News"));

        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>YT</title><entry><title>Video</title>
            <link rel="alternate" href="https://www.youtube.com/watch?v=abc"/><published>2026-11-09T10:00:00+00:00</published>
            <media:group><media:thumbnail url="https://i.ytimg.com/x.jpg"/><media:description>About it</media:description></media:group></entry></feed>"#;
        let items = parse_feed(atom);
        assert_eq!(items[0].url, "https://www.youtube.com/watch?v=abc");
        assert_eq!(items[0].image.as_deref(), Some("https://i.ytimg.com/x.jpg"));
        assert_eq!(items[0].text, "About it");
        assert!(is_video_url(&items[0].url));
    }
}
