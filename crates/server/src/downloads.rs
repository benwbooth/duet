//! Downloads: torrents through qBittorrent, direct HTTP downloads from the free sources, and
//! importing finished downloads into the library.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use duet_common::{AssetKind, Download, DownloadTarget, NewDownload};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::{State, library};

fn category(state_settings: &duet_common::Settings, kind: AssetKind) -> String {
    match kind {
        AssetKind::Ebook => state_settings.qbittorrent.category_ebooks.clone(),
        AssetKind::Audiobook => state_settings.qbittorrent.category_audiobooks.clone(),
    }
}

pub async fn import_dir(state: &State, kind: AssetKind) -> PathBuf {
    let configured = state.settings.read().await.import_dir.clone();
    let base = if configured.is_empty() { state.data_dir.join("library") } else { PathBuf::from(configured) };
    base.join(match kind {
        AssetKind::Ebook => "Ebooks",
        AssetKind::Audiobook => "Audiobooks",
    })
}

pub async fn start(state: &State, req: NewDownload) -> anyhow::Result<()> {
    match &req.target {
        DownloadTarget::Torrent { url } => {
            let settings = state.settings.read().await.clone();
            state.qbit.add(&settings.qbittorrent, url, &category(&settings, req.kind)).await
        }
        DownloadTarget::Direct { url, filename } => {
            let id = format!("d{}", chrono::Utc::now().timestamp_micros());
            let source = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
            sqlx::query("INSERT INTO downloads (id, name, kind, source, state) VALUES (?, ?, ?, ?, 'downloading')")
                .bind(&id)
                .bind(&req.title)
                .bind(req.kind.as_str())
                .bind(&source)
                .execute(&state.db)
                .await?;
            let (state, url, filename, kind) = (state.clone(), url.clone(), filename.clone(), req.kind);
            let (title, author) = (req.title.clone(), req.author.clone());
            tokio::spawn(async move {
                let result = direct(&state, &id, &url, &filename, kind, &title, author.as_deref()).await;
                let (st, path) = match &result {
                    Ok(p) => ("imported", Some(p.to_string_lossy().to_string())),
                    Err(e) => {
                        tracing::warn!("download {url}: {e:#}");
                        ("failed", None)
                    }
                };
                let _ = sqlx::query("UPDATE downloads SET state = ?, path = ?, progress = CASE WHEN ? = 'imported' THEN 1 ELSE progress END WHERE id = ?")
                    .bind(st)
                    .bind(path)
                    .bind(st)
                    .bind(&id)
                    .execute(&state.db)
                    .await;
            });
            Ok(())
        }
    }
}

/// Internet Archive items: picks the best readable file from the item's metadata.
async fn resolve_archive(http: &reqwest::Client, url: &str) -> anyhow::Result<(String, String)> {
    let id = url.trim_start_matches("https://archive.org/metadata/");
    let meta: serde_json::Value = http.get(url).send().await?.error_for_status()?.json().await?;
    let files = meta["files"].as_array().context("no files in item")?;
    for ext in ["epub", "pdf", "txt"] {
        if let Some(name) = files.iter().filter_map(|f| f["name"].as_str()).find(|n| n.to_lowercase().ends_with(&format!(".{ext}")) && !n.contains("_encrypted")) {
            return Ok((format!("https://archive.org/download/{id}/{}", urlencoding::encode(name)), ext.to_string()));
        }
    }
    bail!("no EPUB, PDF or text file in this item")
}

async fn direct(
    state: &State,
    id: &str,
    url: &str,
    filename: &str,
    kind: AssetKind,
    title: &str,
    author: Option<&str>,
) -> anyhow::Result<PathBuf> {
    let (url, filename) = if url.starts_with("https://archive.org/metadata/") {
        let (u, ext) = resolve_archive(&state.http, url).await?;
        (u, format!("{filename}.{ext}"))
    } else {
        (url.to_string(), filename.to_string())
    };
    let dir = import_dir(state, kind).await;
    tokio::fs::create_dir_all(&dir).await?;
    let dest = dir.join(&filename);
    let resp = state.http.get(&url).send().await?.error_for_status()?;
    let total = resp.content_length();
    sqlx::query("UPDATE downloads SET size = ? WHERE id = ?").bind(total.map(|t| t as i64)).bind(id).execute(&state.db).await?;
    let tmp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();
    let mut done: u64 = 0;
    let mut last_report = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        if let Some(total) = total.filter(|_| last_report.elapsed().as_millis() > 500) {
            last_report = std::time::Instant::now();
            sqlx::query("UPDATE downloads SET progress = ? WHERE id = ?").bind(done as f64 / total as f64).bind(id).execute(&state.db).await?;
        }
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&tmp, &dest).await?;

    let target = if library::ext_of(&dest) == "zip" {
        // LibriVox and similar: a zip of audio tracks becomes a folder.
        let folder = dest.with_extension("");
        let (zip, out) = (dest.clone(), folder.clone());
        tokio::task::spawn_blocking(move || unzip(&zip, &out)).await??;
        tokio::fs::remove_file(&dest).await?;
        folder
    } else {
        dest
    };
    for work in import_found(state, &target, "download").await? {
        library::adopt(&state.db, work, title, author).await?;
    }
    Ok(target)
}

fn unzip(zip_path: &Path, out: &Path) -> anyhow::Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    std::fs::create_dir_all(out)?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(name) = f.enclosed_name() else { continue };
        // Flatten: audiobook zips usually hold one folder of tracks.
        let Some(file_name) = name.file_name() else { continue };
        if f.is_dir() {
            continue;
        }
        let mut dest = std::fs::File::create(out.join(file_name))?;
        std::io::copy(&mut f, &mut dest)?;
    }
    Ok(())
}

/// Imports whatever books and audiobooks are at `path` (a file or a folder); returns the works
/// they went into.
pub async fn import_found(state: &State, path: &Path, origin: &str) -> anyhow::Result<Vec<i64>> {
    let p = path.to_path_buf();
    let found = tokio::task::spawn_blocking(move || library::walk(&p)).await?;
    let mut works = Vec::new();
    for (p, kind) in found {
        if let Some(w) = library::import_path(state, &p, kind, origin).await? {
            works.push(w);
        }
    }
    Ok(works)
}

pub async fn list(state: &State) -> anyhow::Result<Vec<Download>> {
    let rows: Vec<(String, String, String, String, String, f64, Option<i64>)> = sqlx::query_as(
        "SELECT id, name, kind, source, state, progress, size FROM downloads ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.db)
    .await?;
    let mut out: Vec<Download> = rows
        .into_iter()
        .map(|(id, name, kind, source, st, progress, size)| Download {
            id,
            name,
            kind: AssetKind::parse(&kind).unwrap_or(AssetKind::Ebook),
            progress,
            state: st,
            size,
            source,
        })
        .collect();
    let settings = state.settings.read().await.clone();
    if settings.qbittorrent.enabled {
        for kind in [AssetKind::Ebook, AssetKind::Audiobook] {
            for t in state.qbit.list(&settings.qbittorrent, &category(&settings, kind)).await? {
                out.push(Download {
                    id: t.hash,
                    name: t.name,
                    kind,
                    progress: t.progress,
                    state: t.state,
                    size: Some(t.size),
                    source: "qBittorrent".into(),
                });
            }
        }
    }
    Ok(out)
}

/// Imports finished torrents in duet's categories, once each.
pub async fn watch_torrents(state: State) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        if let Err(e) = import_finished(&state).await {
            tracing::debug!("torrent import: {e:#}");
        }
    }
}

async fn import_finished(state: &State) -> anyhow::Result<()> {
    let settings = state.settings.read().await.clone();
    if !settings.qbittorrent.enabled || !settings.qbittorrent.auto_import {
        return Ok(());
    }
    for kind in [AssetKind::Ebook, AssetKind::Audiobook] {
        for t in state.qbit.list(&settings.qbittorrent, &category(&settings, kind)).await? {
            if !t.finished() || t.content_path.is_empty() {
                continue;
            }
            let done: Option<(String,)> =
                sqlx::query_as("SELECT hash FROM imported_torrents WHERE hash = ?").bind(&t.hash).fetch_optional(&state.db).await?;
            if done.is_some() {
                continue;
            }
            let n = import_found(state, Path::new(&t.content_path), "download").await?.len();
            tracing::info!("imported {n} item(s) from torrent {}", t.name);
            sqlx::query("INSERT INTO imported_torrents (hash) VALUES (?)").bind(&t.hash).execute(&state.db).await?;
        }
    }
    Ok(())
}
