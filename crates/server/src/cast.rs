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

/// Sentences per direction request.
const CHUNK: usize = 120;

/// (speaker, acting direction) for each sentence with dialogue, by sentence number.
type Script = BTreeMap<usize, (String, String)>;

#[derive(Default, Serialize, Deserialize)]
struct Cast {
    narrator: String,
    characters: BTreeMap<String, String>,
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
async fn direct(state: &State, title: &str, author: &str, sentences: &[String], cast: &mut Cast) -> anyhow::Result<Script> {
    let mut script = Script::new();
    for start in (0..sentences.len()).step_by(CHUNK) {
        let part = &sentences[start..(start + CHUNK).min(sentences.len())];
        if !part.iter().any(|s| is_quote(s)) && !cast.narrator.is_empty() {
            continue;
        }
        let numbered: String = part.iter().enumerate().map(|(i, s)| format!("{}: {s}\n", start + i)).collect();
        let known: String = cast.characters.iter().map(|(n, d)| format!("- {n}: {d}\n")).collect();
        let system = "You are the director of a full-cast audiobook. For each numbered sentence containing quoted \
            speech, name the character speaking (one consistent name per character) and give a short acting \
            direction for how the line is delivered: emotion, intensity, pace, volume (e.g. 'barely holding back \
            tears', 'dry, amused'). Use the surrounding narration for cues. For any character not in the known \
            list, write a one-sentence voice description for casting: gender, approximate age, accent, pitch, \
            timbre, pace and temperament. If asked, also describe the ideal narrator's voice the same way. Reply \
            with JSON only: {\"lines\": {\"<sentence number>\": {\"speaker\": \"<name>\", \"direction\": \"<direction>\"}}, \
            \"new_characters\": {\"<name>\": \"<voice description>\"}, \"narrator\": \"<voice description or empty>\"}";
        let ask_narrator = if cast.narrator.is_empty() { "\nAlso describe the narrator's voice." } else { "" };
        let user = format!(
            "Book: {title} by {author}\nKnown characters:\n{}{ask_narrator}\n\nSentences:\n{numbered}",
            if known.is_empty() { "(none yet)\n" } else { &known }
        );
        let reply = json_in(&crate::llm::chat(state, system, &[("user".into(), user)]).await?)?;
        if let Some(lines) = reply["lines"].as_object() {
            for (k, v) in lines {
                let (Ok(n), Some(speaker)) = (k.parse::<usize>(), v["speaker"].as_str().or(v.as_str())) else { continue };
                if n < sentences.len() && !speaker.is_empty() {
                    script.insert(n, (speaker.to_string(), v["direction"].as_str().unwrap_or("").trim().to_string()));
                }
            }
        }
        if let Some(new) = reply["new_characters"].as_object() {
            for (n, d) in new {
                cast.characters.entry(n.clone()).or_insert_with(|| d.as_str().unwrap_or("").to_string());
            }
        }
        if let Some(d) = reply["narrator"].as_str().filter(|d| !d.is_empty() && cast.narrator.is_empty()) {
            cast.narrator = d.to_string();
        }
    }
    Ok(script)
}

fn voice_name(asset: i64, who: Option<&str>) -> String {
    let who = who.unwrap_or("narrator").to_lowercase();
    let slug: String = who.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    format!("cast{asset}-{}", slug.trim_matches('-'))
}

/// The chapter's script and the cast, directing (and casting) the chapter the first time.
async fn prepared(state: &State, asset: i64, chapter: usize, sentences: &[String]) -> anyhow::Result<(Script, Cast)> {
    let base = dir(state, asset);
    let (script_path, cast_path) = (base.join(format!("script-{chapter}.json")), base.join("cast.json"));
    let lock = lock_for(asset);
    let _held = lock.lock().await;
    let mut cast: Cast = read_json(&cast_path).await.unwrap_or_default();
    if let Some(script) = read_json::<Script>(&script_path).await {
        return Ok((script, cast));
    }
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let w = db::work_detail(&state.db, a.work_id).await?.context("no such book")?;
    let script = match direct(state, &w.title, w.author.as_deref().unwrap_or(""), sentences, &mut cast).await {
        Ok(s) => s,
        Err(e) => {
            // No chat model (or it failed): the narrator reads this chapter; try directing again later.
            tracing::warn!("directing chapter {chapter}: {e:#}");
            return Ok((Script::new(), cast));
        }
    };
    if cast.narrator.is_empty() {
        cast.narrator = "A warm, clear, expressive audiobook narrator.".into();
    }
    // Cast everyone in this chapter up front, so the voice server isn't swapping models mid-chapter.
    let mut voices = vec![json!({ "voice": voice_name(asset, None), "instructions": cast.narrator })];
    for who in script.values().map(|(s, _)| s).collect::<std::collections::BTreeSet<_>>() {
        let desc = cast.characters.get(who).cloned().unwrap_or_default();
        voices.push(json!({ "voice": voice_name(asset, Some(who)), "instructions": desc }));
    }
    let tts = state.settings.read().await.ai.tts_url.clone();
    state
        .http
        .post(format!("{}/voices", tts.trim_end_matches('/')))
        .json(&json!({ "voices": voices }))
        .timeout(std::time::Duration::from_secs(1800))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")?;
    write_json(&cast_path, &cast).await?;
    write_json(&script_path, &script).await?;
    Ok((script, cast))
}

/// One sentence, performed: MP3 bytes, from the cache when it's been performed before.
pub async fn sentence(state: &State, asset: i64, chapter: usize, n: usize) -> anyhow::Result<Vec<u8>> {
    let path = dir(state, asset).join(format!("{chapter}-{n}.mp3"));
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes);
    }
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let src = book::source(&a, &state.data_dir);
    let sentences = tokio::task::spawn_blocking(move || book::chapter_sentences(&src, asset, chapter)).await??;
    let text = sentences.get(n).cloned().context("no such sentence")?;
    let (script, cast) = prepared(state, asset, chapter, &sentences).await?;
    let parts: Vec<_> = pieces(&text, script.get(&n))
        .into_iter()
        .map(|(t, who, direction)| {
            let input = if direction.is_empty() { t } else { format!("[{direction}] {t}") };
            let desc = who.as_ref().and_then(|w| cast.characters.get(w)).unwrap_or(&cast.narrator);
            json!({ "input": input, "voice": voice_name(asset, who.as_deref()), "instructions": desc })
        })
        .collect();
    let tts = state.settings.read().await.ai.tts_url.clone();
    let resp = state
        .http
        .post(format!("{}/audio/script", tts.trim_end_matches('/')))
        .json(&json!({ "pieces": parts, "response_format": "mp3", "pause": 0.3 }))
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")?;
    if !resp.status().is_success() {
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        anyhow::bail!("voice server: {}", v["error"].as_str().unwrap_or("failed"));
    }
    let bytes = resp.bytes().await?.to_vec();
    write_json(&path, &()).await.ok(); // makes the directory
    tokio::fs::write(&path, &bytes).await?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::pieces;

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
