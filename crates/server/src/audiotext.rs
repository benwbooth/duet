//! A book made from an audiobook, as you go: each track becomes a chapter the first time it's
//! opened. The recording is transcribed with word timings, the chat model formats it like printed
//! text (punctuation, dialogue, paragraphs, headings), and since every word's time is known the
//! text is in sync with the recording from the start.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use anyhow::Context;
use duet_common::{Asset, AssetKind, BookLayout, ReadMode, SyncSpan, TocEntry};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::{State, book, db, jobs};

/// A long pause in the narration starts a new paragraph (when there's no chat model to format it).
const PARAGRAPH_PAUSE: f64 = 1.2;
/// Characters of transcript formatted per request.
const BATCH: usize = 4000;

#[derive(Deserialize)]
struct Segment {
    start: f64,
    end: f64,
    text: String,
    words: Vec<(String, f64, f64)>,
}

static LOCKS: LazyLock<std::sync::Mutex<HashMap<(i64, usize), Arc<Mutex<()>>>>> = LazyLock::new(Default::default);

fn dir(state: &State, audio: i64) -> PathBuf {
    state.data_dir.join("transcripts").join(audio.to_string())
}

/// The audiobook a transcript was made from (its folder is named after it).
fn audio_of(asset: &Asset) -> Option<i64> {
    Path::new(&asset.path).file_name()?.to_str()?.parse().ok()
}

/// The work's text-from-the-audiobook edition, created (empty) the first time it's asked for.
pub async fn ensure_asset(state: &State, work_id: i64) -> anyhow::Result<Asset> {
    let assets = db::work_assets(&state.db, work_id).await?;
    if let Some(a) = assets.iter().find(|a| a.format == "transcript") {
        return Ok(a.clone());
    }
    let audio = assets
        .iter()
        .filter(|a| a.kind == AssetKind::Audiobook)
        .min_by_key(|a| a.origin == "generated")
        .context("this book has no audiobook")?;
    let folder = dir(state, audio.id);
    tokio::fs::create_dir_all(&folder).await?;
    let toc = audio.tracks.iter().enumerate().map(|(i, t)| TocEntry { title: t.title.clone(), chapter: i, depth: 0 }).collect();
    let layout = BookLayout { mode: ReadMode::Chapters, chapters: audio.tracks.len(), toc, pages: 0 };
    tokio::fs::write(folder.join("layout.json"), serde_json::to_vec(&layout)?).await?;
    let path = folder.to_string_lossy().to_string();
    sqlx::query("INSERT INTO assets (work_id, kind, format, path, size, origin) VALUES (?, 'ebook', 'transcript', ?, 0, 'generated')")
        .bind(work_id)
        .bind(&path)
        .execute(&state.db)
        .await?;
    db::work_assets(&state.db, work_id).await?.into_iter().find(|a| a.format == "transcript").context("creating the transcript")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Plain words for matching formatted text back to what was heard.
fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’')
        .map(|w| w.trim_matches(['\'', '’']).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Paragraphs at long pauses, straight from the transcript.
fn raw_paragraphs(segments: &[Segment]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut last_end = 0.0;
    for seg in segments {
        // "Chapter VII." read on its own is the chapter's heading.
        let text = seg.text.trim();
        if text.len() < 40 && text.to_lowercase().starts_with("chapter") {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(format!("# {}", text.trim_end_matches('.')));
            last_end = seg.end;
            continue;
        }
        if !cur.is_empty() && seg.start - last_end > PARAGRAPH_PAUSE {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(seg.text.trim());
        last_end = seg.end;
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Has the chat model set the transcript like a printed book, a few thousand characters at a time.
/// Without a chat model (or if it fails) the plain transcript is used.
async fn format(state: &State, title: &str, paras: Vec<String>) -> Vec<String> {
    let system = format!(
        "You format audiobook transcripts of \"{title}\" as printed book text. Fix punctuation, capitalisation, \
         dialogue quotation marks (use curly quotes) and paragraph breaks. Put any chapter or section heading the \
         narrator reads on its own line starting with '# '. Drop recording notices that aren't part of the book \
         (for example 'This is a LibriVox recording...'). Do not add, summarise or reword anything else. Reply \
         with the text only, paragraphs separated by blank lines."
    );
    let mut out = Vec::new();
    let mut batch: Vec<String> = Vec::new();
    let flush = |batch: Vec<String>| {
        let system = system.clone();
        async move {
            let text = batch.join("\n\n");
            match crate::llm::chat(state, &system, &[("user".into(), text)]).await {
                Ok(fixed) if !fixed.trim().is_empty() => {
                    fixed.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect()
                }
                Ok(_) => batch,
                Err(e) => {
                    tracing::warn!("formatting the transcript: {e:#}");
                    batch
                }
            }
        }
    };
    for p in paras {
        batch.push(p);
        if batch.iter().map(String::len).sum::<usize>() > BATCH {
            out.extend(flush(std::mem::take(&mut batch)).await);
        }
    }
    if !batch.is_empty() {
        out.extend(flush(batch).await);
    }
    out
}

/// Times for each sentence: its words are matched, in order, to the words heard.
fn timings(sentences: &[String], heard: &[(String, f64, f64)], chapter: usize, track: usize) -> Vec<SyncSpan> {
    let heard: Vec<(String, f64, f64)> =
        heard.iter().flat_map(|(w, s, e)| tokens(w).into_iter().map(move |t| (t, *s, *e))).collect();
    let mut at = 0usize;
    let mut out = Vec::new();
    for (i, sentence) in sentences.iter().enumerate() {
        let (mut first, mut last) = (None::<f64>, None::<f64>);
        for w in tokens(sentence) {
            // Formatting rarely moves words far, so look a little way ahead for each one.
            if let Some(k) = (at..(at + 40).min(heard.len())).find(|&k| heard[k].0 == w) {
                first.get_or_insert(heard[k].1);
                last = Some(heard[k].2);
                at = k + 1;
            }
        }
        if let (Some(start), Some(end)) = (first, last) {
            out.push(SyncSpan { chapter, sentence: i, track, start, end: end.max(start) });
        }
    }
    out
}

/// Makes chapter `n` (track `n` of the recording) if it hasn't been made yet, and syncs it.
pub async fn ensure_chapter(state: &State, asset: &Asset, n: usize) -> anyhow::Result<()> {
    let folder = PathBuf::from(&asset.path);
    let html_path = folder.join(format!("{n}.html"));
    if html_path.exists() {
        return Ok(());
    }
    let lock = LOCKS.lock().unwrap().entry((asset.id, n)).or_default().clone();
    let _held = lock.lock().await;
    if html_path.exists() {
        return Ok(());
    }
    let audio_id = audio_of(asset).context("not a transcript")?;
    let track_path = db::track_path(&state.db, audio_id, n as i64).await?.context("no such part of the recording")?;
    let tts = state.settings.read().await.ai.tts_url.clone();
    let resp = state
        .http
        .post(format!("{}/track-words", tts.trim_end_matches('/')))
        .json(&json!({ "audio": track_path }))
        .timeout(std::time::Duration::from_secs(3600))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices); it does the listening")?;
    if !resp.status().is_success() {
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        anyhow::bail!("transcribing: {}", v["error"].as_str().unwrap_or("failed"));
    }
    #[derive(Deserialize)]
    struct Reply {
        segments: Vec<Segment>,
    }
    let segments = resp.json::<Reply>().await?.segments;
    // The plain transcript first, so the chapter can be read right away; the chat model's typeset
    // version replaces it when it's done.
    write_chapter(state, asset, audio_id, n, &raw_paragraphs(&segments), &segments).await?;
    let (state, asset) = (state.clone(), asset.clone());
    tokio::spawn(async move {
        let title = db::work_detail(&state.db, asset.work_id).await.ok().flatten().map(|w| w.title).unwrap_or_default();
        let raw = raw_paragraphs(&segments);
        let formatted = format(&state, &title, raw.clone()).await;
        if formatted != raw {
            if let Err(e) = write_chapter(&state, &asset, audio_id, n, &formatted, &segments).await {
                tracing::warn!("formatted chapter {n}: {e:#}");
            }
        }
    });
    Ok(())
}

/// Writes chapter `n` from its paragraphs and times its sentences from the words heard.
async fn write_chapter(state: &State, asset: &Asset, audio_id: i64, n: usize, paras: &[String], segments: &[Segment]) -> anyhow::Result<()> {
    let folder = PathBuf::from(&asset.path);
    let audio = db::asset(&state.db, audio_id).await?.context("the audiobook is gone")?;
    let track_title = audio.tracks.get(n).map(|t| t.title.clone()).unwrap_or_else(|| format!("Part {}", n + 1));
    let heading = paras.iter().find_map(|p| p.strip_prefix("# ")).map(|h| h.trim().to_string());
    let mut html = String::new();
    if heading.is_none() {
        html.push_str(&format!("<h2>{}</h2>", escape(&track_title)));
    }
    for p in paras {
        match p.strip_prefix("# ") {
            Some(h) => html.push_str(&format!("<h2>{}</h2>", escape(h.trim()))),
            None => html.push_str(&format!("<p>{}</p>", escape(&p.replace('\n', " ")))),
        }
    }
    let html_path = folder.join(format!("{n}.html"));
    let tmp = folder.join(format!("{n}.html.tmp"));
    tokio::fs::write(&tmp, &html).await?;
    tokio::fs::rename(&tmp, &html_path).await?;

    // The chapter takes the heading the narrator read, if any ("Chapter 1" → "Down the Rabbit-Hole").
    if let Some(h) = heading {
        let layout_path = folder.join("layout.json");
        if let Ok(mut layout) = serde_json::from_slice::<BookLayout>(&tokio::fs::read(&layout_path).await?) {
            if let Some(t) = layout.toc.iter_mut().find(|t| t.chapter == n) {
                t.title = h;
            }
            tokio::fs::write(&layout_path, serde_json::to_vec(&layout)?).await?;
        }
    }

    let src = book::source(asset, &state.data_dir);
    let id = asset.id;
    let sentences = tokio::task::spawn_blocking(move || book::chapter_sentences(&src, id, n)).await??;
    let heard: Vec<(String, f64, f64)> = segments.iter().flat_map(|s| s.words.iter().cloned()).collect();
    let spans = timings(&sentences, &heard, n, n);
    jobs::replace_chapter_sync(state, asset.work_id, asset.id, audio_id, n, spans).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn times_sentences_from_heard_words() {
        let heard = vec![
            ("Alice".to_string(), 0.0, 0.4),
            ("was".to_string(), 0.4, 0.6),
            ("tired.".to_string(), 0.6, 1.0),
            ("Oh".to_string(), 1.5, 1.7),
            ("dear!".to_string(), 1.7, 2.0),
        ];
        let sentences = vec!["Alice was tired.".to_string(), "“Oh, dear!”".to_string()];
        let t = super::timings(&sentences, &heard, 3, 2);
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].start, t[0].end), (0.0, 1.0));
        assert_eq!((t[1].start, t[1].end, t[1].chapter, t[1].track), (1.5, 2.0, 3, 2));
    }
}
