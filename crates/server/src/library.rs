//! Library scanning: finds ebooks and audiobooks, reads their metadata and links the editions of
//! the same book into one work.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::Context;
use duet_common::{AssetKind, ScanReport, text};
use lofty::{file::{AudioFile, TaggedFileExt}, tag::Accessor};
use sqlx::SqlitePool;

use crate::{State, images};

pub const EBOOK_EXTS: &[&str] = &[
    "epub", "pdf", "txt", "md", "html", "htm", "xhtml", "cbz", "cbr", "cb7", "cbt", "mobi", "azw", "azw3", "azw4", "prc",
    "pdb", "fb2", "fbz", "docx", "rtf", "odt", "lit", "lrf", "chm", "djvu", "htmlz", "txtz", "pml", "rb", "snb", "tcr",
];
pub const AUDIO_EXTS: &[&str] = &["m4b", "m4a", "mp3", "aac", "ogg", "oga", "opus", "flac", "wav"];

pub fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// Metadata found for one asset before it's attached to a work.
#[derive(Default, Debug)]
pub struct Meta {
    pub title: String,
    pub author: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub description: Option<String>,
    /// Publication year, when the file states one (EPUBs and comics often do).
    pub year: Option<i32>,
    pub cover: Option<Vec<u8>>,
}

pub struct FoundTrack {
    pub path: PathBuf,
    pub title: String,
    pub duration: f64,
}

pub async fn scan(state: &State) -> anyhow::Result<ScanReport> {
    let dirs = state.settings.read().await.library_dirs.clone();
    let mut report = ScanReport { works: 0, ebooks: 0, audiobooks: 0, errors: Vec::new() };
    for dir in dirs {
        let dir = PathBuf::from(dir);
        let found = tokio::task::spawn_blocking(move || walk(&dir)).await?;
        for (path, kind) in found {
            match import_path(state, &path, kind, "scan").await {
                Ok(Some(_)) => match kind {
                    AssetKind::Ebook => report.ebooks += 1,
                    AssetKind::Audiobook => report.audiobooks += 1,
                },
                Ok(None) => {}
                Err(e) => report.errors.push(format!("{}: {e:#}", path.display())),
            }
        }
    }
    // Drop assets whose files are gone, then works with nothing left.
    let paths: Vec<(i64, String)> = sqlx::query_as("SELECT id, path FROM assets").fetch_all(&state.db).await?;
    for (id, path) in paths {
        if !Path::new(&path).exists() {
            sqlx::query("DELETE FROM assets WHERE id = ?").bind(id).execute(&state.db).await?;
        }
    }
    sqlx::query("DELETE FROM works WHERE NOT EXISTS (SELECT 1 FROM assets WHERE assets.work_id = works.id)")
        .execute(&state.db)
        .await?;
    backfill_years(state).await;
    // New books' details looked up online, in the background.
    {
        let st = state.clone();
        tokio::spawn(async move { crate::metadata::backfill(&st).await });
    }
    let (works,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM works").fetch_one(&state.db).await?;
    report.works = works as usize;
    Ok(report)
}

/// Reads publication years for works that predate year tracking (or whose
/// editions didn't state one), from their first ebook file.
pub async fn backfill_years(state: &State) {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT w.id, a.path FROM works w JOIN assets a ON a.work_id = w.id AND a.kind = 'ebook'
         WHERE w.year IS NULL GROUP BY w.id",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for (id, path) in rows {
        let p = PathBuf::from(path);
        let year = tokio::task::spawn_blocking(move || ebook_meta(&p).year).await.unwrap_or_default();
        if let Some(y) = year {
            let _ = sqlx::query("UPDATE works SET year = ? WHERE id = ? AND year IS NULL")
                .bind(y)
                .bind(id)
                .execute(&state.db)
                .await;
        }
    }
}

/// Every ebook file, and every audiobook (a folder of tracks, or a single .m4b), under `dir`.
pub fn walk(dir: &Path) -> Vec<(PathBuf, AssetKind)> {
    let mut out = Vec::new();
    let mut audio_dirs: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for entry in walkdir::WalkDir::new(dir).follow_links(true).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let p = entry.path().to_path_buf();
        let ext = ext_of(&p);
        if EBOOK_EXTS.contains(&ext.as_str()) {
            out.push((p, AssetKind::Ebook));
        } else if AUDIO_EXTS.contains(&ext.as_str()) {
            audio_dirs.entry(p.parent().unwrap_or(dir).to_path_buf()).or_default().push(p);
        }
    }
    for (folder, files) in audio_dirs {
        let m4bs = files.iter().filter(|f| ext_of(f) == "m4b").count();
        if m4bs > 1 && m4bs == files.len() {
            // A folder of separate .m4b books.
            out.extend(files.into_iter().map(|f| (f, AssetKind::Audiobook)));
        } else if files.len() == 1 {
            out.push((files.into_iter().next().unwrap(), AssetKind::Audiobook));
        } else {
            out.push((folder, AssetKind::Audiobook));
        }
    }
    out
}

/// Adds one asset (if new) and links it to a matching work, creating the work if needed.
/// Returns the work id when something was added.
pub async fn import_path(state: &State, path: &Path, kind: AssetKind, origin: &str) -> anyhow::Result<Option<i64>> {
    let path_str = path.to_string_lossy().to_string();
    let exists: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM assets WHERE path = ?").bind(&path_str).fetch_optional(&state.db).await?;
    if exists.is_some() {
        return Ok(None);
    }
    let p = path.to_path_buf();
    let (meta, tracks, format, size) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        Ok(match kind {
            AssetKind::Ebook => {
                let meta = ebook_meta(&p);
                let size = std::fs::metadata(&p)?.len() as i64;
                (meta, Vec::new(), ext_of(&p), size)
            }
            AssetKind::Audiobook => {
                let (meta, tracks) = audiobook_meta(&p)?;
                let format = tracks.first().map(|t| ext_of(&t.path)).unwrap_or_default();
                let size = tracks.iter().filter_map(|t| std::fs::metadata(&t.path).ok()).map(|m| m.len() as i64).sum();
                (meta, tracks, format, size)
            }
        })
    })
    .await??;

    let work_id = find_or_create_work(&state.db, &meta).await?;
    if let Some(cover) = &meta.cover {
        save_cover_if_missing(state, work_id, cover).await?;
    }
    let duration: Option<f64> = (!tracks.is_empty()).then(|| tracks.iter().map(|t| t.duration).sum());
    let (asset_id,): (i64,) = sqlx::query_as(
        "INSERT INTO assets (work_id, kind, format, path, size, duration_secs, origin) VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(work_id)
    .bind(kind.as_str())
    .bind(&format)
    .bind(&path_str)
    .bind(size)
    .bind(duration)
    .bind(origin)
    .fetch_one(&state.db)
    .await?;
    // Formats only Calibre reads get an EPUB made straight away.
    if kind == AssetKind::Ebook && crate::book::CONVERTIBLE.contains(&format.as_str()) {
        crate::jobs::enqueue_pair(state, duet_common::JobKind::Convert, work_id, Some(asset_id), None).await?;
    }
    for (i, t) in tracks.iter().enumerate() {
        sqlx::query("INSERT INTO tracks (asset_id, idx, path, title, duration_secs) VALUES (?, ?, ?, ?, ?)")
            .bind(asset_id)
            .bind(i as i64)
            .bind(t.path.to_string_lossy().to_string())
            .bind(&t.title)
            .bind(t.duration)
            .execute(&state.db)
            .await?;
    }
    Ok(Some(work_id))
}

pub fn match_key(title: &str, author: Option<&str>) -> String {
    let surname = author.map(text::normalize).and_then(|a| a.rsplit(' ').next().map(str::to_string)).unwrap_or_default();
    format!("{}|{}", text::title_key(title), surname)
}

/// Calibre and LibGen title their files "Title - Author"; the library
/// convention is bare title with the author in its own field.
pub fn clean_title(title: &str, author: Option<&str>) -> String {
    let t = title.trim().to_string();
    if let Some(a) = author.map(str::trim).filter(|a| !a.is_empty()) {
        for sep in [" - ", " by "] {
            if let Some((head, tail)) = t.rsplit_once(sep) {
                if tail.eq_ignore_ascii_case(a) && !head.trim().is_empty() {
                    return head.trim().to_string();
                }
            }
        }
    }
    t
}

async fn find_or_create_work(db: &SqlitePool, meta: &Meta) -> anyhow::Result<i64> {
    let title = clean_title(&meta.title, meta.author.as_deref());
    let key = match_key(&title, meta.author.as_deref());
    let title_part = key.split('|').next().unwrap_or("").to_string();
    // Exact match, or a same-title work where one side has no author.
    let existing: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM works WHERE match_key = ?1
            OR (?2 = '' AND match_key LIKE ?3 || '|%') OR match_key = ?3 || '|'
         ORDER BY match_key = ?1 DESC LIMIT 1",
    )
    .bind(&key)
    .bind(key.split('|').nth(1).unwrap_or(""))
    .bind(&title_part)
    .fetch_optional(db)
    .await?;
    if let Some((id,)) = existing {
        // Fill in anything the first edition lacked.
        sqlx::query(
            "UPDATE works SET author = COALESCE(author, ?), series = COALESCE(series, ?),
                series_index = COALESCE(series_index, ?), description = COALESCE(description, ?),
                year = COALESCE(year, ?) WHERE id = ?",
        )
        .bind(&meta.author)
        .bind(&meta.series)
        .bind(meta.series_index)
        .bind(&meta.description)
        .bind(meta.year)
        .bind(id)
        .execute(db)
        .await?;
        return Ok(id);
    }
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO works (title, author, series, series_index, description, year, match_key) VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&title)
    .bind(&meta.author)
    .bind(&meta.series)
    .bind(meta.series_index)
    .bind(&meta.description)
    .bind(meta.year)
    .bind(&key)
    .fetch_one(db)
    .await?;
    Ok(id)
}

async fn save_cover_if_missing(state: &State, work_id: i64, bytes: &[u8]) -> anyhow::Result<()> {
    let (cover,): (Option<String>,) =
        sqlx::query_as("SELECT cover_path FROM works WHERE id = ?").bind(work_id).fetch_one(&state.db).await?;
    if cover.is_some() {
        return Ok(());
    }
    let dir = state.data_dir.join("covers");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{work_id}.img"));
    tokio::fs::write(&path, bytes).await?;
    sqlx::query("UPDATE works SET cover_path = ? WHERE id = ?")
        .bind(path.to_string_lossy().to_string())
        .bind(work_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// "Author - Title", "Title - Author" is ambiguous, so only the first form is recognised.
fn meta_from_filename(path: &Path) -> Meta {
    let stem = path.file_stem().map(|s| s.to_string_lossy().replace('_', " ")).unwrap_or_default();
    match stem.split_once(" - ") {
        Some((author, title)) => Meta { title: title.trim().into(), author: Some(author.trim().into()), ..Default::default() },
        None => Meta { title: stem.trim().into(), ..Default::default() },
    }
}

/// First plausible publication year in free text ("2011-03-15" or "2011").
fn parse_year(s: &str) -> Option<i32> {
    let digits: Vec<char> = s.chars().collect();
    for i in 0..digits.len().saturating_sub(3) {
        if digits[i..i + 4].iter().all(|c| c.is_ascii_digit())
            && (i == 0 || !digits[i - 1].is_ascii_digit())
            && digits.get(i + 4).is_none_or(|c| !c.is_ascii_digit())
        {
            let year: i32 = digits[i..i + 4].iter().collect::<String>().parse().ok()?;
            if (1000..=2100).contains(&year) {
                return Some(year);
            }
        }
    }
    None
}

pub fn ebook_meta(path: &Path) -> Meta {
    if ext_of(path) == "epub"
        && let Ok(mut doc) = epub::doc::EpubDoc::new(path) {
            let get = |k: &str| doc.mdata(k).map(|m| m.value.trim().to_string()).filter(|v| !v.is_empty());
            // EPUB 3 collections are only series when refined as one (they can also be reading lists).
            let series = doc
                .mdata("belongs-to-collection")
                .filter(|m| m.refinement("collection-type").is_some_and(|r| r.value == "series"))
                .map(|m| m.value.trim().to_string())
                .or_else(|| get("calibre:series"));
            let series_index = get("group-position").or_else(|| get("calibre:series_index")).and_then(|v| v.parse().ok());
            let mut meta = Meta {
                title: doc.get_title().unwrap_or_default(),
                author: get("creator"),
                description: get("description").map(|d| strip_tags(&d)),
                series,
                series_index,
                year: get("date").as_deref().and_then(parse_year),
                cover: None,
            };
            meta.cover = doc.get_cover().map(|(bytes, _)| bytes);
            if !meta.title.is_empty() {
                return meta;
            }
        }
    if images::is_comic(path) {
        return comic_meta(path);
    }
    if ext_of(path) == "pdf" {
        return pdf_meta(path);
    }
    meta_from_filename(path)
}

/// The text of an XML element, e.g. `<Series>…</Series>` in ComicInfo.xml.
fn xml_field(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    let v = xml[start..end].trim().replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">");
    (!v.is_empty()).then_some(v)
}

/// Comics: ComicInfo.xml when present (series, issue, writer), and the first page as the cover.
fn comic_meta(path: &Path) -> Meta {
    let mut meta = meta_from_filename(path);
    if let Some(xml) = images::comic_info(path) {
        let series = xml_field(&xml, "Series");
        let number = xml_field(&xml, "Number");
        meta.title = match (xml_field(&xml, "Title"), &series, &number) {
            (Some(t), _, _) => t,
            (None, Some(s), Some(n)) => format!("{s} #{n}"),
            (None, Some(s), None) => s.clone(),
            _ => meta.title,
        };
        meta.author = xml_field(&xml, "Writer").or(meta.author);
        meta.description = xml_field(&xml, "Summary");
        meta.series_index = number.and_then(|n| n.parse().ok());
        meta.series = series;
        meta.year = xml_field(&xml, "Year").and_then(|y| y.parse().ok());
    }
    meta.cover = images::comic_page(path, 0).ok().map(|(bytes, _)| bytes);
    meta
}

/// PDFs: the document's title and author when it has them, and page one as the cover.
fn pdf_meta(path: &Path) -> Meta {
    let mut meta = meta_from_filename(path);
    if let Ok(out) = std::process::Command::new("pdfinfo").arg(path).output() {
        let info = String::from_utf8_lossy(&out.stdout);
        let field = |k: &str| {
            info.lines()
                .find_map(|l| l.strip_prefix(k))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("untitled") && !v.ends_with(".doc"))
        };
        // Tools often stamp the file name or their own URL in here; those aren't a title or author.
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if let Some(t) = field("Title:").filter(|t| *t != stem && !t.contains("://") && !t.contains('\\') && !t.contains('/')) {
            meta.title = t;
        }
        meta.author = field("Author:").filter(|a| !a.contains("://") && !a.contains('@')).or(meta.author);
    }
    let tmp = std::env::temp_dir().join(format!("duet-cover-{}", std::process::id()));
    meta.cover = images::pdf_page(path, 0, 600, &tmp).ok();
    let _ = std::fs::remove_dir_all(&tmp);
    meta
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Natural sort so "Track 2" comes before "Track 10".
fn natural_key(p: &Path) -> Vec<(String, u64)> {
    let s = p.to_string_lossy().to_lowercase();
    let mut out = Vec::new();
    let mut text = String::new();
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((std::mem::take(&mut text), num.parse().unwrap_or(0)));
                num.clear();
            }
            text.push(c);
        }
    }
    out.push((text, num.parse().unwrap_or(0)));
    out
}

pub fn audiobook_meta(path: &Path) -> anyhow::Result<(Meta, Vec<FoundTrack>)> {
    let mut files: Vec<PathBuf> = if path.is_dir() {
        std::fs::read_dir(path)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && AUDIO_EXTS.contains(&ext_of(p).as_str()))
            .collect()
    } else {
        vec![path.to_path_buf()]
    };
    files.sort_by_key(|p| natural_key(p));
    let mut meta = Meta::default();
    let mut tracks = Vec::new();
    for f in files {
        let tagged = lofty::read_from_path(&f).with_context(|| format!("reading tags of {}", f.display()))?;
        let duration = tagged.properties().duration().as_secs_f64();
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        let title = tag.and_then(|t| t.title().map(|s| s.to_string())).unwrap_or_else(|| {
            f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
        });
        if meta.title.is_empty()
            && let Some(t) = tag {
                meta.title = t.album().map(|s| s.to_string()).unwrap_or_default();
                meta.author = t.artist().map(|s| s.to_string());
                meta.cover = t.pictures().first().map(|p| p.data().to_vec());
            }
        tracks.push(FoundTrack { path: f, title, duration });
    }
    if meta.title.is_empty() {
        let fallback = meta_from_filename(path);
        meta.title = fallback.title;
        meta.author = meta.author.or(fallback.author);
    }
    if meta.cover.is_none() && path.is_dir() {
        for name in ["cover.jpg", "cover.png", "folder.jpg", "folder.png"] {
            if let Ok(bytes) = std::fs::read(path.join(name)) {
                meta.cover = Some(bytes);
                break;
            }
        }
    }
    Ok((meta, tracks))
}

/// Moves an asset to another work (manual linking), deleting the old work if it's now empty.
pub async fn link_asset(db: &SqlitePool, asset_id: i64, work_id: i64) -> anyhow::Result<()> {
    let (old,): (i64,) = sqlx::query_as("SELECT work_id FROM assets WHERE id = ?").bind(asset_id).fetch_one(db).await?;
    sqlx::query("UPDATE assets SET work_id = ? WHERE id = ?").bind(work_id).bind(asset_id).execute(db).await?;
    sqlx::query("DELETE FROM works WHERE id = ? AND NOT EXISTS (SELECT 1 FROM assets WHERE work_id = ?)")
        .bind(old)
        .bind(old)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_link_editions() {
        assert_eq!(match_key("The Hobbit", Some("J.R.R. Tolkien")), match_key("Hobbit (Unabridged)", Some("J. R. R. Tolkien")));
    }

    #[test]
    fn strips_author_suffix() {
        assert_eq!(clean_title("Doctor Sleep - Stephen King", Some("Stephen King")), "Doctor Sleep");
        assert_eq!(clean_title("Dune", Some("Frank Herbert")), "Dune");
        assert_eq!(clean_title("Emma", None), "Emma");
    }

    #[test]
    fn natural_order() {
        let mut v = vec![PathBuf::from("a/Track 10.mp3"), PathBuf::from("a/Track 2.mp3")];
        v.sort_by_key(|p| natural_key(p));
        assert_eq!(v[0], PathBuf::from("a/Track 2.mp3"));
    }
}

/// Folds one work into another: its editions, place, sync maps and cast move over.
pub async fn merge_works(db: &SqlitePool, from: i64, into: i64) -> anyhow::Result<()> {
    if from == into {
        return Ok(());
    }
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE assets SET work_id = ? WHERE work_id = ?").bind(into).bind(from).execute(&mut *tx).await?;
    sqlx::query("UPDATE sync_maps SET work_id = ? WHERE work_id = ?").bind(into).bind(from).execute(&mut *tx).await?;
    sqlx::query("UPDATE OR IGNORE cast_members SET work_id = ? WHERE work_id = ?").bind(into).bind(from).execute(&mut *tx).await?;
    sqlx::query("UPDATE OR IGNORE positions SET work_id = ? WHERE work_id = ?").bind(into).bind(from).execute(&mut *tx).await?;
    sqlx::query("UPDATE jobs SET work_id = ? WHERE work_id = ?").bind(into).bind(from).execute(&mut *tx).await?;
    sqlx::query("UPDATE works SET cover_path = COALESCE(cover_path, (SELECT cover_path FROM works WHERE id = ?)) WHERE id = ?")
        .bind(from)
        .bind(into)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM works WHERE id = ?").bind(from).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// After a download is imported: put it in the book it was fetched for. If another work already
/// has that title and author it joins that one; otherwise its work takes on that title, so the
/// other half of an ebook + audiobook pair finds it. Download titles often read
/// "Title - Author"; the suffix is stripped so editions link up.
pub async fn adopt(db: &SqlitePool, work_id: i64, title: &str, author: Option<&str>) -> anyhow::Result<i64> {
    let title = clean_title(title, author);
    let key = match_key(&title, author);
    let other: Option<(i64,)> = sqlx::query_as("SELECT id FROM works WHERE match_key = ? AND id != ? ORDER BY id LIMIT 1")
        .bind(&key)
        .bind(work_id)
        .fetch_optional(db)
        .await?;
    if let Some((into,)) = other {
        merge_works(db, work_id, into).await?;
        return Ok(into);
    }
    sqlx::query("UPDATE works SET title = ?, author = COALESCE(?, author), match_key = ? WHERE id = ?")
        .bind(&title)
        .bind(author)
        .bind(&key)
        .bind(work_id)
        .execute(db)
        .await?;
    Ok(work_id)
}
