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
    // Resolve the real file URL. LibGen's get.php keys are single-use-ish
    // and its CDN nodes flake (500s), so resolve fresh on every attempt.
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=3 {
        let attempt_url = if url.starts_with("https://archive.org/metadata/") && attempt == 1 {
            let site = state.site_client().await;
            let (u, ext) = resolve_archive(&site, url).await?;
            (u, format!("{filename}.{ext}"))
        } else {
            let settings = state.settings.read().await.clone();
            let mirror = settings.annas_archive.mirror.trim_end_matches('/').to_string();
            let key = settings.annas_archive.secret_key.clone();
            // All download resolutions go through the site client (proxy when set).
            let site = state.site_client().await;
            if url.starts_with(&format!("{mirror}/md5/")) || (mirror.contains("annas-archive") && url.contains("/md5/")) {
                let md5 = url.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("").to_string();
                let download_url = state.annas.resolve_download(&site, &mirror, &key, &md5).await?;
                (download_url, filename.to_string())
            } else if url.contains("/ads.php?md5=") {
                let download_url = crate::libgen::resolve_download(&site, url).await?;
                (download_url, filename.to_string())
            } else {
                (url.to_string(), filename.to_string())
            }
        };
        match fetch_one(state, id, &attempt_url.0, &attempt_url.1, kind, title, author).await {
            Ok(p) => return Ok(p),
            Err(e) => {
                tracing::warn!("download {url} attempt {attempt}: {e:#}");
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("download failed")))
}

async fn fetch_one(state: &State, id: &str, url: &str, filename: &str, kind: AssetKind, title: &str, author: Option<&str>) -> anyhow::Result<PathBuf> {
    let (url, filename) = (url.to_string(), filename.to_string());
    let dir = import_dir(state, kind).await;
    tokio::fs::create_dir_all(&dir).await?;
    let dest = dir.join(&filename);
    let site = state.site_client().await;
    let resp = site.get(&url).send().await?.error_for_status()?;
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
    // Into the book it was fetched for ("Find another edition"), or a book of its own.
    crate::editions::after_direct(state, &target, kind, title, author).await?;
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
    // Bulk torrents store books under bare AACIDs (no extension), so sniff
    // those by magic bytes and give them a real one before walking.
    let p = path.to_path_buf();
    let sniff_path = p.clone();
    tokio::task::spawn_blocking(move || sniff_extensionless(&sniff_path)).await?;
    let found = tokio::task::spawn_blocking(move || library::walk(&p)).await?;
    let mut works = Vec::new();
    for (p, kind) in found {
        if let Some(w) = library::import_path(state, &p, kind, origin).await? {
            works.push(w);
        }
    }
    Ok(works)
}

/// Renames bare-AACID files (no extension) by their magic bytes so the
/// library walker recognises them: PDF, ZIP-based (epub), MOBI, DJVU.
pub fn sniff_extensionless(path: &Path) {
    fn visit(path: &Path) {
        if !path.exists() {
            return;
        }
        if path.is_file() {
            if path.extension().is_none() {
                if let Ok(bytes) = std::fs::read(path).map(|b| b.into_iter().take(128).collect::<Vec<_>>()) {
                    let ext = if bytes.starts_with(b"%PDF") {
                        Some("pdf")
                    } else if bytes.starts_with(b"PK\x03\x04") {
                        Some("epub")
                    } else if bytes.windows(8).any(|w| w == b"BOOKMOBI") {
                        Some("mobi")
                    } else if bytes.starts_with(b"DJVU") {
                        Some("djvu")
                    } else {
                        None
                    };
                    if let Some(ext) = ext {
                        let name = path.to_string_lossy().into_owned();
                        let renamed = format!("{name}.{ext}");
                        if !Path::new(&renamed).exists() {
                            let _ = std::fs::rename(path, &renamed);
                        }
                    }
                }
            }
            return;
        }
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                visit(&entry.path());
            }
        }
    }
    visit(path);
}

pub async fn list(state: &State) -> anyhow::Result<Vec<Download>> {
    let rows: Vec<(String, String, String, String, String, f64, Option<i64>, String)> = sqlx::query_as(
        "SELECT id, name, kind, source, state, progress, size, created_at FROM downloads ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.db)
    .await?;
    let mut out: Vec<Download> = rows
        .into_iter()
        .map(|(id, name, kind, source, st, progress, size, created_at)| Download {
            id,
            name,
            kind: AssetKind::parse(&kind).unwrap_or(AssetKind::Ebook),
            progress,
            state: st,
            size,
            source,
            started_at: created_at
                .parse::<chrono::DateTime<chrono::Utc>>()
                .ok()
                .map(|t| t.timestamp()),
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
                    started_at: (t.added_on > 0).then_some(t.added_on),
                });
            }
        }
    }
    // Newest started first, so the pane reads in start order.
    out.sort_by_key(|d| std::cmp::Reverse(d.started_at.unwrap_or(0)));
    Ok(out)
}

/// Dismisses one finished item from the pane. Direct downloads in a terminal
/// state have their row deleted; duet-managed torrents are removed from
/// qBittorrent (files stay on disk — imports already happened).
pub async fn clear(state: &State, id: &str) -> anyhow::Result<()> {
    let done: Option<(String,)> =
        sqlx::query_as("DELETE FROM downloads WHERE id = ? AND state IN ('imported', 'failed') RETURNING id")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    if done.is_some() {
        return Ok(());
    }
    let settings = state.settings.read().await.clone();
    if !settings.qbittorrent.enabled {
        bail!("nothing to clear");
    }
    // Only duet's own categories: never touch foreign torrents.
    for kind in [AssetKind::Ebook, AssetKind::Audiobook] {
        for t in state.qbit.list(&settings.qbittorrent, &category(&settings, kind)).await? {
            if t.hash.eq_ignore_ascii_case(id) && (!t.state.eq_ignore_ascii_case("downloading") || t.finished()) {
                state.qbit.remove(&settings.qbittorrent, &t.hash).await?;
                return Ok(());
            }
        }
    }
    bail!("that download is still running")
}

/// qBittorrent inside Docker saves into its container `/downloads`; on the
/// host the same folder is `Settings → qBittorrent → Host save path`.
fn to_host_path(settings: &duet_common::QbitSettings, container_path: &str) -> PathBuf {
    let host = settings.host_save_path.trim();
    if host.is_empty() {
        return PathBuf::from(container_path);
    }
    if let Some(rest) = container_path.strip_prefix("/downloads") {
        let mut p = PathBuf::from(host);
        if let Some(rel) = rest.strip_prefix('/') {
            p.push(rel);
        }
        return p;
    }
    PathBuf::from(container_path)
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
            let n = crate::editions::after_torrent(state, &t.hash, &to_host_path(&settings.qbittorrent, &t.content_path)).await?;
            tracing::info!("imported {n} item(s) from torrent {}", t.name);
            sqlx::query("INSERT INTO imported_torrents (hash) VALUES (?)").bind(&t.hash).execute(&state.db).await?;
        }
    }
    Ok(())
}
