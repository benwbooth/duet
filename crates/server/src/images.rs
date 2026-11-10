//! Books that are pictures of pages: comic archives (CBZ, CBR, CB7, CBT) and PDFs. Each page is
//! served as an image so the reader can lay them out and turn them like any other page.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, bail};

use crate::library::ext_of;

pub const COMIC_EXTS: &[&str] = &["cbz", "cbr", "cb7", "cbt"];
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "avif", "bmp"];

pub fn is_comic(path: &Path) -> bool {
    COMIC_EXTS.contains(&ext_of(path).as_str())
}

/// Page lists, by archive path; listing a big CBR is slow enough to be worth keeping.
fn listings() -> &'static Mutex<HashMap<PathBuf, Vec<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<String>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// "page 2.jpg" before "page 10.jpg".
fn natural(s: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let (mut text, mut num) = (String::new(), String::new());
    for c in s.to_lowercase().chars() {
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

fn run(cmd: &str, args: &[&std::ffi::OsStr]) -> anyhow::Result<Vec<u8>> {
    let out = std::process::Command::new(cmd).args(args).output().with_context(|| format!("running {cmd} (is it installed?)"))?;
    if !out.status.success() {
        bail!("{cmd} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// Fixed-layout comic EPUBs (how comics and manga are often sold): every page a single picture
/// with next to no text. The pictures in reading order, or None for an ordinary EPUB.
pub fn epub_comic_pages(path: &Path) -> Option<Vec<String>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<Vec<String>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap().get(path) {
        return v.clone();
    }
    let found = scan_epub_comic(path);
    cache.lock().unwrap().insert(path.to_path_buf(), found.clone());
    found
}

fn scan_epub_comic(path: &Path) -> Option<Vec<String>> {
    let mut doc = epub::doc::EpubDoc::new(path).ok()?;
    let n = doc.spine.len();
    if n < 4 {
        return None;
    }
    let mut pages = Vec::new();
    let mut texty = 0;
    for i in 0..n {
        if !doc.set_current_chapter(i) {
            continue;
        }
        let base = doc.get_current_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let Some((html, _)) = doc.get_current_str() else { continue };
        let (pictures, words) = picture_page(&html);
        if words > 200 {
            texty += 1;
            // A novel: no need to look any further.
            if texty > 2 || i < 6 {
                return None;
            }
        }
        if let Some(src) = pictures.first().filter(|_| words < 60) {
            let src = urlencoding::decode(src).map(|s| s.into_owned()).unwrap_or_else(|_| src.clone());
            pages.push(crate::book::resolve(&base, &src));
        }
    }
    // Nearly every page a picture.
    (pages.len() >= 4 && pages.len() * 10 >= n * 8).then_some(pages)
}

/// The pictures a page of HTML shows (`<img src>`, SVG `<image href>`) and how many letters of
/// text it has besides.
fn picture_page(html: &str) -> (Vec<String>, usize) {
    let body = html.find("<body").map(|i| &html[i..]).unwrap_or(html);
    let mut pictures = Vec::new();
    let mut letters = 0;
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        letters += rest[..open].chars().filter(|c| c.is_alphanumeric()).count();
        let Some(close) = rest[open..].find('>') else { break };
        let tag = &rest[open + 1..open + close];
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("img") || lower.starts_with("image") || lower.starts_with("svg:image") {
            for attr in ["xlink:href=", "src=", "href="] {
                if let Some(i) = lower.find(attr) {
                    let v = &tag[i + attr.len()..];
                    let q = v.chars().next().unwrap_or('"');
                    if q == '"' || q == '\'' {
                        if let Some(end) = v[1..].find(q) {
                            pictures.push(v[1..1 + end].to_string());
                            break;
                        }
                    }
                }
            }
        }
        rest = &rest[open + close + 1..];
    }
    letters += rest.chars().filter(|c| c.is_alphanumeric()).count();
    (pictures, letters)
}

fn is_zip(path: &Path) -> bool {
    matches!(ext_of(path).as_str(), "cbz" | "epub")
}

/// The image files of a comic archive, in reading order.
pub fn comic_pages(path: &Path) -> anyhow::Result<Vec<String>> {
    if let Some(list) = listings().lock().unwrap().get(path) {
        return Ok(list.clone());
    }
    if ext_of(path) == "epub" {
        return epub_comic_pages(path).context("not a comic EPUB");
    }
    let names: Vec<String> = if ext_of(path) == "cbz" {
        let zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        zip.file_names().map(str::to_string).collect()
    } else {
        // libarchive's bsdtar reads RAR, 7z and tar alike.
        String::from_utf8_lossy(&run("bsdtar", &["-tf".as_ref(), path.as_os_str()])?).lines().map(str::to_string).collect()
    };
    let mut pages: Vec<String> = names
        .into_iter()
        .filter(|n| !n.ends_with('/') && !n.split('/').any(|p| p.starts_with('.') || p == "__MACOSX"))
        .filter(|n| IMAGE_EXTS.contains(&ext_of(Path::new(n)).as_str()))
        .collect();
    pages.sort_by_key(|n| natural(n));
    listings().lock().unwrap().insert(path.to_path_buf(), pages.clone());
    Ok(pages)
}

fn archive_file(path: &Path, name: &str) -> anyhow::Result<Vec<u8>> {
    if is_zip(path) {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let mut f = zip.by_name(name)?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut buf)?;
        Ok(buf)
    } else {
        run("bsdtar", &["-xOf".as_ref(), path.as_os_str(), name.as_ref()])
    }
}

/// One comic page: (image bytes, MIME type).
pub fn comic_page(path: &Path, n: usize) -> anyhow::Result<(Vec<u8>, String)> {
    let pages = comic_pages(path)?;
    let name = pages.get(n).context("no such page")?;
    let mime = mime_guess::from_path(name).first_or_octet_stream().to_string();
    Ok((archive_file(path, name)?, mime))
}

/// ComicInfo.xml, the de-facto metadata file inside comic archives.
pub fn comic_info(path: &Path) -> Option<String> {
    let names = if is_zip(path) {
        zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?.file_names().map(str::to_string).collect::<Vec<_>>()
    } else {
        String::from_utf8_lossy(&run("bsdtar", &["-tf".as_ref(), path.as_os_str()]).ok()?).lines().map(str::to_string).collect()
    };
    let name = names.into_iter().find(|n| n.to_lowercase().ends_with("comicinfo.xml"))?;
    archive_file(path, &name).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub fn pdf_pages(path: &Path) -> anyhow::Result<usize> {
    let info = String::from_utf8_lossy(&run("pdfinfo", &[path.as_os_str()])?).into_owned();
    info.lines()
        .find_map(|l| l.strip_prefix("Pages:").and_then(|n| n.trim().parse().ok()))
        .context("pdfinfo didn't report a page count")
}

/// Renders one PDF page (0-based) to a JPEG `width` pixels wide, cached under `cache_dir`.
pub fn pdf_page(path: &Path, n: usize, width: u32, cache_dir: &Path) -> anyhow::Result<Vec<u8>> {
    let cached = cache_dir.join(format!("{n}-{width}.jpg"));
    if let Ok(bytes) = std::fs::read(&cached) {
        return Ok(bytes);
    }
    std::fs::create_dir_all(cache_dir)?;
    let page = (n + 1).to_string();
    let width_s = width.to_string();
    let prefix = cache_dir.join(format!("render-{n}-{width}"));
    run(
        "pdftoppm",
        &[
            "-f".as_ref(), page.as_ref(), "-l".as_ref(), page.as_ref(),
            "-scale-to-x".as_ref(), width_s.as_ref(), "-scale-to-y".as_ref(), "-1".as_ref(),
            "-jpeg".as_ref(), "-jpegopt".as_ref(), "quality=88".as_ref(), "-singlefile".as_ref(),
            path.as_os_str(), prefix.as_os_str(),
        ],
    )?;
    let rendered = prefix.with_extension("jpg");
    std::fs::rename(&rendered, &cached)?;
    Ok(std::fs::read(&cached)?)
}
