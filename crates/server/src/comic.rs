//! Reading comics: a vision model looks at each page (a local one on the graphics card by
//! default) and finds its speech and thought balloons, captions, signs and sound effects, in the
//! order a reader reads them, with who's speaking and the panels they're in. Pages in another
//! language are translated as they're read. Each page is read once, the first time it's needed
//! (the page being looked at, or the full cast getting to it), a few pages ahead, and kept.
//!
//! The full cast performs a comic a page at a time: page `p` is its "chapter" `p`, and the
//! page's lines are that chapter's sentences.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use anyhow::Context;
use duet_common::Asset;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::{State, book, db, images};

/// Bumped when the way pages are read changes enough to read them again.
const VERSION: u32 = 1;

/// One piece of text on a page.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Line {
    /// Where it is on the page: left, top, right, bottom, as fractions of the page's size.
    pub r#box: [f32; 4],
    /// Which panel it's in (an index into `PageText::panels`).
    pub panel: usize,
    /// "speech", "thought", "caption", "sfx" or "sign".
    pub kind: String,
    /// Who says it: their name when the page gives it, otherwise what they look like.
    pub speaker: String,
    /// The words in the reader's language (translated when the page is in another).
    pub text: String,
    /// The words as printed, when they were translated.
    #[serde(default)]
    pub original: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PageText {
    #[serde(default)]
    pub v: u32,
    pub lines: Vec<Line>,
    /// Each panel's box, in reading order.
    pub panels: Vec<[f32; 4]>,
    /// The language printed on the page (ISO 639-1), when there's text.
    pub language: String,
    /// Whether `text` was translated from it.
    pub translated: bool,
}

/// How a comic is read, as the reader chose (or as its file or its pages say).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Info {
    /// Pages and panels right to left (manga).
    pub rtl: bool,
    /// Translations shown over the balloons.
    pub translate: bool,
    /// The language printed in it, once known.
    pub language: String,
    /// Chosen by the reader (rather than guessed): kept as is.
    #[serde(default)]
    pub rtl_chosen: bool,
    #[serde(default)]
    pub translate_chosen: bool,
}

fn dir(state: &State, asset: i64) -> PathBuf {
    state.data_dir.join("cache").join("comic").join(asset.to_string())
}

fn info_path(state: &State, asset: i64) -> PathBuf {
    state.data_dir.join("comic").join(format!("{asset}.json"))
}

/// Whether this book is read as pictures of pages (a comic, a comic EPUB or a PDF).
pub fn is_pages(a: &Asset, state: &State) -> bool {
    matches!(book::source(a, &state.data_dir), book::Source::Comic(_) | book::Source::Pdf(_))
}

/// How the comic is read: what the reader chose, else what ComicInfo.xml says (manga, its
/// language), else what its pages turned out to be.
pub async fn info(state: &State, asset: i64) -> Info {
    if let Some(i) = tokio::fs::read(info_path(state, asset)).await.ok().and_then(|b| serde_json::from_slice::<Info>(&b).ok()) {
        return i;
    }
    let mut i = Info::default();
    if let Ok(Some(a)) = db::asset(&state.db, asset).await {
        if let book::Source::Comic(path) = book::source(&a, &state.data_dir) {
            let xml = tokio::task::spawn_blocking(move || images::comic_info(&path)).await.ok().flatten();
            if let Some(xml) = xml {
                let tag = |name: &str| -> Option<String> {
                    let open = format!("<{name}>");
                    let start = xml.find(&open)? + open.len();
                    let end = xml[start..].find("</")? + start;
                    Some(xml[start..end].trim().to_string())
                };
                let manga = tag("Manga").unwrap_or_default().to_lowercase();
                i.language = tag("LanguageISO").unwrap_or_default().to_lowercase();
                i.rtl = manga == "yesandrighttoleft" || (manga == "yes" && i.language == "ja");
            }
        }
    }
    let target = state.settings.read().await.ai.translate_to.clone();
    i.translate = !i.language.is_empty() && !same_language(&i.language, &target);
    i
}

pub async fn set_info(state: &State, asset: i64, rtl: Option<bool>, translate: Option<bool>) -> anyhow::Result<Info> {
    let mut i = info(state, asset).await;
    if let Some(r) = rtl {
        i.rtl = r;
        i.rtl_chosen = true;
    }
    if let Some(t) = translate {
        i.translate = t;
        i.translate_chosen = true;
    }
    save_info(state, asset, &i).await?;
    Ok(i)
}

async fn save_info(state: &State, asset: i64, i: &Info) -> anyhow::Result<()> {
    let path = info_path(state, asset);
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    tokio::fs::write(path, serde_json::to_vec(i)?).await?;
    Ok(())
}

/// What a page has in it once read, without reading it.
pub async fn cached(state: &State, asset: i64, page: usize) -> Option<PageText> {
    let t: PageText = serde_json::from_slice(&tokio::fs::read(dir(state, asset).join(format!("{page}.json"))).await.ok()?).ok()?;
    (t.v == VERSION).then_some(t)
}

/// Whether a language code or name ("ja", "en", "English") is the reader's language.
fn same_language(code: &str, target: &str) -> bool {
    let (c, t) = (code.trim().to_lowercase(), target.trim().to_lowercase());
    if c.is_empty() {
        return true;
    }
    const NAMES: &[(&str, &str)] = &[
        ("en", "english"), ("ja", "japanese"), ("zh", "chinese"), ("ko", "korean"), ("fr", "french"), ("de", "german"),
        ("es", "spanish"), ("it", "italian"), ("pt", "portuguese"), ("ru", "russian"), ("nl", "dutch"), ("pl", "polish"),
        ("sv", "swedish"), ("da", "danish"), ("no", "norwegian"), ("fi", "finnish"), ("tr", "turkish"), ("ar", "arabic"),
        ("he", "hebrew"), ("hi", "hindi"), ("th", "thai"), ("vi", "vietnamese"), ("id", "indonesian"), ("uk", "ukrainian"),
        ("cs", "czech"), ("el", "greek"), ("hu", "hungarian"), ("ro", "romanian"),
    ];
    let name = |x: &str| NAMES.iter().find(|(k, n)| *k == x || *n == x).map(|(k, _)| k.to_string()).unwrap_or_else(|| x.to_string());
    name(&c) == name(&t) || t.starts_with(&c) || c.starts_with(&t)
}

/// One page as a picture the vision model can take: (bytes, MIME type). Big pages are made
/// smaller first (2048 pixels on the long side is plenty to read balloons).
async fn page_picture(state: &State, a: &Asset, page: usize) -> anyhow::Result<(Vec<u8>, String, (u32, u32))> {
    let src = book::source(a, &state.data_dir);
    let cache = state.data_dir.join("cache").join("pages").join(a.id.to_string());
    let (bytes, mime) = tokio::task::spawn_blocking(move || match src {
        book::Source::Comic(path) => images::comic_page(&path, page),
        book::Source::Pdf(path) => images::pdf_page(&path, page, 1600, &cache).map(|b| (b, "image/jpeg".to_string())),
        _ => anyhow::bail!("not a comic"),
    })
    .await??;
    let size = imagesize::blob_size(&bytes).map(|s| (s.width as u32, s.height as u32)).unwrap_or((0, 0));
    let usual = matches!(mime.as_str(), "image/jpeg" | "image/png" | "image/webp");
    if usual && bytes.len() < 3_500_000 && size.0.max(size.1) <= 2400 && size.0 > 0 {
        return Ok((bytes, mime, size));
    }
    let small = tokio::task::spawn_blocking(move || shrink(&bytes)).await??;
    let size = imagesize::blob_size(&small).map(|s| (s.width as u32, s.height as u32)).unwrap_or((0, 0));
    Ok((small, "image/jpeg".into(), size))
}

/// A JPEG of the picture at most 2048 pixels on its long side (ffmpeg reads them all).
fn shrink(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i", "pipe:0", "-vf", "scale='if(gt(iw,ih),min(2048,iw),-2)':'if(gt(iw,ih),-2,min(2048,ih))'"])
        .args(["-frames:v", "1", "-q:v", "3", "-f", "image2", "-c:v", "mjpeg", "pipe:1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("running ffmpeg")?;
    let mut stdin = child.stdin.take().unwrap();
    let input = bytes.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    anyhow::ensure!(out.status.success() && !out.stdout.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(out.stdout)
}

/// One lock per page: a page is read once, however many ask for it together.
static READING: LazyLock<std::sync::Mutex<HashMap<(i64, usize), Arc<Mutex<()>>>>> = LazyLock::new(Default::default);

/// A page's text, read by the vision model the first time it's needed; and the next few pages
/// read in the background after it, so they're ready when the reader (or the full cast) gets there.
pub async fn page(state: &State, asset: i64, page: usize) -> anyhow::Result<PageText> {
    let t = read_page(state, asset, page).await;
    ahead(state, asset, page + 1);
    t
}

async fn read_page(state: &State, asset: i64, page: usize) -> anyhow::Result<PageText> {
    if let Some(t) = cached(state, asset, page).await {
        return Ok(t);
    }
    let lock = READING.lock().unwrap().entry((asset, page)).or_default().clone();
    let _held = lock.lock().await;
    if let Some(t) = cached(state, asset, page).await {
        return Ok(t);
    }
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let info = info(state, asset).await;
    let target = state.settings.read().await.ai.translate_to.clone();
    let target = if target.trim().is_empty() { "English".to_string() } else { target };
    let (picture, mime, size) = page_picture(state, &a, page).await?;
    // The page before, for who's who and what's going on.
    let before = match page.checked_sub(1) {
        Some(p) => cached(state, asset, p).await,
        None => None,
    };
    let order = if info.rtl {
        "This is manga, read right to left: panels go from the top right across to the left, row by row, and within a panel the balloons go right to left, top to bottom. Japanese text runs in vertical columns, read right to left."
    } else {
        "Panels go left to right, row by row, top to bottom (unless the layout clearly leads another way); within a panel, balloons go top to bottom and left to right, following the conversation."
    };
    let system = format!(
        "You read comic and manga pages aloud for a full-cast audiobook. Find every piece of text a reader \
         reads on the page: speech balloons, thought balloons, caption and narration boxes, signs or notes \
         that matter to the story, and sound effects. List them in the order a reader reads them. {order} A \
         balloon's tail points at its speaker; a thought balloon trails bubbles toward the thinker. Reply with \
         JSON only: {{\"language\": \"<ISO 639-1 code of the language printed on the page>\", \"panels\": \
         [[x0, y0, x1, y1], ...], \"lines\": [{{\"panel\": <index into panels>, \"box\": [x0, y0, x1, y1], \
         \"kind\": \"speech|thought|caption|sfx|sign\", \"speaker\": \"<who says it: their name if the page or \
         the earlier dialogue gives it, otherwise a short description of the character the tail points to, e.g. \
         'old man with a beard'; empty for captions, signs and sound effects>\", \"text\": \"<the words exactly \
         as printed, as one line: rejoin words hyphenated across lines, and write them in normal sentence case \
         rather than ALL CAPS (keep shouting as an exclamation mark)>\", \"translation\": \"<the words in \
         {target}, natural and in character, as a good letterer would write them; empty if the text is already \
         in {target}>\"}}]}}. Coordinates are whole numbers from 0 to 1000 across the page's width (x) and down \
         its height (y). Panels in reading order. Leave out page numbers, credits, logos and barcodes. A page \
         with no text has empty lists. A page of prose rather than a comic gives each paragraph as a caption."
    );
    let mut prompt = String::new();
    if let Some(b) = before.filter(|b| !b.lines.is_empty()) {
        prompt.push_str("The previous page, for context:\n");
        for l in &b.lines {
            let who = if l.speaker.is_empty() { l.kind.clone() } else { l.speaker.clone() };
            prompt.push_str(&format!("- {who}: {}\n", l.text));
        }
        prompt.push('\n');
    }
    prompt.push_str(&format!("Read page {}. Reply with the JSON only.", page + 1));
    let title = crate::activity::title(state, asset).await;
    let key = format!("comic-read:{asset}:{page}");
    crate::activity::set(&key, "Comic", &title, format!("Reading the balloons on page {}", page + 1), 0, 0);
    let reply = crate::llm::vision(state, &system, &prompt, &picture, &mime).await;
    crate::activity::clear(&key);
    let text = parse(&reply?, size, &target).context("the vision model's answer made no sense")?;
    tokio::fs::create_dir_all(dir(state, asset)).await?;
    tokio::fs::write(dir(state, asset).join(format!("{page}.json")), serde_json::to_vec(&text)?).await?;
    // The first page with text says what language the comic is in (and Japanese is manga,
    // read right to left), unless the reader has said otherwise.
    if !text.language.is_empty() && (info.language.is_empty() || (!info.translate_chosen && text.translated && !info.translate)) {
        let mut i = info.clone();
        i.language = text.language.clone();
        if !i.translate_chosen {
            i.translate = text.translated || !same_language(&i.language, &target);
        }
        if !i.rtl_chosen && i.language == "ja" {
            i.rtl = true;
        }
        save_info(state, asset, &i).await.ok();
    }
    Ok(text)
}

/// The vision model's answer as a page's text, boxes made fractions of the page.
fn parse(reply: &str, size: (u32, u32), target: &str) -> anyhow::Result<PageText> {
    let (start, end) = (reply.find('{').context("no JSON")?, reply.rfind('}').context("no JSON")?);
    let v: serde_json::Value = serde_json::from_str(&reply[start..=end])?;
    // Whole numbers to 1000, as asked; a model that gave pixels (or fractions) instead is read
    // that way.
    let numbers = |b: &serde_json::Value| -> Vec<f32> { b.as_array().into_iter().flatten().filter_map(|x| x.as_f64().map(|x| x as f32)).collect() };
    let mut all: Vec<f32> = v["panels"].as_array().into_iter().flatten().flat_map(numbers).collect();
    for l in v["lines"].as_array().into_iter().flatten() {
        all.extend(numbers(&l["box"]));
        all.extend(numbers(&l["bbox"]));
    }
    let pixels = all.iter().any(|&x| x > 1000.5) && size.0 > 0;
    let fractions = !all.is_empty() && all.iter().all(|&x| x <= 1.0);
    let (sx, sy) = if pixels {
        (size.0 as f32, size.1 as f32)
    } else if fractions {
        (1.0, 1.0)
    } else {
        (1000.0, 1000.0)
    };
    let quad = |b: &serde_json::Value| -> Option<[f32; 4]> {
        let n = numbers(b);
        if n.len() != 4 {
            return None;
        }
        let f = |x: f32, s: f32| (x / s).clamp(0.0, 1.0);
        let (x0, y0, x1, y1) = (f(n[0], sx), f(n[1], sy), f(n[2], sx), f(n[3], sy));
        Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
    };
    let panels: Vec<[f32; 4]> = v["panels"].as_array().into_iter().flatten().filter_map(quad).collect();
    let language = v["language"].as_str().unwrap_or("").trim().to_lowercase();
    let mut translated = false;
    let mut lines = Vec::new();
    for l in v["lines"].as_array().into_iter().flatten() {
        let printed = l["text"].as_str().unwrap_or("").trim().to_string();
        if !printed.chars().any(char::is_alphanumeric) {
            continue;
        }
        let translation = l["translation"].as_str().unwrap_or("").trim().to_string();
        let foreign = !same_language(&language, target) || printed.chars().any(|c| ('\u{3040}'..='\u{30ff}').contains(&c) || ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{ac00}'..='\u{d7af}').contains(&c));
        let (text, original) = if foreign && translation.chars().any(char::is_alphanumeric) && translation != printed {
            translated = true;
            (translation, printed)
        } else {
            (printed, String::new())
        };
        let kind = match l["kind"].as_str().unwrap_or("speech").to_lowercase().as_str() {
            k if k.starts_with("thought") => "thought",
            k if k.starts_with("caption") || k.starts_with("narrat") => "caption",
            k if k.starts_with("sfx") || k.starts_with("sound") => "sfx",
            k if k.starts_with("sign") || k.starts_with("note") || k.starts_with("text") => "sign",
            _ => "speech",
        }
        .to_string();
        let r#box = quad(&l["box"]).or_else(|| quad(&l["bbox"])).unwrap_or([0.0, 0.0, 0.0, 0.0]);
        let panel = l["panel"].as_u64().map(|p| p as usize).unwrap_or_else(|| {
            // (Left out: the panel its box is in.)
            let (cx, cy) = ((r#box[0] + r#box[2]) / 2.0, (r#box[1] + r#box[3]) / 2.0);
            panels.iter().position(|p| cx >= p[0] && cx <= p[2] && cy >= p[1] && cy <= p[3]).unwrap_or(0)
        });
        let speaker = if matches!(kind.as_str(), "speech" | "thought") { l["speaker"].as_str().unwrap_or("").trim().to_string() } else { String::new() };
        lines.push(Line { r#box, panel: panel.min(panels.len().saturating_sub(1)), kind, speaker, text, original });
    }
    Ok(PageText { v: VERSION, lines, panels, language, translated })
}

/// Comics being read ahead in the background.
static AHEAD: LazyLock<std::sync::Mutex<HashSet<i64>>> = LazyLock::new(Default::default);

/// Reads the next few pages from `from` in the background, one at a time.
pub fn ahead(state: &State, asset: i64, from: usize) {
    if !AHEAD.lock().unwrap().insert(asset) {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let pages = match db::asset(&state.db, asset).await {
            Ok(Some(a)) => {
                let src = book::source(&a, &state.data_dir);
                tokio::task::spawn_blocking(move || book::layout(&src).map(|l| l.pages)).await.ok().and_then(Result::ok).unwrap_or(0)
            }
            _ => 0,
        };
        for p in from..(from + 3).min(pages) {
            if cached(&state, asset, p).await.is_some() {
                continue;
            }
            if let Err(e) = read_page(&state, asset, p).await {
                tracing::warn!("reading comic page {p}: {e:#}");
                break;
            }
        }
        AHEAD.lock().unwrap().remove(&asset);
    });
}

/// A page's lines as the full cast performs them: balloons quoted (so they go to their
/// speaker), captions and sound effects as narration. With a note on each for the director:
/// what kind of text it is, and who the artwork shows saying it.
pub async fn script(state: &State, asset: i64, page: usize) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let t = self::page(state, asset, page).await?;
    let mut sentences = Vec::new();
    let mut notes = Vec::new();
    for l in &t.lines {
        // (Quote marks inside a balloon would split it between voices.)
        let words = l.text.replace(['“', '”', '"'], "'").replace('\n', " ");
        let (sentence, note) = match l.kind.as_str() {
            "speech" | "thought" => {
                let who = if l.speaker.is_empty() { "not clear from the art".to_string() } else { l.speaker.clone() };
                let what = if l.kind == "thought" { "thought balloon (inner voice)" } else { "speech balloon" };
                (format!("“{words}”"), format!("{what}; speaker in the art: {who}"))
            }
            "caption" => (words, "caption box (narrator)".to_string()),
            "sfx" => (words, "sound effect (narrator performs it)".to_string()),
            _ => (words, "sign or lettering in the art (narrator)".to_string()),
        };
        sentences.push(sentence);
        notes.push(note);
    }
    Ok((sentences, notes))
}

/// How many pages a comic has.
pub async fn pages(state: &State, asset: i64) -> usize {
    match db::asset(&state.db, asset).await {
        Ok(Some(a)) => {
            let src = book::source(&a, &state.data_dir);
            tokio::task::spawn_blocking(move || book::layout(&src).map(|l| l.pages)).await.ok().and_then(Result::ok).unwrap_or(0)
        }
        _ => 0,
    }
}

/// The page's text for the reader: balloons and panels, and how the comic is read.
pub async fn for_reader(state: &State, asset: i64, page: usize) -> anyhow::Result<serde_json::Value> {
    let t = self::page(state, asset, page).await?;
    let info = info(state, asset).await;
    Ok(json!({ "page": page, "lines": t.lines, "panels": t.panels, "language": t.language, "translated": t.translated, "rtl": info.rtl, "translate": info.translate }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_models_answer() {
        let reply = r#"Here: {"language": "ja", "panels": [[500, 0, 1000, 500], [0, 0, 500, 500]],
            "lines": [{"panel": 0, "box": [600, 50, 700, 200], "kind": "speech", "speaker": "boy in a cap", "text": "おはよう!", "translation": "Good morning!"},
                      {"panel": 1, "box": [100, 50, 200, 100], "kind": "SFX", "text": "ドン", "translation": "Bam"},
                      {"panel": 1, "box": [0, 0, 0, 0], "kind": "speech", "text": "...", "translation": ""}]}"#;
        let t = parse(reply, (1000, 1500), "English").unwrap();
        assert_eq!(t.language, "ja");
        assert!(t.translated);
        assert_eq!(t.lines.len(), 2);
        assert_eq!(t.lines[0].text, "Good morning!");
        assert_eq!(t.lines[0].original, "おはよう!");
        assert_eq!(t.lines[0].speaker, "boy in a cap");
        assert!((t.lines[0].r#box[0] - 0.6).abs() < 1e-4);
        assert_eq!(t.lines[1].kind, "sfx");
        assert_eq!(t.lines[1].speaker, "");
        assert_eq!(t.panels.len(), 2);
    }

    #[test]
    fn english_pages_stay_as_printed() {
        let reply = r#"{"language": "en", "panels": [], "lines": [{"box": [10, 10, 20, 20], "kind": "caption", "text": "Meanwhile...", "translation": "Meanwhile..."}]}"#;
        let t = parse(reply, (0, 0), "English").unwrap();
        assert!(!t.translated);
        assert_eq!(t.lines[0].text, "Meanwhile...");
        assert_eq!(t.lines[0].original, "");
    }

    #[test]
    fn pixel_boxes_become_fractions() {
        let reply = r#"{"language": "en", "panels": [[0, 0, 1200, 900]], "lines": [{"box": [600, 450, 1200, 1800], "kind": "speech", "text": "Hi"}]}"#;
        let t = parse(reply, (1200, 1800), "English").unwrap();
        assert!((t.lines[0].r#box[1] - 0.25).abs() < 1e-4);
        assert!((t.lines[0].r#box[3] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn languages() {
        assert!(same_language("en", "English"));
        assert!(!same_language("ja", "English"));
        assert!(same_language("fr", "French"));
    }
}
