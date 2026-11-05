//! Background jobs. The heavy AI work runs in Python workers (./workers) so it can use the
//! current open models directly; each worker prints JSON lines like
//! `{"progress": 0.4, "message": "Transcribing chapter 3"}` and writes its results to files.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use duet_common::{Asset, AssetKind, CastMember, Job, JobKind, SyncMap, SyncSpan};
use serde::Deserialize;
use sqlx::Row;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{State, book, db, downloads, library};

pub async fn enqueue(state: &State, kind: JobKind, work_id: i64) -> anyhow::Result<i64> {
    let (id,): (i64,) = sqlx::query_as("INSERT INTO jobs (kind, work_id) VALUES (?, ?) RETURNING id")
        .bind(kind.as_str())
        .bind(work_id)
        .fetch_one(&state.db)
        .await?;
    state.jobs_wake.notify_one();
    Ok(id)
}

pub async fn list(state: &State) -> anyhow::Result<Vec<Job>> {
    let rows = sqlx::query("SELECT * FROM jobs ORDER BY id DESC LIMIT 100").fetch_all(&state.db).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let kind: String = r.get("kind");
            let parse = |s: String| chrono::DateTime::parse_from_rfc3339(&s).map(|d| d.to_utc()).unwrap_or_default();
            Some(Job {
                id: r.get("id"),
                kind: JobKind::parse(&kind)?,
                work_id: r.get("work_id"),
                status: r.get("status"),
                progress: r.get("progress"),
                message: r.get("message"),
                created_at: parse(r.get("created_at")),
                updated_at: parse(r.get("updated_at")),
            })
        })
        .collect())
}

async fn update(state: &State, id: i64, status: &str, progress: Option<f64>, message: &str) {
    let _ = sqlx::query(
        "UPDATE jobs SET status = ?, progress = COALESCE(?, progress), message = ?,
            updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
    )
    .bind(status)
    .bind(progress)
    .bind(message)
    .bind(id)
    .execute(&state.db)
    .await;
}

/// Runs queued jobs one at a time (they mostly want the whole GPU).
pub async fn run(state: State) {
    // Jobs interrupted by a restart start over.
    let _ = sqlx::query("UPDATE jobs SET status = 'queued' WHERE status = 'running'").execute(&state.db).await;
    loop {
        let next: Option<(i64, String, i64)> =
            sqlx::query_as("SELECT id, kind, work_id FROM jobs WHERE status = 'queued' ORDER BY id LIMIT 1")
                .fetch_optional(&state.db)
                .await
                .unwrap_or(None);
        let Some((id, kind, work_id)) = next else {
            state.jobs_wake.notified().await;
            continue;
        };
        update(&state, id, "running", Some(0.0), "Starting").await;
        let result = match JobKind::parse(&kind) {
            Some(kind) => run_one(&state, id, kind, work_id).await,
            None => Err(anyhow::anyhow!("unknown job kind {kind}")),
        };
        match result {
            Ok(msg) => update(&state, id, "done", Some(1.0), &msg).await,
            Err(e) => {
                tracing::warn!("job {id} failed: {e:#}");
                update(&state, id, "failed", None, &format!("{e:#}")).await;
            }
        }
    }
}

fn first(assets: &[Asset], kind: AssetKind) -> Option<&Asset> {
    // Prefer files the user added over generated ones.
    assets.iter().filter(|a| a.kind == kind).min_by_key(|a| a.origin == "generated")
}

fn readable<'a>(assets: &'a [Asset], data_dir: &Path) -> Option<&'a Asset> {
    assets.iter().filter(|a| a.kind == AssetKind::Ebook).find(|a| {
        matches!(book::source(a, data_dir), book::Source::Epub(_) | book::Source::Text(_) | book::Source::Html(_))
    })
}

async fn run_one(state: &State, id: i64, kind: JobKind, work_id: i64) -> anyhow::Result<String> {
    let work = db::work_detail(&state.db, work_id).await?.context("work not found")?;
    let dir = state.data_dir.join("jobs").join(id.to_string());
    tokio::fs::create_dir_all(&dir).await?;
    match kind {
        JobKind::Convert => {
            let src = work
                .assets
                .iter()
                .find(|a| a.kind == AssetKind::Ebook && book::CONVERTIBLE.contains(&a.format.as_str()))
                .context("no MOBI/AZW3/FB2/DOCX edition to convert")?;
            let out = book::converted_path(&state.data_dir, src.id);
            tokio::fs::create_dir_all(out.parent().unwrap()).await?;
            let status = tokio::process::Command::new("ebook-convert")
                .arg(&src.path)
                .arg(&out)
                .status()
                .await
                .context("running ebook-convert (install Calibre)")?;
            if !status.success() {
                bail!("ebook-convert failed");
            }
            Ok("Converted to EPUB".into())
        }
        JobKind::Align => {
            let ebook = readable(&work.assets, &state.data_dir).context("needs a readable ebook (EPUB/text); convert it first")?;
            let audio = first(&work.assets, AssetKind::Audiobook).context("needs an audiobook")?;
            write_book_json(state, ebook, &dir).await?;
            write_audio_json(state, audio, &dir).await?;
            let s = state.settings.read().await.ai.clone();
            worker(state, id, "align.py", &dir, &[
                "--book", "book.json", "--audio", "audio.json", "--out", "sync.json",
                "--device", &s.device, "--model", &whisper_model(&s.stt_model),
            ])
            .await?;
            store_sync(state, work_id, ebook.id, audio.id, &dir.join("sync.json")).await?;
            Ok("Text and audio are in sync".into())
        }
        JobKind::FullCast => {
            let ebook = readable(&work.assets, &state.data_dir).context("needs a readable ebook (EPUB/text); convert it first")?;
            write_book_json(state, ebook, &dir).await?;
            let s = state.settings.read().await.ai.clone();
            let out = downloads::import_dir(state, AssetKind::Audiobook).await.join(sanitize(&format!(
                "{} - {} (full cast)",
                work.author.as_deref().unwrap_or("Unknown"),
                work.title
            )));
            tokio::fs::create_dir_all(&out).await?;
            let out_s = out.to_string_lossy().to_string();
            worker(state, id, "full_cast.py", &dir, &[
                "--book", "book.json", "--title", &work.title, "--author", work.author.as_deref().unwrap_or(""),
                "--out-dir", &out_s, "--cast", "cast.json", "--sync", "sync.json",
                "--llm-url", &s.llm_url, "--llm-model", &s.llm_model, "--llm-key", &s.llm_api_key,
                "--device", &s.device,
            ])
            .await?;
            let added = library::import_path(state, &out, AssetKind::Audiobook, "generated").await?;
            let audio_id: (i64,) = sqlx::query_as("SELECT id FROM assets WHERE path = ?").bind(&out_s).fetch_one(&state.db).await?;
            if let Some(w) = added.filter(|w| *w != work_id) {
                library::link_asset(&state.db, audio_id.0, work_id).await?;
                tracing::debug!("linked generated audiobook from work {w}");
            }
            if let Ok(cast) = tokio::fs::read(dir.join("cast.json")).await {
                for m in serde_json::from_slice::<Vec<CastMember>>(&cast)? {
                    db::upsert_cast(&state.db, work_id, &m).await?;
                }
            }
            // The generator knows exactly when each sentence is spoken.
            if dir.join("sync.json").exists() {
                store_sync(state, work_id, ebook.id, audio_id.0, &dir.join("sync.json")).await?;
            }
            Ok("Full-cast audiobook created".into())
        }
        JobKind::Transcribe => {
            let audio = first(&work.assets, AssetKind::Audiobook).context("needs an audiobook")?;
            write_audio_json(state, audio, &dir).await?;
            let s = state.settings.read().await.ai.clone();
            let out = downloads::import_dir(state, AssetKind::Ebook)
                .await
                .join(sanitize(&format!("{} - {} (transcribed).epub", work.author.as_deref().unwrap_or("Unknown"), work.title)));
            tokio::fs::create_dir_all(out.parent().unwrap()).await?;
            let out_s = out.to_string_lossy().to_string();
            worker(state, id, "transcribe.py", &dir, &[
                "--audio", "audio.json", "--out", &out_s, "--title", &work.title,
                "--author", work.author.as_deref().unwrap_or(""), "--device", &s.device,
                "--model", &whisper_model(&s.stt_model),
                "--llm-url", &s.llm_url, "--llm-model", &s.llm_model, "--llm-key", &s.llm_api_key,
            ])
            .await?;
            if let Some(w) = library::import_path(state, &out, AssetKind::Ebook, "generated").await?
                && w != work_id {
                    let (asset_id,): (i64,) =
                        sqlx::query_as("SELECT id FROM assets WHERE path = ?").bind(&out_s).fetch_one(&state.db).await?;
                    library::link_asset(&state.db, asset_id, work_id).await?;
                }
            // Sentence numbering comes from the rendered EPUB, so sync it like any other book.
            if !work.has_sync {
                enqueue(state, JobKind::Align, work_id).await?;
            }
            Ok("Ebook created; syncing queued".into())
        }
    }
}

/// "Systran/faster-whisper-large-v3" -> "large-v3" for faster-whisper; other names pass through.
fn whisper_model(m: &str) -> String {
    m.rsplit('/').next().unwrap_or(m).trim_start_matches("faster-whisper-").to_string()
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect()
}

async fn write_book_json(state: &State, ebook: &Asset, dir: &Path) -> anyhow::Result<()> {
    let src = book::source(ebook, &state.data_dir);
    let id = ebook.id;
    let (chapters, layout) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        Ok((book::all_sentences(&src, id)?, book::layout(&src)?))
    })
    .await??;
    let titles: Vec<Option<String>> =
        (0..chapters.len()).map(|i| layout.toc.iter().find(|t| t.chapter == i).map(|t| t.title.clone())).collect();
    let json = serde_json::json!({
        "chapters": chapters.iter().zip(titles).enumerate().map(|(i, (s, title))| serde_json::json!({
            "index": i, "title": title, "sentences": s,
        })).collect::<Vec<_>>(),
    });
    tokio::fs::write(dir.join("book.json"), serde_json::to_vec_pretty(&json)?).await?;
    Ok(())
}

async fn write_audio_json(state: &State, audio: &Asset, dir: &Path) -> anyhow::Result<()> {
    let mut tracks = Vec::new();
    for t in &audio.tracks {
        let path = db::track_path(&state.db, audio.id, t.index).await?.context("track missing")?;
        tracks.push(serde_json::json!({ "index": t.index, "path": path, "duration": t.duration_secs, "title": t.title }));
    }
    tokio::fs::write(dir.join("audio.json"), serde_json::to_vec_pretty(&serde_json::json!({ "tracks": tracks }))?).await?;
    Ok(())
}

async fn store_sync(state: &State, work_id: i64, ebook: i64, audio: i64, path: &Path) -> anyhow::Result<()> {
    let spans: Vec<SyncSpan> = serde_json::from_slice(&tokio::fs::read(path).await.context("worker wrote no sync map")?)?;
    let map = SyncMap { ebook_asset: ebook, audiobook_asset: audio, spans };
    let dest = state.data_dir.join("sync");
    tokio::fs::create_dir_all(&dest).await?;
    let dest = dest.join(format!("{work_id}.json"));
    tokio::fs::write(&dest, serde_json::to_vec(&map)?).await?;
    sqlx::query(
        "INSERT INTO sync_maps (work_id, ebook_asset, audiobook_asset, path) VALUES (?, ?, ?, ?)
         ON CONFLICT(work_id) DO UPDATE SET ebook_asset = excluded.ebook_asset,
            audiobook_asset = excluded.audiobook_asset, path = excluded.path",
    )
    .bind(work_id)
    .bind(ebook)
    .bind(audio)
    .bind(dest.to_string_lossy().to_string())
    .execute(&state.db)
    .await?;
    Ok(())
}

pub async fn sync_map(state: &State, work_id: i64) -> anyhow::Result<Option<SyncMap>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT path FROM sync_maps WHERE work_id = ?").bind(work_id).fetch_optional(&state.db).await?;
    match row {
        Some((p,)) => Ok(Some(serde_json::from_slice(&tokio::fs::read(p).await?)?)),
        None => Ok(None),
    }
}

#[derive(Deserialize)]
struct WorkerLine {
    progress: Option<f64>,
    message: Option<String>,
}

async fn worker(state: &State, job: i64, script: &str, dir: &Path, args: &[&str]) -> anyhow::Result<()> {
    let ai = state.settings.read().await.ai.clone();
    let workers = PathBuf::from(&ai.workers_dir);
    let script_path = std::path::absolute(workers.join(script))?;
    let mut child = tokio::process::Command::new(&ai.python)
        .arg(&script_path)
        .args(args)
        .current_dir(dir)
        .env("PYTHONUNBUFFERED", "1")
        .env("DUET_TTS_URL", &ai.tts_url)
        .env("DUET_TTS_MODEL", &ai.tts_model)
        .env("DUET_STT_URL", &ai.stt_url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {} {}", ai.python, script_path.display()))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail: Vec<String> = Vec::new();
        while let Ok(Some(l)) = lines.next_line().await {
            tracing::debug!("worker: {l}");
            tail.push(l);
            if tail.len() > 20 {
                tail.remove(0);
            }
        }
        tail
    });
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        if let Ok(w) = serde_json::from_str::<WorkerLine>(&line) {
            update(state, job, "running", w.progress, w.message.as_deref().unwrap_or("")).await;
        }
    }
    let status = child.wait().await?;
    let tail = err_task.await.unwrap_or_default();
    if !status.success() {
        bail!("{script} failed: {}", tail.last().cloned().unwrap_or_else(|| status.to_string()));
    }
    Ok(())
}
