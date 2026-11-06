//! Background jobs. The heavy AI work runs in Python workers (./workers) so it can use the
//! current open models directly; each worker prints JSON lines like
//! `{"progress": 0.4, "message": "Transcribing chapter 3"}` and writes its results to files.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use duet_common::{Asset, AssetKind, Job, JobKind, SyncMap, SyncSpan};
use sqlx::Row;

use crate::{State, book, db};

/// Queues a job; for syncing, `ebook`/`audio` choose the editions to pair.
pub async fn enqueue_pair(state: &State, kind: JobKind, work_id: i64, ebook: Option<i64>, audio: Option<i64>) -> anyhow::Result<i64> {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO jobs (kind, work_id, ebook_asset, audiobook_asset) VALUES (?, ?, ?, ?) RETURNING id",
    )
    .bind(kind.as_str())
    .bind(work_id)
    .bind(ebook)
    .bind(audio)
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
        let next: Option<(i64, String, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT id, kind, work_id, ebook_asset, audiobook_asset FROM jobs WHERE status = 'queued' ORDER BY id LIMIT 1",
        )
                .fetch_optional(&state.db)
                .await
                .unwrap_or(None);
        let Some((id, kind, work_id, ebook, audio)) = next else {
            state.jobs_wake.notified().await;
            continue;
        };
        update(&state, id, "running", Some(0.0), "Starting").await;
        let result = match JobKind::parse(&kind) {
            Some(kind) => run_one(&state, id, kind, work_id, ebook, audio).await,
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



async fn run_one(
    state: &State,
    id: i64,
    kind: JobKind,
    work_id: i64,
    ebook_choice: Option<i64>,
    _audio_choice: Option<i64>,
) -> anyhow::Result<String> {
    let work = db::work_detail(&state.db, work_id).await?.context("work not found")?;
    let dir = state.data_dir.join("jobs").join(id.to_string());
    tokio::fs::create_dir_all(&dir).await?;
    match kind {
        JobKind::Convert => {
            let src = work
                .assets
                .iter()
                .filter(|a| a.kind == AssetKind::Ebook && book::CONVERTIBLE.contains(&a.format.as_str()))
                .find(|a| ebook_choice.is_none_or(|c| c == a.id))
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
    }
}




/// The book's sentences as the workers want them, kept between runs (rebuilt if the file is newer).
pub async fn book_json_cached(state: &State, ebook: &Asset) -> anyhow::Result<PathBuf> {
    let dir = state.data_dir.join("cache").join("books").join(ebook.id.to_string());
    let out = dir.join("book.json");
    let fresh = match (tokio::fs::metadata(&out).await, tokio::fs::metadata(&ebook.path).await) {
        (Ok(o), Ok(src)) => o.modified().ok() >= src.modified().ok(),
        _ => false,
    };
    if !fresh {
        tokio::fs::create_dir_all(&dir).await?;
        write_book_json(state, ebook, &dir).await?;
    }
    Ok(std::path::absolute(out)?)
}

/// Adds spans to a pairing's sync map (creating it), replacing any for the same sentences.
pub async fn merge_sync(state: &State, work_id: i64, ebook: i64, audio: i64, new: Vec<SyncSpan>) -> anyhow::Result<()> {
    if new.is_empty() {
        return Ok(());
    }
    let fresh: std::collections::HashSet<(usize, usize)> = new.iter().map(|s| (s.chapter, s.sentence)).collect();
    save_spans(state, work_id, ebook, audio, new, |s| !fresh.contains(&(s.chapter, s.sentence))).await
}

/// Replaces a whole chapter's spans (its sentences were renumbered, say).
pub async fn replace_chapter_sync(state: &State, work_id: i64, ebook: i64, audio: i64, chapter: usize, new: Vec<SyncSpan>) -> anyhow::Result<()> {
    save_spans(state, work_id, ebook, audio, new, |s| s.chapter != chapter).await
}

async fn save_spans(
    state: &State,
    work_id: i64,
    ebook: i64,
    audio: i64,
    new: Vec<SyncSpan>,
    keep: impl Fn(&SyncSpan) -> bool,
) -> anyhow::Result<()> {
    let mut spans = sync_map(state, ebook, audio).await?.map(|m| m.spans).unwrap_or_default();
    spans.retain(|s| keep(s));
    spans.extend(new);
    let dir = state.data_dir.join("jobs").join("live");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{work_id}-{ebook}-{audio}.json"));
    tokio::fs::write(&path, serde_json::to_vec(&spans)?).await?;
    store_sync(state, work_id, ebook, audio, &path).await
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


async fn store_sync(state: &State, work_id: i64, ebook: i64, audio: i64, path: &Path) -> anyhow::Result<()> {
    let spans: Vec<SyncSpan> = serde_json::from_slice(&tokio::fs::read(path).await.context("worker wrote no sync map")?)?;
    let map = SyncMap { ebook_asset: ebook, audiobook_asset: audio, spans };
    let dest = state.data_dir.join("sync");
    tokio::fs::create_dir_all(&dest).await?;
    let dest = dest.join(format!("{work_id}-{ebook}-{audio}.json"));
    tokio::fs::write(&dest, serde_json::to_vec(&map)?).await?;
    sqlx::query(
        "INSERT INTO sync_maps (work_id, ebook_asset, audiobook_asset, path) VALUES (?, ?, ?, ?)
         ON CONFLICT(ebook_asset, audiobook_asset) DO UPDATE SET path = excluded.path",
    )
    .bind(work_id)
    .bind(ebook)
    .bind(audio)
    .bind(dest.to_string_lossy().to_string())
    .execute(&state.db)
    .await?;
    Ok(())
}

/// The sync map for one ebook/audiobook pairing.
pub async fn sync_map(state: &State, ebook: i64, audio: i64) -> anyhow::Result<Option<SyncMap>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT path FROM sync_maps WHERE ebook_asset = ? AND audiobook_asset = ?")
        .bind(ebook)
        .bind(audio)
        .fetch_optional(&state.db)
        .await?;
    match row {
        Some((p,)) => Ok(Some(serde_json::from_slice(&tokio::fs::read(p).await?)?)),
        None => Ok(None),
    }
}

/// Which ebook/audiobook pairings of a work are synced: (ebook asset, audiobook asset).
pub async fn synced_pairs(state: &State, work_id: i64) -> anyhow::Result<Vec<(i64, i64)>> {
    Ok(sqlx::query_as("SELECT ebook_asset, audiobook_asset FROM sync_maps WHERE work_id = ?")
        .bind(work_id)
        .fetch_all(&state.db)
        .await?)
}



