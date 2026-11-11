//! The daily briefing: every morning (or whenever you ask) the chat model reads what's new in
//! your feeds, picks the stories worth your time, and writes a conversation about them for two
//! AI hosts, which the local voices perform line by line. Playing starts as soon as the first
//! line is recorded; the rest is recorded faster than it plays. You can stop and ask about
//! what you're hearing, and the host answers (looking things up on the web when the model can).
//!
//! Everything lives under `data/podcast/`: `settings.json`, and one folder per episode with
//! `episode.json`, `notes.json` (what the hosts read, for questions) and a recording per line.

mod browser;
mod feeds;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, LazyLock},
    time::Duration,
};

use anyhow::{Context, bail};
use axum::{
    Json, Router,
    extract::{Path, State as AxState},
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use duet_common::podcast::{
    Episode, EpisodeSummary, HostVoice, Line, PodcastAnswer, PodcastAsk, PodcastSettings, Segment, SourceLink, strip_directions,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::{
    State,
    error::{ApiError, ApiResult},
};

pub fn routes() -> Router<State> {
    Router::new()
        .route("/podcast/settings", get(get_settings).put(put_settings))
        .route("/podcast/episodes", get(list).post(start))
        .route("/podcast/episodes/{id}", get(episode).delete(remove))
        .route("/podcast/episodes/{id}/record", post(record_again))
        .route("/podcast/episodes/{id}/audio/{key}/{line}", get(line_audio))
        .route("/podcast/ask", post(ask))
        .route("/podcast/voices", get(voices))
        .route("/podcast/voices/recast", post(recast))
        .route("/podcast/voices/{name}/clip", get(clip))
        // (A voice recording can be a few megabytes.)
        .route("/podcast/voices/{name}/sample", post(upload_sample).layer(axum::extract::DefaultBodyLimit::max(64 << 20)))
        .route("/podcast/warm", post(warm))
        .route("/podcast/browser/login", post(browser_login))
        .route("/podcast/browser/check", post(browser_check))
}

fn base(state: &State) -> PathBuf {
    state.data_dir.join("podcast")
}

fn episode_dir(state: &State, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'), "no such episode");
    Ok(base(state).join("episodes").join(id))
}

async fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    serde_json::from_slice(&tokio::fs::read(path).await.ok()?).ok()
}

async fn write_json<T: serde::Serialize>(path: &std::path::Path, v: &T) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(v)?).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

pub async fn settings(state: &State) -> PodcastSettings {
    let path = base(state).join("settings.json");
    let Some(mut s) = read_json::<PodcastSettings>(&path).await else {
        return PodcastSettings::default();
    };
    if s.upgrade() {
        let _ = write_json(&path, &s).await;
    }
    s
}

/// The voice server's name for a host's voice (its clip is `voices/<name>.wav`).
fn voice_name(host: &str) -> String {
    let slug: String = host.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    format!("podcast-{}", slug.trim_matches('-'))
}

// ---- Episodes being made ----

/// Episodes being made right now, as the player should see them.
static MAKING: LazyLock<std::sync::Mutex<HashMap<String, Arc<Mutex<Episode>>>>> = LazyLock::new(Default::default);

fn making(id: &str) -> Option<Arc<Mutex<Episode>>> {
    MAKING.lock().unwrap().get(id).cloned()
}

async fn load(state: &State, id: &str) -> anyhow::Result<Episode> {
    if let Some(ep) = making(id) {
        return Ok(ep.lock().await.clone());
    }
    let mut ep: Episode = read_json(&episode_dir(state, id)?.join("episode.json")).await.context("no such episode")?;
    if !ep.finished() {
        // Nothing is making it any more (duet restarted part-way): say so, and keep only the
        // parts that got written, so "Record again" can finish recording those.
        ep.stage = "failed".into();
        ep.doing.clear();
        ep.error = Some("Interrupted (duet restarted while making it)".into());
        ep.segments.retain(|s| !s.writing && !s.lines.is_empty());
    }
    Ok(ep)
}

async fn save(state: &State, ep: &Episode) -> anyhow::Result<()> {
    write_json(&episode_dir(state, &ep.id)?.join("episode.json"), ep).await
}

/// Changes the live episode and writes it out.
async fn update(state: &State, ep: &Arc<Mutex<Episode>>, f: impl FnOnce(&mut Episode)) {
    let snapshot = {
        let mut e = ep.lock().await;
        f(&mut e);
        e.clone()
    };
    if let Err(e) = save(state, &snapshot).await {
        tracing::warn!("saving the briefing: {e:#}");
    }
}

/// Starts a new episode in the background and returns it as it begins.
pub async fn begin(state: &State) -> anyhow::Result<Episode> {
    if let Some(ep) = MAKING.lock().unwrap().values().next().cloned() {
        // One at a time: the voices can only record one thing at once anyway.
        if let Ok(e) = ep.try_lock() {
            return Ok(e.clone());
        }
    }
    let s = settings(state).await;
    if !s.sources.iter().any(|s| s.enabled && !s.value.trim().is_empty()) {
        bail!("Add some sources first");
    }
    let now = chrono::Local::now();
    let ep = Episode {
        id: now.format("%Y-%m-%d-%H%M%S").to_string(),
        title: format!("{}", now.format("%A, %B %-d")),
        created: chrono::Utc::now().to_rfc3339(),
        stage: "gathering".into(),
        doing: "Checking your sources".into(),
        hosts: s.hosts.iter().map(|h| h.name.clone()).collect(),
        ..Default::default()
    };
    // Live before it's on disk, so it never reads back as an interrupted one.
    let live = Arc::new(Mutex::new(ep.clone()));
    MAKING.lock().unwrap().insert(ep.id.clone(), live.clone());
    if let Err(e) = save(state, &ep).await {
        MAKING.lock().unwrap().remove(&ep.id);
        return Err(e);
    }
    let st = state.clone();
    tokio::spawn(async move {
        let id = live.lock().await.id.clone();
        if let Err(e) = make(&st, &live, &s).await {
            tracing::warn!("briefing {id}: {e:#}");
            update(&st, &live, |ep| {
                ep.stage = "failed".into();
                ep.error = Some(format!("{e:#}"));
                ep.doing.clear();
            })
            .await;
        }
        MAKING.lock().unwrap().remove(&id);
    });
    Ok(ep)
}

/// A story picked for the episode.
#[derive(Deserialize)]
struct Pick {
    title: String,
    #[serde(default)]
    items: Vec<usize>,
    /// "roundup": a run through the best posts and comments (your X feed today); otherwise a
    /// deep dive.
    #[serde(default)]
    kind: String,
}

impl Pick {
    fn roundup(&self) -> bool {
        self.kind.eq_ignore_ascii_case("roundup")
    }
}

#[derive(Deserialize)]
struct Picks {
    #[serde(default)]
    title: String,
    stories: Vec<Pick>,
}

async fn make(state: &State, ep: &Arc<Mutex<Episode>>, s: &PodcastSettings) -> anyhow::Result<()> {
    anyhow::ensure!(!s.hosts.is_empty(), "Add a host (Briefing → Hosts)");
    let id = ep.lock().await.id.clone();
    let dir = episode_dir(state, &id)?;
    // The hosts' voices are made first if they're new (that briefly needs the whole graphics
    // card), while the feeds are read.
    let cast = {
        let st = state.clone();
        let hosts = s.hosts.clone();
        tokio::spawn(async move { cast_voices(&st, &hosts, false).await })
    };

    // Fish takes minutes to load and compile: start it now, while the feeds are read and the
    // script written, so the first line doesn't wait for it. (Not while a new host's voice is
    // being made: that needs the whole card.)
    crate::cast::cast_open();
    let voices_dir = state.data_dir.join("voices");
    if s.hosts.iter().all(|h| h.engine == "fish" && voices_dir.join(format!("{}.wav", voice_name(&h.name))).exists()) {
        let st = state.clone();
        tokio::spawn(async move {
            let tts = st.settings.read().await.ai.tts_url.clone();
            let _ = st.http.post(format!("{}/warm", tts.trim_end_matches('/'))).json(&json!({})).timeout(Duration::from_secs(5)).send().await;
        });
    }
    let (mut items, problems) = feeds::gather(state, s).await;
    let used: HashSet<String> = read_json(&base(state).join("used.json")).await.unwrap_or_default();
    items.retain(|i| !used.contains(&feeds::normal_url(&i.url)));
    if items.is_empty() {
        if problems.is_empty() {
            bail!("Nothing new in your sources since the last episode");
        }
        bail!("Couldn't read your sources: {}", problems.join("; "));
    }
    items.truncate(400);

    // Choosing.
    // Deep dives, not headlines: a few stories, each given room.
    let stories = ((s.minutes.max(5) as f64) / 6.0).round().clamp(3.0, 10.0) as usize;
    update(state, ep, |e| {
        e.stage = "choosing".into();
        e.doing = format!("Choosing from {} posts and articles", items.len());
    })
    .await;
    let picks = choose(state, s, &items, stories).await?;
    anyhow::ensure!(!picks.stories.is_empty(), "the chat model didn't pick any stories");

    let mut segments = vec![Segment { key: "intro".into(), title: "Today's briefing".into(), kind: "intro".into(), writing: true, ..Default::default() }];
    let mut chosen: Vec<Vec<feeds::Item>> = Vec::new();
    let mut roundups: Vec<bool> = Vec::new();
    let mut titles: Vec<String> = Vec::new();
    for p in &picks.stories {
        let its: Vec<feeds::Item> = p.items.iter().filter_map(|&i| items.get(i).cloned()).take(if p.roundup() { 10 } else { 4 }).collect();
        if its.is_empty() {
            continue;
        }
        segments.push(Segment {
            key: format!("s{}", chosen.len() + 1),
            title: p.title.clone(),
            kind: "story".into(),
            writing: true,
            sources: its.iter().map(source_link).collect(),
            lines: Vec::new(),
        });
        chosen.push(its);
        roundups.push(p.roundup());
        titles.push(p.title.clone());
    }
    segments.push(Segment { key: "outro".into(), title: "Sign-off".into(), kind: "outro".into(), writing: true, ..Default::default() });
    let title = picks.title.trim().to_string();
    update(state, ep, |e| {
        if !title.is_empty() {
            e.title = format!("{} · {title}", e.title);
        }
        e.segments = segments.clone();
        e.stage = "reading".into();
        e.doing = format!("Reading {} stories", chosen.len());
    })
    .await;

    // Reading: the articles, threads and videos behind each story.
    let mut notes: Vec<String> = vec![String::new(); chosen.len()];
    let mut done = 0;
    let mut jobs = Vec::new();
    for (k, its) in chosen.clone().into_iter().enumerate() {
        let st = state.clone();
        let watch = s.watch_videos;
        jobs.push(async move {
            let mut text = Vec::new();
            let mut images = Vec::new();
            for it in &its {
                let r = feeds::read(&st, it, watch).await;
                text.push(r.text);
                images.push((r.image, r.site));
            }
            (k, text.join("\n\n---\n\n"), images)
        });
    }
    let mut reads = futures_util::stream::iter(jobs).buffer_unordered(4);
    while let Some((k, text, images)) = reads.next().await {
        notes[k] = text;
        done += 1;
        update(state, ep, |e| {
            let key = format!("s{}", k + 1);
            if let Some(seg) = e.segments.iter_mut().find(|s| s.key == key) {
                for (src, (img, site)) in seg.sources.iter_mut().zip(images) {
                    if src.image.is_none() {
                        src.image = img;
                    }
                    if let Some(site) = site.filter(|_| !src.site.contains('·')) {
                        src.site = site;
                    }
                }
            }
            e.doing = format!("Read {done} of {} stories", notes.len());
        })
        .await;
    }
    drop(reads);
    write_json(&dir.join("notes.json"), &notes).await?;
    let mut all_used = used;
    for its in &chosen {
        all_used.extend(its.iter().map(|i| feeds::normal_url(&i.url)));
    }
    // (Only the last few thousand: enough to never repeat a story.)
    let all_used: HashSet<String> = all_used.into_iter().take(5000).collect();
    write_json(&base(state).join("used.json"), &all_used).await?;

    // Writing and recording together: lines are recorded in order as their part is written.
    update(state, ep, |e| {
        e.stage = "writing".into();
        e.doing = "Writing the script".into();
    })
    .await;
    let _ = cast.await;
    let recorder = {
        let st = state.clone();
        let ep = ep.clone();
        let hosts = s.hosts.clone();
        tokio::spawn(async move { record(&st, &ep, &hosts, None).await })
    };
    let total = notes.len() + 2;
    let words = (s.minutes.max(5) as usize * 155) / notes.len().max(1);
    let mut jobs = Vec::new();
    for k in 0..total {
        let st = state.clone();
        let s = s.clone();
        let titles = titles.clone();
        let note = if k == 0 || k == total - 1 { String::new() } else { notes[k - 1].clone() };
        let roundup = k > 0 && k < total - 1 && roundups[k - 1];
        jobs.push(async move { (k, write(&st, &s, &titles, k, total, &note, words, roundup).await) });
    }
    let mut writes = futures_util::stream::iter(jobs).buffered(3);
    let mut written = 0;
    while let Some((k, lines)) = writes.next().await {
        written += 1;
        let lines = match lines {
            Ok(l) if !l.is_empty() => l,
            Ok(_) => vec![],
            Err(e) => {
                tracing::warn!("writing part {k} of the briefing: {e:#}");
                vec![]
            }
        };
        update(state, ep, |e| {
            let key = if k == 0 { "intro".to_string() } else if k == total - 1 { "outro".to_string() } else { format!("s{k}") };
            if let Some(seg) = e.segments.iter_mut().find(|s| s.key == key) {
                seg.lines = lines;
                seg.writing = false;
            }
            if written < total {
                e.doing = format!("Writing the script ({written} of {total} parts)");
            }
        })
        .await;
    }
    drop(writes);
    update(state, ep, |e| {
        e.stage = "recording".into();
        e.segments.retain(|s| !s.lines.is_empty());
    })
    .await;
    recorder.await??;
    update(state, ep, |e| {
        e.stage = "ready".into();
        e.doing.clear();
    })
    .await;
    Ok(())
}

fn source_link(i: &feeds::Item) -> SourceLink {
    let url = if i.url.starts_with("http") { i.url.clone() } else { i.discussion.clone().unwrap_or_default() };
    SourceLink { title: i.title.clone(), url, site: i.source.clone(), image: i.image.clone() }
}

/// Today as said aloud: "Wednesday, November 11th".
fn spoken_date() -> String {
    let now = chrono::Local::now();
    let d = now.format("%-d").to_string().parse::<u32>().unwrap_or(1);
    let suffix = match (d % 10, d % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{}, {} {d}{suffix}", now.format("%A"), now.format("%B"))
}

fn today() -> String {
    chrono::Local::now().format("%A, %B %-d, %Y").to_string()
}

fn hosts_blurb(s: &PodcastSettings) -> String {
    let who: Vec<String> = s.hosts.iter().map(|h| format!("{} ({})", h.name, h.voice)).collect();
    if s.hosts.len() == 1 {
        format!(
            "The host is {}, talking straight to the listener on their own: a lively, personal monologue, like a \
             favourite radio host catching a friend up. Every line's speaker is the host.",
            who[0]
        )
    } else {
        format!("The hosts are {}. They talk with each other and the listener like old friends who've read everything.", who.join(" and "))
    }
}

/// Pulls the JSON out of a model's answer (it may wrap it in a code fence or a sentence).
fn json_in<T: serde::de::DeserializeOwned>(text: &str, open: char, close: char) -> anyhow::Result<T> {
    let end = text.rfind(close).context("the chat model didn't answer in JSON")?;
    // Notes from research can come first, with brackets of their own: try each opening in turn.
    let mut last = None;
    for (start, _) in text[..end].match_indices(open) {
        match serde_json::from_str(&text[start..=end]) {
            Ok(v) => return Ok(v),
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) => Err(anyhow::Error::from(e)).context("the chat model's JSON didn't parse"),
        None => bail!("the chat model didn't answer in JSON"),
    }
}

async fn choose(state: &State, s: &PodcastSettings, items: &[feeds::Item], stories: usize) -> anyhow::Result<Picks> {
    let mut list = String::new();
    for (k, i) in items.iter().enumerate() {
        let mut meta = vec![i.source.clone()];
        if let Some(sc) = i.score {
            meta.push(format!("{sc} points"));
        }
        if let Some(c) = i.comments {
            meta.push(format!("{c} comments"));
        }
        if i.video {
            meta.push("video".into());
        }
        list.push_str(&format!("[{k}] {} ({})", i.title, meta.join(", ")));
        if !i.text.is_empty() {
            list.push_str(&format!(" — {}", feeds::clip(&i.text, 160)));
        }
        list.push('\n');
    }
    let interests = if s.interests.trim().is_empty() { "Not said yet: go for what's most important and most interesting.".into() } else { s.interests.clone() };
    let system = format!(
        "You're the producer of a personal daily news podcast for one listener, made by AI. Today is {}. \
         The show is deep dives, not a headline read: from the posts and articles in the listener's own feeds, pick the \
         {stories} stories most worth really digging into this morning: ones with substance, a story behind them, or \
         something genuinely cool, that this listener would love, in a mix across their interests (not all one topic). \
         Group items about the same story together. Skip ads, self-promotion, duplicates, and thin news with nothing more to say. \
         Routine world news and political headlines are boring to this listener: leave them out, unless one is \
         genuinely huge, and then at most one.\n\n\
         Also, if there are posts from the listener's X timeline (source \"X (your timeline)\") or Reddit with interesting \
         takes, add one extra \"roundup\" story, placed second, titled like \"What's on your X feed today\": up to ten of \
         the most interesting posts and insightful comments, to be read out and reacted to.\n\n\
         What the listener cares about: {interests}\n\n\
         Answer with JSON only, in running order (lead with the strongest story, end on something fun): \
         {{\"title\": \"a short, catchy name for today's episode\", \"stories\": [{{\"title\": \"a short spoken headline\", \"kind\": \"deep\" or \"roundup\", \"items\": [numbers of the items]}}]}}",
        today()
    );
    let answer = crate::llm::chat(state, &system, &[("user".into(), list)]).await?;
    json_in(&answer, '{', '}')
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(default)]
    speaker: String,
    #[serde(default)]
    text: String,
}

/// The script for one part: the intro (k = 0), a story, or the sign-off (the last).
#[allow(clippy::too_many_arguments)]
async fn write(state: &State, s: &PodcastSettings, titles: &[String], k: usize, total: usize, notes: &str, words: usize, roundup: bool) -> anyhow::Result<Vec<Line>> {
    let names: Vec<String> = s.hosts.iter().map(|h| h.name.clone()).collect();
    let interests = if s.interests.trim().is_empty() { String::new() } else { format!("\nThe listener cares about: {}", s.interests) };
    let system = format!(
        "You write a personal daily podcast of deep dives for one listener, made and voiced by AI from their own feeds. Today is {}. \
         {}{interests}\n\n\
         Write for the ear: natural, lively spoken English with contractions, short sentences, real reactions, \
         questions to each other, a joke where one fits, and plain explanations of anything technical. Never read out \
         addresses or say \"link\". Say where things come from (\"a post on r/worldnews\", \"according to The Verge\", \
         \"someone on X\"), and say plainly when something is a rumour or one person's unverified claim. Use only \
         facts from the material given or from your own research; never make up quotes, numbers or details.\n\n\
         Begin every line with an acting direction in square brackets for the voice actor, describing how it's said, \
         e.g. [amused], [genuinely surprised], [leaning in, conspiratorial], [warmly]. Keep the delivery varied and \
         expressive.\n\n\
         Answer with JSON only: an array of lines, [{{\"speaker\": one of {:?}, \"text\": \"[direction] what they say\"}}].",
        today(),
        hosts_blurb(s),
        names
    );
    let running: String = titles.iter().enumerate().map(|(i, t)| format!("{}. {t}\n", i + 1)).collect();
    let prompt = if k == 0 {
        format!(
            "Write the opening, about 90 words. Start with a greeting and today's date said in full, the way a person \
             says it (\"It's {}.\"). Then a quick, enticing taste of what's coming (the first three or four stories), \
             then straight into the first story's hand-off. Mention once, lightly, that this is AI putting together \
             their feeds.\n\nToday's running order:\n{running}",
            spoken_date()
        )
    } else if k == total - 1 {
        format!("Write the sign-off, about 50 words: a warm, quick goodbye that wishes them a good day (no summary of every story).\n\nToday's stories were:\n{running}")
    } else {
        let story = &titles[k - 1];
        let prev = if k >= 2 { format!("The story before was \"{}\" (don't greet the listener; pick up from there).", titles[k - 2]) } else { "This follows straight on from the opening.".into() };
        let next = titles.get(k).map(|t| format!("End with a quick, natural hand-off towards the next story, \"{t}\".")).unwrap_or_else(|| "This is the last story before the sign-off.".into());
        if roundup {
            format!(
                "Write part {k} of {}: \"{story}\", a run through the most interesting posts and comments from the \
                 listener's feeds today. About {words} words. {prev} {next}\n\n\
                 For each, say who posted it (by name or handle, said naturally) and read the post out, or its best part, \
                 word for word, then react: what's interesting, funny or insightful about it, and a line of context where \
                 it helps. Include insightful replies and comments. Keep it moving, like reading a friend the good bits.\n\n\
                 The posts:\n\"\"\"\n{notes}\n\"\"\"",
                total - 2
            )
        } else {
            format!(
                "Write story {k} of {}: \"{story}\", as a deep dive. About {words} words. {prev} {next}\n\n\
                 Don't just read the headline. First research it on the web: the background and history, how the thing \
                 actually works, who's involved, what came before, what experts and the sharpest commenters are saying, and \
                 what it could mean next. Then tell it as a story: what happened, the context that makes it interesting, \
                 the details a fan would love, the best insightful comments from the discussion (say who and where), and \
                 your own take. Facts from your research count as material; say where they come from.\n\n\
                 The material from the feeds:\n\"\"\"\n{notes}\n\"\"\"",
                total - 2
            )
        }
    };
    // Deep dives look things up; the rest is written from what's in hand.
    let answer = if k > 0 && k < total - 1 && !roundup {
        match crate::llm::chat_web(state, &system, &[("user".into(), prompt.clone())]).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("researching story {k}: {e:#}; writing it from the feeds alone");
                crate::llm::chat(state, &system, &[("user".into(), prompt)]).await?
            }
        }
    } else {
        crate::llm::chat(state, &system, &[("user".into(), prompt)]).await?
    };
    let raw: Vec<RawLine> = json_in(&answer, '[', ']')?;
    let mut out = Vec::new();
    for (n, l) in raw.into_iter().enumerate() {
        if l.text.trim().is_empty() {
            continue;
        }
        let speaker = names
            .iter()
            .find(|h| h.eq_ignore_ascii_case(l.speaker.trim()))
            .cloned()
            .unwrap_or_else(|| names[n % names.len()].clone());
        out.push(Line { speaker, text: l.text.trim().to_string(), ready: false });
    }
    Ok(out)
}

/// The recording a host's voice clip was cut from ("" for one made from a description).
async fn clip_sample(state: &State, host: &str) -> String {
    read_json::<serde_json::Value>(&state.data_dir.join("voices").join(format!("{}.json", voice_name(host))))
        .await
        .and_then(|m| m["sample"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Makes the hosts' voices: from their recording when they have one, otherwise from their
/// description (`replace`: again, though they exist). A voice whose recording has changed (or
/// gone) since its clip was made is made again.
async fn cast_voices(state: &State, hosts: &[duet_common::podcast::PodcastHost], replace: bool) -> anyhow::Result<()> {
    let voices_dir = state.data_dir.join("voices");
    let mut wanted = Vec::new();
    for h in hosts {
        let sample = h.sample.trim();
        let stale = !voices_dir.join(format!("{}.wav", voice_name(&h.name))).exists() || clip_sample(state, &h.name).await != sample;
        if replace || stale {
            wanted.push(json!({ "voice": voice_name(&h.name), "instructions": h.voice, "sample": sample, "replace": true }));
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let tts = state.settings.read().await.ai.tts_url.clone();
    state
        .http
        .post(format!("{}/voices", tts.trim_end_matches('/')))
        .json(&json!({ "voices": wanted }))
        .timeout(Duration::from_secs(1800))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")?
        .error_for_status()?;
    Ok(())
}

/// One line, performed in its host's voice.
async fn perform(state: &State, text: &str, host: &duet_common::podcast::PodcastHost) -> anyhow::Result<Vec<u8>> {
    // Fish has the card: the picture drawing waits rather than unloading it mid-episode (each
    // reload recompiles for minutes).
    crate::cast::cast_open();
    let ai = state.settings.read().await.ai.clone();
    let body = json!({ "model": ai.tts_model, "input": text, "voice": voice_name(&host.name), "instructions": host.voice, "engine": host.engine, "response_format": "mp3" });
    let mut last = None;
    for _ in 0..2 {
        match state
            .http
            .post(format!("{}/audio/speech", ai.tts_url.trim_end_matches('/')))
            .bearer_auth(&ai.llm_api_key)
            .json(&body)
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(r) => return Ok(r.bytes().await?.to_vec()),
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    Err(anyhow::Error::from(last.unwrap())).context("the voice server isn't answering (Settings → Voices)")
}

/// Records every line in order, waiting for each part to be written. Returns once everything
/// written has been recorded and nothing's still being written. `only`: just that part (a
/// detour); otherwise everything but the detours, which record themselves.
async fn record(state: &State, ep: &Arc<Mutex<Episode>>, hosts: &[duet_common::podcast::PodcastHost], only: Option<&str>) -> anyhow::Result<()> {
    let id = ep.lock().await.id.clone();
    let dir = episode_dir(state, &id)?;
    let mut failures = 0;
    loop {
        // The first line not yet recorded, unless a part before it is still being written.
        let (next, waiting) = {
            let e = ep.lock().await;
            let mut next = None;
            let mut waiting = false;
            for seg in e.segments.iter().filter(|s| match only {
                Some(k) => s.key == k,
                None => s.kind != "detour",
            }) {
                if seg.writing {
                    waiting = true;
                    break;
                }
                if let Some(n) = seg.lines.iter().position(|l| !l.ready) {
                    next = Some((seg.key.clone(), n, seg.lines[n].clone(), seg.title.clone()));
                    break;
                }
            }
            (next, waiting)
        };
        let Some((key, n, line, title)) = next else {
            if waiting {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            return Ok(());
        };
        {
            let mut e = ep.lock().await;
            if only.is_none() && (e.stage == "recording" || e.stage == "writing") {
                e.doing = format!("Recording “{title}”");
            }
        }
        let host = hosts.iter().find(|h| h.name == line.speaker).unwrap_or(&hosts[0]);
        match perform(state, &line.text, host).await {
            Ok(mp3) => {
                tokio::fs::write(dir.join(format!("{key}-{n}.mp3")), mp3).await?;
                update(state, ep, |e| {
                    if let Some(l) = e.segments.iter_mut().find(|s| s.key == key).and_then(|s| s.lines.get_mut(n)) {
                        l.ready = true;
                    }
                })
                .await;
            }
            Err(e) => {
                failures += 1;
                if failures >= 3 {
                    return Err(e);
                }
                tracing::warn!("recording a briefing line: {e:#}");
            }
        }
    }
}

// ---- The morning episode ----

/// Makes the day's episode at the time set, if there isn't one yet. (Not more than a few hours
/// late: if the computer was off all morning, it waits for you to ask.)
pub async fn daily(state: State) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let s = settings(&state).await;
        let Ok(at) = chrono::NaiveTime::parse_from_str(s.daily_at.trim(), "%H:%M") else { continue };
        let now = chrono::Local::now();
        let late = now.time().signed_duration_since(at);
        if late < chrono::Duration::zero() || late > chrono::Duration::hours(4) {
            continue;
        }
        if !s.sources.iter().any(|s| s.enabled) || !MAKING.lock().unwrap().is_empty() {
            continue;
        }
        let day = now.format("%Y-%m-%d").to_string();
        if summaries(&state).await.iter().any(|e| e.id.starts_with(&day)) {
            continue;
        }
        tracing::info!("making the morning briefing");
        if let Err(e) = begin(&state).await {
            tracing::warn!("morning briefing: {e:#}");
        }
    }
}

async fn summaries(state: &State) -> Vec<EpisodeSummary> {
    let mut out = Vec::new();
    let Ok(mut dirs) = tokio::fs::read_dir(base(state).join("episodes")).await else { return out };
    while let Ok(Some(d)) = dirs.next_entry().await {
        let id = d.file_name().to_string_lossy().to_string();
        if let Ok(e) = load(state, &id).await {
            out.push(EpisodeSummary {
                stories: e.segments.iter().filter(|s| s.kind == "story").count(),
                id: e.id,
                title: e.title,
                created: e.created,
                stage: e.stage,
            });
        }
    }
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

// ---- Questions while listening ----

pub async fn answer(state: &State, q: PodcastAsk) -> anyhow::Result<PodcastAnswer> {
    let s = settings(state).await;
    let ep = load(state, &q.episode).await?;
    let notes: Vec<String> = read_json(&episode_dir(state, &q.episode)?.join("notes.json")).await.unwrap_or_default();
    let seg = ep.segments.get(q.segment);
    let speaker = seg
        .and_then(|s| s.lines.get(q.line))
        .map(|l| l.speaker.clone())
        .or_else(|| s.hosts.first().map(|h| h.name.clone()))
        .unwrap_or_else(|| "Host".into());
    let host = s.hosts.iter().find(|h| h.name == speaker).cloned();
    let mut context = String::new();
    if let Some(seg) = seg {
        context.push_str(&format!("You're in the middle of the part about \"{}\".", seg.title));
        let said: Vec<String> = seg.lines.iter().take(q.line + 1).map(|l| format!("{}: {}", l.speaker, strip_directions(&l.text))).collect();
        if !said.is_empty() {
            context.push_str(&format!("\nWhat's been said so far in it:\n{}", said.join("\n")));
        }
        if seg.kind == "story" {
            let k = ep.segments.iter().take(q.segment).filter(|s| s.kind == "story").count();
            if let Some(n) = notes.get(k) {
                context.push_str(&format!("\n\nWhat you read for it:\n\"\"\"\n{}\n\"\"\"", feeds::clip(n, 12000)));
            }
            let srcs: Vec<String> = seg.sources.iter().map(|s| format!("{} ({}) {}", s.title, s.site, s.url)).collect();
            context.push_str(&format!("\nSources: {}", srcs.join("; ")));
        }
    }
    let others: Vec<String> = ep.segments.iter().filter(|s| s.kind == "story").map(|s| s.title.clone()).collect();
    let system = format!(
        "You are {speaker}, an AI host of the listener's personal daily news podcast (made from their own feeds), and \
         they've just paused it to ask you something. Today is {}. Today's stories: {}.\n\n{context}\n\n\
         Answer the way you'd talk on the show: conversational, clear and brief (two to five spoken sentences unless \
         they ask for more). Look things up on the web when you need facts beyond what you read, and say where they \
         came from. Be honest about what isn't known. Never read out web addresses. Begin what you say with an acting \
         direction in square brackets for the voice, e.g. [thoughtfully].\n\n\
         If they want to go deeper or off on a tangent (\"tell me more about...\", \"what's the history of...\", \
         \"go down that rabbit hole\"), give a short answer now and set \"dive\" to a short title for a fuller segment \
         you'll make about it next; otherwise leave it empty.\n\n\
         Reply with JSON only: {{\"say\": \"[direction] what you say\", \"links\": [{{\"title\": \"...\", \"url\": \"...\", \
         \"site\": \"...\"}}] (the best pages you found or used, if any), \"dive\": \"\"}}",
        today(),
        others.join("; ")
    );
    let mut turns: Vec<crate::llm::Turn> = q.history.iter().map(|t| (t.role.clone(), t.content.clone())).collect();
    turns.push(("user".into(), q.question.clone()));
    let reply = crate::llm::chat_web(state, &system, &turns).await?;
    #[derive(Deserialize, Default)]
    struct Reply {
        #[serde(default)]
        say: String,
        #[serde(default)]
        links: Vec<SourceLink>,
        #[serde(default)]
        dive: String,
    }
    let r: Reply = json_in(&reply, '{', '}').unwrap_or_else(|_| Reply { say: reply.clone(), ..Default::default() });
    let spoken = if r.say.trim().is_empty() { reply.clone() } else { r.say.trim().to_string() };
    let links: Vec<SourceLink> = r.links.into_iter().filter(|l| l.url.starts_with("http")).take(8).collect();
    let mut detour_key = None;
    if !r.dive.trim().is_empty() && !s.hosts.is_empty() {
        let key = format!("d{}", chrono::Utc::now().timestamp_millis());
        let seg = Segment { key: key.clone(), title: r.dive.trim().to_string(), kind: "detour".into(), sources: links.clone(), writing: true, lines: vec![] };
        let after = ep.segments.get(q.segment).map(|s| s.key.clone());
        start_detour(state, &q.episode, after, seg, q.question.clone(), context.clone(), s.clone()).await?;
        detour_key = Some(key);
    }
    let audio = match &host {
        Some(h) => match perform(state, &spoken, h).await {
            Ok(mp3) => Some(format!("data:audio/mpeg;base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, mp3))),
            Err(e) => {
                tracing::warn!("speaking the answer: {e:#}");
                None
            }
        },
        None => None,
    };
    Ok(PodcastAnswer { speaker, text: strip_directions(&spoken), spoken, audio, links, detour: detour_key })
}

/// A detour: a fuller segment on something the listener wanted to dig into, researched on the
/// web, put in right after the part they were listening to, and recorded at once.
async fn start_detour(
    state: &State,
    id: &str,
    after: Option<String>,
    seg: Segment,
    question: String,
    context: String,
    s: PodcastSettings,
) -> anyhow::Result<()> {
    // The live episode (it may still be being made), or brought back to life for this.
    let (live, ours) = match making(id) {
        Some(l) => (l, false),
        None => {
            let mut e = load(state, id).await?;
            e.stage = "recording".into();
            e.doing = format!("Looking into “{}”", seg.title);
            let l = Arc::new(Mutex::new(e));
            MAKING.lock().unwrap().insert(id.to_string(), l.clone());
            (l, true)
        }
    };
    let key = seg.key.clone();
    let title = seg.title.clone();
    update(state, &live, |e| {
        // After the part being listened to, and after any detours already queued there.
        let mut at = after.and_then(|a| e.segments.iter().position(|x| x.key == a)).map(|p| p + 1).unwrap_or(e.segments.len());
        while e.segments.get(at).is_some_and(|x| x.kind == "detour") {
            at += 1;
        }
        e.segments.insert(at.min(e.segments.len()), seg);
    })
    .await;
    let st = state.clone();
    let id = id.to_string();
    tokio::spawn(async move {
        let r = async {
            let lines = write_detour(&st, &s, &title, &question, &context).await?;
            anyhow::ensure!(!lines.is_empty(), "nothing came of it");
            update(&st, &live, |e| {
                if let Some(x) = e.segments.iter_mut().find(|x| x.key == key) {
                    x.lines = lines;
                    x.writing = false;
                }
            })
            .await;
            record(&st, &live, &s.hosts, Some(&key)).await
        }
        .await;
        if let Err(err) = &r {
            tracing::warn!("detour “{title}”: {err:#}");
            update(&st, &live, |e| e.segments.retain(|x| x.key != key || !x.lines.is_empty())).await;
        }
        if ours {
            update(&st, &live, |e| {
                e.segments.retain(|x| !(x.key == key && x.lines.is_empty()));
                if let Some(x) = e.segments.iter_mut().find(|x| x.key == key) {
                    x.writing = false;
                }
                e.stage = "ready".into();
                e.doing.clear();
            })
            .await;
            MAKING.lock().unwrap().remove(&id);
        }
    });
    Ok(())
}

async fn write_detour(state: &State, s: &PodcastSettings, title: &str, question: &str, context: &str) -> anyhow::Result<Vec<Line>> {
    let names: Vec<String> = s.hosts.iter().map(|h| h.name.clone()).collect();
    let system = format!(
        "You write a personal daily news podcast for one listener, made and voiced by AI. Today is {}. {}\n\n\
         The listener stopped the show to go down a rabbit hole, and this is the detour: research it properly on the \
         web first (several searches; read the best sources), then write a lively, well-informed segment of about 350 \
         words that digs into it: background, the interesting details, what's new, what's disputed. Say where facts \
         come from; don't make anything up. Open by picking up their question (\"So you asked about...\") and end by \
         handing back to the show.\n\n\
         Begin every line with an acting direction in square brackets for the voice actor.\n\n\
         Answer with JSON only: [{{\"speaker\": one of {:?}, \"text\": \"[direction] what they say\"}}].",
        today(),
        hosts_blurb(s),
        names
    );
    let prompt = format!("Detour: {title}\nTheir words: \"{question}\"\n\nWhere the show was:\n{context}");
    let answer = crate::llm::chat_web(state, &system, &[("user".into(), prompt)]).await?;
    let raw: Vec<RawLine> = json_in(&answer, '[', ']')?;
    Ok(raw
        .into_iter()
        .enumerate()
        .filter(|(_, l)| !l.text.trim().is_empty())
        .map(|(n, l)| Line {
            speaker: names.iter().find(|h| h.eq_ignore_ascii_case(l.speaker.trim())).cloned().unwrap_or_else(|| names[n % names.len()].clone()),
            text: l.text.trim().to_string(),
            ready: false,
        })
        .collect())
}

// ---- Routes ----

fn bad_gateway(e: anyhow::Error) -> ApiError {
    ApiError(axum::http::StatusCode::BAD_GATEWAY, format!("{e:#}"))
}

async fn get_settings(AxState(s): AxState<State>) -> Json<PodcastSettings> {
    Json(settings(&s).await)
}

async fn put_settings(AxState(s): AxState<State>, Json(new): Json<PodcastSettings>) -> ApiResult<Json<PodcastSettings>> {
    write_json(&base(&s).join("settings.json"), &new).await?;
    Ok(Json(new))
}

async fn list(AxState(s): AxState<State>) -> Json<Vec<EpisodeSummary>> {
    Json(summaries(&s).await)
}

async fn start(AxState(s): AxState<State>) -> ApiResult<Json<Episode>> {
    Ok(Json(begin(&s).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?))
}

async fn episode(AxState(s): AxState<State>, Path(id): Path<String>) -> ApiResult<Json<Episode>> {
    load(&s, &id).await.map(Json).map_err(|_| ApiError::not_found("episode"))
}

async fn remove(AxState(s): AxState<State>, Path(id): Path<String>) -> ApiResult<()> {
    if making(&id).is_some() {
        return Err(ApiError::bad_request("That episode is still being made"));
    }
    tokio::fs::remove_dir_all(episode_dir(&s, &id)?).await?;
    Ok(())
}

/// Records any lines that didn't get recorded (the voices weren't running, say).
async fn record_again(AxState(s): AxState<State>, Path(id): Path<String>) -> ApiResult<Json<Episode>> {
    if making(&id).is_some() {
        return Err(ApiError::bad_request("That episode is still being made"));
    }
    let mut e = load(&s, &id).await.map_err(|_| ApiError::not_found("episode"))?;
    e.stage = "recording".into();
    e.error = None;
    e.doing = "Recording".into();
    let live = Arc::new(Mutex::new(e.clone()));
    MAKING.lock().unwrap().insert(id.clone(), live.clone());
    let hosts = settings(&s).await.hosts;
    tokio::spawn(async move {
        let r = record(&s, &live, &hosts, None).await;
        update(&s, &live, |e| match r {
            Ok(()) => {
                e.stage = "ready".into();
                e.doing.clear();
            }
            Err(err) => {
                e.stage = "failed".into();
                e.error = Some(format!("{err:#}"));
            }
        })
        .await;
        MAKING.lock().unwrap().remove(&id);
    });
    Ok(Json(e))
}

async fn line_audio(AxState(s): AxState<State>, Path((id, key, line)): Path<(String, String, usize)>) -> ApiResult<Response> {
    if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(ApiError::not_found("recording"));
    }
    let file = episode_dir(&s, &id)?.join(format!("{key}-{line}.mp3"));
    let bytes = tokio::fs::read(file).await.map_err(|_| ApiError::not_found("recording"))?;
    Ok(([(header::CONTENT_TYPE, "audio/mpeg"), (header::CACHE_CONTROL, "max-age=86400")], bytes).into_response())
}

async fn ask(AxState(s): AxState<State>, Json(q): Json<PodcastAsk>) -> ApiResult<Json<PodcastAnswer>> {
    Ok(Json(answer(&s, q).await.map_err(bad_gateway)?))
}

async fn voices(AxState(s): AxState<State>) -> Json<Vec<HostVoice>> {
    let dir = s.data_dir.join("voices");
    Json(
        settings(&s)
            .await
            .hosts
            .into_iter()
            .map(|h| HostVoice { clip: dir.join(format!("{}.wav", voice_name(&h.name))).exists(), name: h.name, voice: h.voice })
            .collect(),
    )
}

#[derive(Deserialize)]
struct Recast {
    name: String,
}

/// Makes a host's voice again from their (changed) description.
async fn recast(AxState(s): AxState<State>, Json(r): Json<Recast>) -> ApiResult<()> {
    let hosts = settings(&s).await.hosts;
    let host = hosts.into_iter().find(|h| h.name == r.name).ok_or_else(|| ApiError::not_found("host"))?;
    cast_voices(&s, &[host], true).await.map_err(bad_gateway)?;
    Ok(())
}

async fn clip(AxState(s): AxState<State>, Path(name): Path<String>) -> ApiResult<Response> {
    let bytes = tokio::fs::read(s.data_dir.join("voices").join(format!("{}.wav", voice_name(&name))))
        .await
        .map_err(|_| ApiError::not_found("voice"))?;
    Ok(([(header::CONTENT_TYPE, "audio/wav"), (header::CACHE_CONTROL, "no-cache")], bytes).into_response())
}

/// A recording of a host's real voice, uploaded: kept with the briefing's files, made the
/// host's sample, and their voice made from it.
async fn upload_sample(AxState(s): AxState<State>, Path(name): Path<String>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> ApiResult<Json<PodcastSettings>> {
    if body.is_empty() {
        return Err(ApiError::bad_request("no recording"));
    }
    let mut settings = settings(&s).await;
    let i = settings.hosts.iter().position(|h| h.name == name).ok_or_else(|| ApiError::not_found("host"))?;
    let ext = match headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("") {
        t if t.contains("mpeg") || t.contains("mp3") => "mp3",
        t if t.contains("wav") => "wav",
        t if t.contains("ogg") || t.contains("opus") => "ogg",
        t if t.contains("flac") => "flac",
        t if t.contains("webm") => "webm",
        t if t.contains("mp4") || t.contains("m4a") || t.contains("aac") => "m4a",
        _ => "audio",
    };
    let dir = base(&s).join("samples");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| bad_gateway(e.into()))?;
    // A new name each time, so a new recording always counts as a change.
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let file = dir.join(format!("{}-{stamp}.{ext}", voice_name(&name)));
    tokio::fs::write(&file, &body).await.map_err(|e| bad_gateway(e.into()))?;
    settings.hosts[i].sample = file.to_string_lossy().into_owned();
    write_json(&base(&s).join("settings.json"), &settings).await.map_err(bad_gateway)?;
    cast_voices(&s, &[settings.hosts[i].clone()], true).await.map_err(bad_gateway)?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct Site {
    site: String,
}

/// Opens the site's login page in the remote browser, for you to log in through the viewer.
async fn browser_login(AxState(s): AxState<State>, Json(site): Json<Site>) -> ApiResult<()> {
    let wd = settings(&s).await.webdriver_url;
    browser::open_login(&s, &wd, &site.site).await.map_err(bad_gateway)
}

async fn browser_check(AxState(s): AxState<State>, Json(site): Json<Site>) -> ApiResult<Json<bool>> {
    let wd = settings(&s).await.webdriver_url;
    Ok(Json(browser::logged_in(&s, &wd, &site.site).await.map_err(bad_gateway)?))
}

/// The player is open: have the performer loaded and kept loaded.
async fn warm(AxState(s): AxState<State>) -> Json<serde_json::Value> {
    crate::cast::cast_open();
    let tts = s.settings.read().await.ai.tts_url.clone();
    let _ = s.http.post(format!("{}/warm", tts.trim_end_matches('/'))).json(&json!({})).timeout(Duration::from_secs(5)).send().await;
    Json(json!({ "ok": true }))
}
