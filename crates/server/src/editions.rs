//! "Find another edition": fetching a different ebook or audiobook for a book that's already in
//! the library, either alongside what it has or in place of it.
//!
//! The download itself goes through `downloads::start` like any other. What's different is
//! where it lands: an `edition_targets` row, keyed by the download, says which book it belongs
//! to, so when it finishes it joins that book (keeping its title, cover and your place) instead
//! of becoming a new one. Replaced editions leave the library but their files stay on disk;
//! their paths go in `hidden_paths` so scans don't bring them back.

use std::path::Path;

use axum::{
    Json, Router,
    extract::{Path as UrlPath, State as AxState},
    http::StatusCode,
    routing::{get, put},
};
use duet_common::{Asset, AssetKind, DownloadTarget, NewDownload, Position, text::normalize};
use serde::{Deserialize, Serialize};

use crate::{
    State, book, db, downloads,
    error::{ApiError, ApiResult},
    library,
};

pub fn routes() -> Router<State> {
    Router::new()
        .route("/works/{id}/editions", get(pending).post(fetch))
        .route("/works/{id}/default-edition", put(set_default))
}

#[derive(Deserialize)]
struct DefaultEdition {
    asset: i64,
}

/// Makes an edition the one the book opens with (its kind decides ebook or recording).
async fn set_default(AxState(s): AxState<State>, UrlPath(id): UrlPath<i64>, Json(req): Json<DefaultEdition>) -> ApiResult<StatusCode> {
    let asset = db::asset(&s.db, req.asset).await?.filter(|a| a.work_id == id).ok_or_else(|| ApiError::not_found("edition"))?;
    let column = match asset.kind {
        AssetKind::Ebook => "default_ebook",
        AssetKind::Audiobook => "default_audio",
    };
    sqlx::query(&format!("UPDATE works SET {column} = ? WHERE id = ?"))
        .bind(asset.id)
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct FetchEdition {
    download: NewDownload,
    /// Take the book's current editions of this kind out of the library once the new one is in.
    #[serde(default)]
    replace: bool,
}

/// An edition on its way to a book.
#[derive(Serialize)]
struct Pending {
    kind: AssetKind,
    replace: bool,
    since: String,
}

async fn pending(AxState(s): AxState<State>, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Vec<Pending>>> {
    // Anything older than a day was abandoned (a torrent that never finished, say).
    sqlx::query("DELETE FROM edition_targets WHERE created_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-1 day')")
        .execute(&s.db)
        .await
        .map_err(anyhow::Error::from)?;
    let rows: Vec<(String, i64, String)> =
        sqlx::query_as("SELECT kind, replace, created_at FROM edition_targets WHERE work_id = ? ORDER BY created_at")
            .bind(id)
            .fetch_all(&s.db)
            .await
            .map_err(anyhow::Error::from)?;
    Ok(Json(
        rows.into_iter()
            .map(|(kind, replace, since)| Pending { kind: AssetKind::parse(&kind).unwrap_or(AssetKind::Ebook), replace: replace != 0, since })
            .collect(),
    ))
}

async fn fetch(AxState(s): AxState<State>, UrlPath(id): UrlPath<i64>, Json(req): Json<FetchEdition>) -> ApiResult<StatusCode> {
    let work = db::work_detail(&s.db, id).await?.ok_or_else(|| ApiError::not_found("book"))?;
    start(&s, id, &work.title, work.author.clone(), req.download, req.replace).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(StatusCode::ACCEPTED)
}

async fn register(state: &State, key: &str, work_id: i64, kind: AssetKind, replace: bool) -> anyhow::Result<()> {
    sqlx::query("INSERT OR REPLACE INTO edition_targets (key, work_id, kind, replace) VALUES (?, ?, ?, ?)")
        .bind(key)
        .bind(work_id)
        .bind(kind.as_str())
        .bind(replace)
        .execute(&state.db)
        .await?;
    Ok(())
}

fn direct_key(kind: AssetKind, title: &str) -> String {
    format!("direct:{}:{title}", kind.as_str())
}

fn torrent_key(hash: &str) -> String {
    format!("torrent:{}", hash.to_ascii_lowercase())
}

fn magnet_hash(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let rest = &lower[lower.find("btih:")? + 5..];
    let hash: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    (hash.len() == 40).then_some(hash)
}

fn category(settings: &duet_common::Settings, kind: AssetKind) -> String {
    match kind {
        AssetKind::Ebook => settings.qbittorrent.category_ebooks.clone(),
        AssetKind::Audiobook => settings.qbittorrent.category_audiobooks.clone(),
    }
}

/// Starts fetching `req` for book `work_id`.
async fn start(state: &State, work_id: i64, title: &str, author: Option<String>, mut req: NewDownload, replace: bool) -> anyhow::Result<()> {
    // Filed under the book's own title, so the download pane names the book you're on.
    req.title = title.to_string();
    req.author = author;
    let kind = req.kind;
    match &mut req.target {
        DownloadTarget::Direct { filename, .. } => {
            // Never write over a file already in the library (another edition of this book
            // can easily share its file name).
            let dir = downloads::import_dir(state, kind).await;
            *filename = unique_name(&dir, filename);
            register(state, &direct_key(kind, title), work_id, kind, replace).await?;
            if let Err(e) = downloads::start(state, req).await {
                let _ = sqlx::query("DELETE FROM edition_targets WHERE key = ?").bind(direct_key(kind, title)).execute(&state.db).await;
                return Err(e);
            }
            Ok(())
        }
        // Torrents of any shape: a magnet or a .torrent link in `url`.
        target => {
            let url = serde_json::to_value(&*target)?["url"].as_str().unwrap_or_default().to_string();
            let url = &url;
            let settings = state.settings.read().await.clone();
            if !settings.qbittorrent.enabled {
                anyhow::bail!("Turn on qBittorrent in Settings to get torrents");
            }
            let cat = category(&settings, kind);
            if let Some(hash) = magnet_hash(url) {
                register(state, &torrent_key(&hash), work_id, kind, replace).await?;
                return downloads::start(state, req).await;
            }
            // A .torrent link: its hash isn't known until qBittorrent has it, so see which
            // torrent turns up in duet's category.
            let before: Vec<String> = state.qbit.list(&settings.qbittorrent, &cat).await?.into_iter().map(|t| t.hash).collect();
            downloads::start(state, req).await?;
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                let now = state.qbit.list(&settings.qbittorrent, &cat).await?;
                if let Some(t) = now.iter().find(|t| !before.contains(&t.hash)) {
                    return register(state, &torrent_key(&t.hash), work_id, kind, replace).await;
                }
            }
            anyhow::bail!("qBittorrent took the torrent, but duet couldn't tell which one it is, so it will arrive as a separate book")
        }
    }
}

/// `name`, or `name (2)` and so on when a file by that name is already in `dir`.
fn unique_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && e.len() <= 5 => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..).map(|n| format!("{stem} ({n}){ext}")).find(|n| !dir.join(n).exists()).unwrap()
}

async fn take(state: &State, key: &str) -> anyhow::Result<Option<(i64, AssetKind, bool)>> {
    let row: Option<(i64, String, bool)> =
        sqlx::query_as("DELETE FROM edition_targets WHERE key = ? RETURNING work_id, kind, replace").bind(key).fetch_optional(&state.db).await?;
    Ok(row.map(|(w, k, r)| (w, AssetKind::parse(&k).unwrap_or(AssetKind::Ebook), r)))
}

/// After a direct download lands at `path`: into the book it was fetched for, or (an ordinary
/// download) into the library the usual way.
pub async fn after_direct(state: &State, path: &Path, kind: AssetKind, title: &str, author: Option<&str>) -> anyhow::Result<()> {
    if let Some((work_id, _, replace)) = take(state, &direct_key(kind, title)).await? {
        return place(state, path, work_id, kind, replace).await;
    }
    for work in downloads::import_found(state, path, "download").await? {
        library::adopt(&state.db, work, title, author).await?;
    }
    Ok(())
}

/// After a torrent finishes: the same, keyed by its hash. Returns how many things were imported.
pub async fn after_torrent(state: &State, hash: &str, path: &Path) -> anyhow::Result<usize> {
    if let Some((work_id, kind, replace)) = take(state, &torrent_key(hash)).await? {
        place(state, path, work_id, kind, replace).await?;
        return Ok(1);
    }
    Ok(downloads::import_found(state, path, "download").await?.len())
}

/// Imports what's at `path` into book `work_id`; with `replace`, the book's earlier editions of
/// that kind leave the library and your place carries over to the new one.
async fn place(state: &State, path: &Path, work_id: i64, kind: AssetKind, replace: bool) -> anyhow::Result<()> {
    let before: Vec<Asset> = db::work_assets(&state.db, work_id)
        .await?
        .into_iter()
        .filter(|a| a.kind == kind && a.origin != "generated" && a.format != "transcript")
        .collect();
    let p = path.to_path_buf();
    tokio::task::spawn_blocking({
        let p = p.clone();
        move || downloads::sniff_extensionless(&p)
    })
    .await?;
    let found = tokio::task::spawn_blocking(move || library::walk(&p)).await?;
    let mut added = Vec::new();
    for (p, k) in found {
        if k != kind {
            continue;
        }
        let imported_into = library::import_path(state, &p, k, "download").await?;
        let path_str = p.to_string_lossy().to_string();
        let Some((asset_id, home)): Option<(i64, i64)> =
            sqlx::query_as("SELECT id, work_id FROM assets WHERE path = ?").bind(&path_str).fetch_optional(&state.db).await?
        else {
            continue;
        };
        if home != work_id {
            // Jobs queued for the stopgap book it was imported into (an EPUB conversion, say)
            // follow it, or they'd vanish with that book.
            if imported_into == Some(home) {
                sqlx::query("UPDATE jobs SET work_id = ? WHERE work_id = ?").bind(work_id).bind(home).execute(&state.db).await?;
            }
            library::link_asset(&state.db, asset_id, work_id).await?;
        }
        added.push(asset_id);
    }
    if added.is_empty() {
        anyhow::bail!("the download had no {} in it", if kind == AssetKind::Ebook { "ebook" } else { "audiobook" });
    }
    if !replace {
        return Ok(());
    }
    let old: Vec<Asset> = before.into_iter().filter(|a| !added.contains(&a.id)).collect();
    if old.is_empty() {
        return Ok(());
    }
    if let Some(new) = db::asset(&state.db, added[0]).await? {
        if let Err(e) = carry_place(state, work_id, &old[0], &new).await {
            tracing::warn!("couldn't carry your place over to the new edition: {e:#}");
        }
    }
    for a in &old {
        sqlx::query("INSERT OR IGNORE INTO hidden_paths (path) VALUES (?)").bind(&a.path).execute(&state.db).await?;
        sqlx::query("DELETE FROM assets WHERE id = ?").bind(a.id).execute(&state.db).await?;
        tracing::info!("edition {} replaced; its file stays at {}", a.id, a.path);
    }
    Ok(())
}

/// Moves the saved place from `old` to the matching spot in `new`: the same sentence when the
/// text can be found (else the same share of the way through) for ebooks, the same share of the
/// running time for audiobooks.
async fn carry_place(state: &State, work_id: i64, old: &Asset, new: &Asset) -> anyhow::Result<()> {
    let Some(mut pos) = db::get_position(&state.db, work_id).await? else { return Ok(()) };
    match new.kind {
        AssetKind::Audiobook => {
            let durations = |a: &Asset| a.tracks.iter().map(|t| t.duration_secs).collect::<Vec<f64>>();
            let (od, nd) = (durations(old), durations(new));
            let (ot, nt): (f64, f64) = (od.iter().sum(), nd.iter().sum());
            if ot <= 0.0 || nt <= 0.0 {
                return Ok(());
            }
            let at = od.iter().take(pos.track).sum::<f64>() + pos.seconds;
            let (track, seconds) = locate_time(&nd, at / ot * nt);
            pos.track = track;
            pos.seconds = seconds;
        }
        AssetKind::Ebook => {
            let (old_src, new_src) = (book::source(old, &state.data_dir), book::source(new, &state.data_dir));
            let (oid, nid) = (old.id, new.id);
            let at = pos.clone();
            let moved = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<(usize, usize)>> {
                let old_s = book::all_sentences(&old_src, oid)?;
                let new_s = match book::all_sentences(&new_src, nid) {
                    Ok(s) => s,
                    // Not readable yet (waiting for its EPUB conversion): leave the place be.
                    Err(_) => return Ok(None),
                };
                Ok(match_place(&old_s, &new_s, &at))
            })
            .await??;
            let Some((chapter, sentence)) = moved else { return Ok(()) };
            pos.chapter = chapter;
            pos.sentence = sentence;
        }
    }
    db::set_position(&state.db, work_id, pos).await
}

/// Track and seconds into it for `at` seconds into the whole recording.
fn locate_time(durations: &[f64], at: f64) -> (usize, f64) {
    let mut left = at.max(0.0);
    for (i, d) in durations.iter().enumerate() {
        if left < *d || i + 1 == durations.len() {
            return (i, left.min(*d));
        }
        left -= d;
    }
    (0, 0.0)
}

/// The words starting at sentence `i` of `flat`, at least `len` characters of them.
fn key_at(flat: &[String], i: usize, len: usize) -> String {
    let mut k = String::new();
    for s in &flat[i..] {
        if !k.is_empty() && !s.is_empty() {
            k.push(' ');
        }
        k.push_str(s);
        if k.len() >= len {
            break;
        }
    }
    k.chars().take(len).collect()
}

/// Where the reader's place in `old` is in `new`.
fn match_place(old: &[Vec<String>], new: &[Vec<String>], pos: &Position) -> Option<(usize, usize)> {
    let flatten = |b: &[Vec<String>]| -> Vec<(usize, usize, String)> {
        b.iter().enumerate().flat_map(|(c, ss)| ss.iter().enumerate().map(move |(s, t)| (c, s, normalize(t)))).collect()
    };
    let (of, nf) = (flatten(old), flatten(new));
    if nf.is_empty() {
        return None;
    }
    let oi = of.iter().position(|(c, s, _)| (*c, *s) >= (pos.chapter, pos.sentence)).unwrap_or(of.len().saturating_sub(1));
    let share = if of.is_empty() { pos.fraction } else { oi as f64 / of.len() as f64 };
    let guess = ((share * nf.len() as f64) as usize).min(nf.len() - 1);
    let otext: Vec<String> = of.iter().map(|x| x.2.clone()).collect();
    let ntext: Vec<String> = nf.iter().map(|x| x.2.clone()).collect();
    if !otext.is_empty() {
        // The same run of words (sentences split differently between editions, so compare a
        // stretch of text, not one sentence); the hit nearest the expected spot wins.
        let want = key_at(&otext, oi, 48);
        if want.len() >= 12 {
            let hit = (0..ntext.len())
                .filter(|&j| ntext[j].starts_with(want.split(' ').next().unwrap_or("")) && key_at(&ntext, j, want.len()) == want)
                .min_by_key(|&j| j.abs_diff(guess));
            if let Some(j) = hit {
                return Some((nf[j].0, nf[j].1));
            }
        }
    }
    Some((nf[guess].0, nf[guess].1))
}

/// True when a replaced edition lived at `path`, so the library leaves it out.
pub async fn is_hidden(db: &sqlx::SqlitePool, path: &str) -> anyhow::Result<bool> {
    let row: Option<(String,)> = sqlx::query_as("SELECT path FROM hidden_paths WHERE path = ?").bind(path).fetch_optional(db).await?;
    Ok(row.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(chapters: &[&[&str]]) -> Vec<Vec<String>> {
        chapters.iter().map(|c| c.iter().map(|s| s.to_string()).collect()).collect()
    }

    #[test]
    fn finds_the_same_sentence_in_another_edition() {
        let old = book(&[&["Chapter One.", "It was a dark and stormy night.", "The rain fell in torrents."], &["Chapter Two.", "Morning came at last."]]);
        let new = book(&[&["Title page."], &["I.", "It was a dark and stormy night; the rain fell in torrents."], &["II.", "Morning came at last."]]);
        let pos = Position { chapter: 0, sentence: 1, ..Default::default() };
        assert_eq!(match_place(&old, &new, &pos), Some((1, 1)));
        let pos = Position { chapter: 1, sentence: 1, ..Default::default() };
        assert_eq!(match_place(&old, &new, &pos), Some((2, 1)));
    }

    #[test]
    fn times_carry_by_share() {
        assert_eq!(locate_time(&[10.0, 10.0], 15.0), (1, 5.0));
        assert_eq!(locate_time(&[10.0, 10.0], 25.0), (1, 10.0));
    }
}
