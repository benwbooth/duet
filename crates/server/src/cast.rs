//! The full cast, performed live: a chapter is directed the first time it's needed (who says each
//! line of dialogue and how, and a voice description for anyone new), then each sentence is voiced
//! as the listener reaches it, a few sentences ahead. Everything is kept on disk, so a sentence is
//! only ever performed once.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::{State, book, db};

/// Sentences per direction request: a chapter is directed a part at a time, as the listener
/// reaches it, so the first line plays after one short request rather than the whole chapter's.
const CHUNK: usize = 30;

/// (speaker, acting direction) for each sentence with dialogue, by sentence number.
type Script = BTreeMap<usize, (String, String)>;

#[derive(Default, Serialize, Deserialize)]
struct Cast {
    narrator: String,
    characters: BTreeMap<String, String>,
}

/// What the full cast is busy with for each book, for the player to show while it waits:
/// (what, how many done, out of how many, since when).
static STATUS: LazyLock<std::sync::Mutex<HashMap<i64, (String, u32, u32, std::time::Instant)>>> = LazyLock::new(Default::default);

fn status(asset: i64, what: impl Into<String>, done: u32, total: u32) {
    STATUS.lock().unwrap().insert(asset, (what.into(), done, total, std::time::Instant::now()));
}

fn idle(asset: i64) {
    STATUS.lock().unwrap().remove(&asset);
}

/// What the full cast is doing for this book right now, if anything. While the performer
/// itself is starting (minutes, the first time), that, with how far along it is.
pub async fn current(state: &State, asset: i64) -> serde_json::Value {
    let now = STATUS.lock().unwrap().get(&asset).cloned();
    let Some((what, done, total, since)) = now else { return json!(null) };
    if what.starts_with("Performing") {
        let tts = state.settings.read().await.ai.tts_url.clone();
        let url = format!("{}/status", tts.trim_end_matches('/'));
        if let Ok(r) = state.http.get(url).timeout(std::time::Duration::from_secs(2)).send().await {
            if let Ok(v) = r.json::<serde_json::Value>().await {
                if v["fish_starting"].as_bool() == Some(true) {
                    let (elapsed, expected) = (v["elapsed"].as_f64().unwrap_or(0.0), v["expected"].as_f64().unwrap_or(360.0));
                    return json!({
                        "what": "Warming up the voices (the first line after a while takes longest)",
                        "progress": (elapsed / expected.max(1.0)).min(0.97),
                        "left": (expected - elapsed).max(0.0).round(),
                        "secs": elapsed.round(),
                    });
                }
            }
        }
    }
    json!({ "what": what, "done": done, "total": total, "secs": since.elapsed().as_secs() })
}

/// One lock per book: directing and casting happen once, however many requests arrive together.
static LOCKS: LazyLock<std::sync::Mutex<HashMap<i64, Arc<Mutex<()>>>>> = LazyLock::new(Default::default);

fn lock_for(asset: i64) -> Arc<Mutex<()>> {
    LOCKS.lock().unwrap().entry(asset).or_default().clone()
}

fn dir(state: &State, asset: i64) -> PathBuf {
    state.data_dir.join("cache").join("full-cast").join(asset.to_string())
}

fn is_quote(s: &str) -> bool {
    s.contains('“') || s.contains('"')
}

/// Splits a sentence into (text, speaker): quoted speech goes to the speaker, the rest to the narrator.
fn pieces(sentence: &str, line: Option<&(String, String)>) -> Vec<(String, Option<String>, String)> {
    let Some((speaker, direction)) = line else { return vec![(sentence.to_string(), None, String::new())] };
    let mut out = Vec::new();
    let mut rest = sentence;
    while let Some(open) = rest.find(['“', '"']) {
        let (before, from) = rest.split_at(open);
        let open_len = from.chars().next().map(char::len_utf8).unwrap_or(1);
        let inner = &from[open_len..];
        let close = inner.find(['”', '"']).unwrap_or(inner.len());
        out.push((before.trim().to_string(), None, String::new()));
        out.push((inner[..close].trim().to_string(), Some(speaker.clone()), direction.clone()));
        rest = inner.get(close..).map(|r| r.trim_start_matches(['”', '"'])).unwrap_or("");
    }
    out.push((rest.trim().to_string(), None, String::new()));
    out.retain(|(t, _, _)| t.chars().any(char::is_alphanumeric));
    out
}

fn json_in(text: &str) -> anyhow::Result<serde_json::Value> {
    let (start, end) = (text.find('{').context("no JSON in the reply")?, text.rfind('}').context("no JSON in the reply")?);
    Ok(serde_json::from_str(&text[start..=end])?)
}

async fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Option<T> {
    serde_json::from_slice(&tokio::fs::read(path).await.ok()?).ok()
}

async fn write_json<T: Serialize>(path: &PathBuf, v: &T) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    tokio::fs::write(path, serde_json::to_vec(v)?).await?;
    Ok(())
}

/// The chat model directs one chapter: speakers and directions, and descriptions of new characters
/// (and of the narrator, the first time).
async fn direct(state: &State, asset: i64, chapter: usize, part: usize, title: &str, author: &str, sentences: &[String], cast: &mut Cast) -> anyhow::Result<Script> {
    let mut script = Script::new();
    let parts = sentences.len().div_ceil(CHUNK).max(1);
    for start in (part * CHUNK..sentences.len()).step_by(CHUNK).take(1) {
        let what = if parts > 1 { format!("Reading part {} of chapter {} to work out who says what", part + 1, chapter + 1) } else { format!("Reading chapter {} to work out who says what", chapter + 1) };
        status(asset, what, 0, 0);
        let part = &sentences[start..(start + CHUNK).min(sentences.len())];
        if !part.iter().any(|s| is_quote(s)) && !cast.narrator.is_empty() {
            continue;
        }
        let numbered: String = part.iter().enumerate().map(|(i, s)| format!("{}: {s}\n", start + i)).collect();
        let known: String = cast.characters.iter().map(|(n, d)| format!("- {n}: {d}\n")).collect();
        let system = "You are the director of a full-cast audiobook. For each numbered sentence containing quoted \
            speech, name the character speaking and give a short acting \
            direction for how the line is delivered: emotion, intensity, pace, volume (e.g. 'barely holding back \
            tears', 'dry, amused'). Use the surrounding narration for cues. For any character not in the known \
            list, write a one-sentence voice description for casting: gender, approximate age, accent, pitch, \
            timbre, pace and temperament. If asked, also describe the ideal narrator's voice the same way. \
            Accents follow the book: where it's set and where its author is from (an English novel gets English \
            voices, narrator included); a character the story gives another origin speaks with that one instead (a \
            Russian count with a Russian accent, a French maid with a French one). Always name the accent. Reply \
            with JSON only: {\"lines\": {\"<sentence number>\": {\"speaker\": \"<name>\", \"direction\": \"<direction>\"}}, \
            \"new_characters\": {\"<name>\": \"<voice description>\"}, \"narrator\": \"<voice description or empty>\"}. \
            Each character keeps one voice for the whole book, so use exactly the name from the known list when \
            they're in it, and give a new character a plain name only (\"Alice\", \"White Rabbit\"): never add how \
            they're speaking or what they're doing to the name; that goes in the direction. Lines spoken by \
            several characters together go to the one leading them.";
        let ask_narrator = if cast.narrator.is_empty() { "\nAlso describe the narrator's voice." } else { "" };
        let user = format!(
            "Book: {title} by {author}\nKnown characters:\n{}{ask_narrator}\n\nSentences:\n{numbered}",
            if known.is_empty() { "(none yet)\n" } else { &known }
        );
        let reply = json_in(&crate::llm::chat(state, system, &[("user".into(), user)]).await?)?;
        if let Some(new) = reply["new_characters"].as_object() {
            for (n, d) in new {
                if let Some(name) = canonical(n, &cast.characters) {
                    cast.characters.entry(name).or_insert_with(|| d.as_str().unwrap_or("").to_string());
                }
            }
        }
        if let Some(lines) = reply["lines"].as_object() {
            for (k, v) in lines {
                let (Ok(n), Some(speaker)) = (k.parse::<usize>(), v["speaker"].as_str().or(v.as_str())) else { continue };
                // (The narrator's lines need no entry: they're the narrator's anyway.)
                let Some(speaker) = canonical(speaker, &cast.characters) else { continue };
                if n < sentences.len() {
                    script.insert(n, (speaker, v["direction"].as_str().unwrap_or("").trim().to_string()));
                }
            }
        }
        if let Some(d) = reply["narrator"].as_str().filter(|d| !d.is_empty() && cast.narrator.is_empty()) {
            cast.narrator = d.to_string();
        }
    }
    Ok(script)
}

/// The one name a character goes by for the whole book, so they keep one voice: the model
/// sometimes writes "Alice (reciting)", "the White Rabbit", "Narrator (reading address)" or
/// "March Hare and Hatter". Descriptions in brackets or after a dash go (they're directions),
/// "the" goes, a group is its first member, case is ignored against the characters already
/// cast, and the narrator in any guise is the narrator (None).
fn canonical(name: &str, known: &BTreeMap<String, String>) -> Option<String> {
    let mut n = name.trim();
    for cut in ["(", "[", " - ", " – ", " — ", ",", ";", ":"] {
        if let Some(i) = n.find(cut) {
            n = &n[..i];
        }
    }
    for sep in [" and ", " & ", "/"] {
        if let Some(i) = n.find(sep) {
            n = &n[..i];
        }
    }
    let mut n = n.trim().trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    for lead in ["the ", "The "] {
        if let Some(rest) = n.strip_prefix(lead) {
            n = rest.to_string();
        }
    }
    let key = n.to_lowercase();
    if key.is_empty() || key == "narrator" || key == "narration" || key == "unknown" {
        return None;
    }
    let simple = |k: &str| k.to_lowercase().trim_start_matches("the ").to_string();
    Some(known.keys().find(|k| simple(k) == key).cloned().unwrap_or(n))
}

fn voice_name(asset: i64, who: Option<&str>) -> String {
    let who = who.unwrap_or("narrator").to_lowercase();
    let slug: String = who.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    format!("cast{asset}-{}", slug.trim_matches('-'))
}

/// The script for the part of the chapter sentence `n` is in, and the cast, directing (and
/// casting) that part the first time.
async fn prepared(state: &State, asset: i64, chapter: usize, n: usize, sentences: &[String]) -> anyhow::Result<(Script, Cast)> {
    let base = dir(state, asset);
    let part = n / CHUNK;
    let cast_path = base.join("cast.json");
    let lock = lock_for(asset);
    let _held = lock.lock().await;
    let mut cast: Cast = read_json(&cast_path).await.unwrap_or_default();
    // (A whole chapter directed at once, as earlier versions did.)
    if let Some(script) = read_json::<Script>(&base.join(format!("script-{chapter}.json"))).await {
        return Ok((script, cast));
    }
    let script_path = base.join(format!("script-{chapter}-{part}.json"));
    if let Some(script) = read_json::<Script>(&script_path).await {
        return Ok((script, cast));
    }
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let w = db::work_detail(&state.db, a.work_id).await?.context("no such book")?;
    let script = match direct(state, asset, chapter, part, &w.title, w.author.as_deref().unwrap_or(""), sentences, &mut cast).await {
        Ok(s) => s,
        Err(e) => {
            // No chat model (or it failed): the narrator reads this chapter; try directing again later.
            tracing::warn!("directing chapter {chapter}: {e:#}");
            idle(asset);
            return Ok((Script::new(), cast));
        }
    };
    if cast.narrator.is_empty() {
        cast.narrator = "A warm, clear, expressive audiobook narrator.".into();
    }
    // Cast everyone in this part up front, so the voice server isn't swapping models mid-part.
    // (One at a time, so the player can say whose voice is being made.)
    // Every character met so far, not just this part's: making a voice means stopping the
    // performer (they can't share the card), so any still missing are all made in that one stop.
    // (Ones already made are skipped at once.)
    let mut voices = vec![("the narrator".to_string(), json!({ "voice": voice_name(asset, None), "instructions": cast.narrator }))];
    let mut everyone: std::collections::BTreeSet<String> = script.values().map(|(s, _)| s.clone()).collect();
    everyone.extend(cast.characters.keys().cloned());
    for who in &everyone {
        let desc = cast.characters.get(who).cloned().unwrap_or_default();
        voices.push((who.clone(), json!({ "voice": voice_name(asset, Some(who)), "instructions": desc })));
    }
    let tts = state.settings.read().await.ai.tts_url.clone();
    let total = voices.len() as u32;
    for (k, (who, v)) in voices.into_iter().enumerate() {
        status(asset, format!("Finding a voice for {who}"), k as u32, total);
        state
            .http
            .post(format!("{}/voices", tts.trim_end_matches('/')))
            .json(&json!({ "voices": [v] }))
            .timeout(std::time::Duration::from_secs(1800))
            .send()
            .await
            .context("the voice server isn't running (Settings → Voices)")?;
    }
    write_json(&cast_path, &cast).await?;
    write_json(&script_path, &script).await?;
    Ok((script, cast))
}

/// When each word of a performed sentence starts in its recording, for the read-along: timed
/// by the forced aligner once and kept beside the recording.
pub async fn words(state: &State, asset: i64, chapter: usize, n: usize) -> anyhow::Result<Vec<f64>> {
    let base = dir(state, asset);
    let path = base.join(format!("{chapter}-{n}.words.json"));
    if let Some(w) = read_json::<Vec<f64>>(&path).await {
        return Ok(w);
    }
    let mp3 = base.join(format!("{chapter}-{n}.mp3"));
    anyhow::ensure!(tokio::fs::try_exists(&mp3).await.unwrap_or(false), "not performed yet");
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let src = book::source(&a, &state.data_dir);
    let sentences = tokio::task::spawn_blocking(move || book::chapter_sentences(&src, asset, chapter)).await??;
    let text = sentences.get(n).cloned().context("no such sentence")?;
    let tts = state.settings.read().await.ai.tts_url.clone();
    let v: serde_json::Value = state
        .http
        .post(format!("{}/align-words", tts.trim_end_matches('/')))
        .json(&json!({ "audio": mp3, "text": text }))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")?
        .error_for_status()?
        .json()
        .await?;
    let w: Vec<f64> = serde_json::from_value(v["words"].clone()).unwrap_or_default();
    write_json(&path, &w).await.ok();
    Ok(w)
}

/// Lines being performed right now: a second request for one waits for the first.
static PERFORMING: LazyLock<std::sync::Mutex<HashMap<(i64, usize, usize), Arc<Mutex<()>>>>> = LazyLock::new(Default::default);

/// Where the listener is in each book (chapter, sentence), and a count bumped when they move,
/// for the performer working ahead in the background.
static LISTENER: LazyLock<std::sync::Mutex<HashMap<i64, (usize, usize, u64)>>> = LazyLock::new(Default::default);

/// Keeps performing ahead of the listener, in the background, while the card is free: on through
/// this chapter and into the next, a line at a time, so the lines are ready by the time they're
/// reached even though performing takes longer than listening. Stops when the listener moves
/// elsewhere (a new one starts from there).
fn work_ahead(state: &State, asset: i64, chapter: usize, n: usize) {
    let generation = {
        let mut l = LISTENER.lock().unwrap();
        let entry = l.entry(asset).or_insert((chapter, n, 0));
        // Still in the stretch being worked on: nothing new to start.
        if entry.0 == chapter && n >= entry.1 && n < entry.1 + 40 && entry.2 > 0 {
            return;
        }
        *entry = (chapter, n, entry.2 + 1);
        entry.2
    };
    let state = state.clone();
    tokio::spawn(async move {
        let moved = || LISTENER.lock().unwrap().get(&asset).map(|e| e.2) != Some(generation);
        for c in chapter..chapter + 2 {
            let Ok(Some(a)) = db::asset(&state.db, asset).await else { return };
            let src = book::source(&a, &state.data_dir);
            let Ok(Ok(sentences)) = tokio::task::spawn_blocking(move || book::chapter_sentences(&src, asset, c)).await else { return };
            let from = if c == chapter { n } else { 0 };
            for k in from..sentences.len() {
                if moved() {
                    return;
                }
                if let Err(e) = perform(&state, asset, c, k).await {
                    tracing::warn!("performing ahead ({c}, {k}): {e:#}");
                    return;
                }
                if let Some(entry) = LISTENER.lock().unwrap().get_mut(&asset).filter(|e| e.2 == generation) {
                    // (Lets a request a little further on count as the same stretch.)
                    if entry.0 == c {
                        entry.1 = entry.1.max(k.saturating_sub(20));
                    }
                }
            }
        }
    });
}

/// One sentence, performed: MP3 bytes, from the cache when it's been performed before. Asking
/// for one also sets the performer working ahead from it.
pub async fn sentence(state: &State, asset: i64, chapter: usize, n: usize) -> anyhow::Result<Vec<u8>> {
    work_ahead(state, asset, chapter, n);
    perform(state, asset, chapter, n).await
}

async fn perform(state: &State, asset: i64, chapter: usize, n: usize) -> anyhow::Result<Vec<u8>> {
    let path = dir(state, asset).join(format!("{chapter}-{n}.mp3"));
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes);
    }
    let line = PERFORMING.lock().unwrap().entry((asset, chapter, n)).or_default().clone();
    let _one = line.lock().await;
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes);
    }
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    if a.format == "transcript" {
        crate::audiotext::ensure_chapter(state, &a, chapter).await?;
    }
    let src = book::source(&a, &state.data_dir);
    let sentences = tokio::task::spawn_blocking(move || book::chapter_sentences(&src, asset, chapter)).await??;
    let text = sentences.get(n).cloned().context("no such sentence")?;
    let (script, cast) = prepared(state, asset, chapter, n, &sentences).await?;
    // (Scripts directed before names were tidied: the same one voice per character.)
    let line = script.get(&n).and_then(|(who, dir)| canonical(who, &cast.characters).map(|w| (w, dir.clone())));
    let parts: Vec<_> = pieces(&text, line.as_ref())
        .into_iter()
        .map(|(t, who, direction)| {
            let input = if direction.is_empty() { t } else { format!("[{direction}] {t}") };
            let desc = who.as_ref().and_then(|w| cast.characters.get(w)).unwrap_or(&cast.narrator);
            json!({ "input": input, "voice": voice_name(asset, who.as_deref()), "instructions": desc })
        })
        .collect();
    let tts = state.settings.read().await.ai.tts_url.clone();
    status(asset, "Performing the next line", 0, 0);
    let resp = state
        .http
        .post(format!("{}/audio/script", tts.trim_end_matches('/')))
        .json(&json!({ "pieces": parts, "response_format": "mp3", "pause": 0.3 }))
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")?;
    if !resp.status().is_success() {
        idle(asset);
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        anyhow::bail!("voice server: {}", v["error"].as_str().unwrap_or("failed"));
    }
    let bytes = resp.bytes().await?.to_vec();
    idle(asset);
    write_json(&path, &()).await.ok(); // makes the directory
    tokio::fs::write(&path, &bytes).await?;
    // Its words timed straight away, ready for the read-along when it plays.
    let st = state.clone();
    tokio::spawn(async move {
        let _ = words(&st, asset, chapter, n).await;
    });
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{canonical, pieces};
    use std::collections::BTreeMap;

    #[test]
    fn one_name_per_character() {
        let known: BTreeMap<String, String> = [("Alice".to_string(), String::new()), ("White Rabbit".to_string(), String::new())].into();
        assert_eq!(canonical("Alice (reciting)", &known).as_deref(), Some("Alice"));
        assert_eq!(canonical("the white rabbit", &known).as_deref(), Some("White Rabbit"));
        assert_eq!(canonical("Narrator (reading address)", &known), None);
        assert_eq!(canonical("March Hare and Hatter", &known).as_deref(), Some("March Hare"));
        assert_eq!(canonical("Dodo", &known).as_deref(), Some("Dodo"));
    }

    #[test]
    fn splits_dialogue_from_narration() {
        let line = ("Alice".to_string(), "curious".to_string());
        let p = pieces("“Curiouser and curiouser!” cried Alice.", Some(&line));
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].0, "Curiouser and curiouser!");
        assert_eq!(p[0].1.as_deref(), Some("Alice"));
        assert_eq!(p[1].0, "cried Alice.");
        assert!(p[1].1.is_none());
    }
}
