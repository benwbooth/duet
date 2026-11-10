//! Reading: splits ebooks into chapters, renders them as HTML with every sentence wrapped in a
//! numbered span, and exports the same sentences for the aligner and the full-cast pipeline.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use duet_common::{Asset, BookLayout, Chapter, ReadMode, TocEntry, text};
use kuchikiki::{NodeRef, traits::TendrilSink};

use crate::{images, library::ext_of};

/// Where an ebook asset's readable content comes from.
pub enum Source {
    Epub(PathBuf),
    Text(PathBuf),
    Html(PathBuf),
    /// Pages rendered to images: PDF through poppler, comics straight from the archive.
    Pdf(PathBuf),
    Comic(PathBuf),
    /// MOBI, AZW3, FB2, DOCX... until converted to EPUB.
    NeedsConversion,
    /// Text made from an audiobook, a chapter per track as each is transcribed (see audiotext.rs):
    /// a folder of layout.json and <n>.html.
    Transcript(PathBuf),
}

/// Formats Calibre's ebook-convert turns into EPUB for us.
pub const CONVERTIBLE: &[&str] = &[
    "mobi", "azw", "azw3", "azw4", "prc", "pdb", "fb2", "fbz", "docx", "rtf", "odt", "lit", "lrf", "chm", "djvu",
    "htmlz", "txtz", "pml", "rb", "snb", "tcr",
];

pub fn converted_path(data_dir: &Path, asset_id: i64) -> PathBuf {
    data_dir.join("converted").join(format!("{asset_id}.epub"))
}

pub fn source(asset: &Asset, data_dir: &Path) -> Source {
    let path = PathBuf::from(&asset.path);
    if asset.format == "transcript" {
        return Source::Transcript(path);
    }
    match ext_of(&path).as_str() {
        // (A comic sold as an EPUB, a picture a page, reads as a comic.)
        "epub" if images::epub_comic_pages(&path).is_some() => Source::Comic(path),
        "epub" => Source::Epub(path),
        "txt" | "md" => Source::Text(path),
        "html" | "htm" | "xhtml" => Source::Html(path),
        "pdf" => Source::Pdf(path),
        "cbz" | "cbr" | "cb7" | "cbt" => Source::Comic(path),
        _ => {
            let converted = converted_path(data_dir, asset.id);
            if converted.exists() { Source::Epub(converted) } else { Source::NeedsConversion }
        }
    }
}

const BLOCKS: &[&str] = &[
    "p", "div", "h1", "h2", "h3", "h4", "h5", "h6", "li", "blockquote", "td", "th", "pre", "section", "article",
    "header", "footer", "figcaption", "dd", "dt", "body", "aside", "caption",
];

pub fn layout(src: &Source) -> anyhow::Result<BookLayout> {
    match src {
        Source::Epub(path) => {
            let doc = epub::doc::EpubDoc::new(path).context("opening EPUB")?;
            let mut toc = Vec::new();
            fn walk(doc: &epub::doc::EpubDoc<std::io::BufReader<std::fs::File>>, points: &[epub::doc::NavPoint], depth: usize, toc: &mut Vec<TocEntry>) {
                for p in points {
                    let content = PathBuf::from(p.content.to_string_lossy().split('#').next().unwrap_or(""));
                    if let Some(chapter) = doc.resource_uri_to_chapter(&content) {
                        toc.push(TocEntry { title: p.label.trim().to_string(), chapter, depth });
                    }
                    walk(doc, &p.children, depth + 1, toc);
                }
            }
            walk(&doc, &doc.toc, 0, &mut toc);
            Ok(BookLayout { mode: ReadMode::Chapters, chapters: doc.spine.len(), toc, pages: 0 })
        }
        Source::Text(path) => {
            let chapters = text_chapters(&std::fs::read_to_string(path)?);
            let toc = chapters.iter().enumerate().map(|(i, c)| TocEntry { title: c.0.clone(), chapter: i, depth: 0 }).collect();
            Ok(BookLayout { mode: ReadMode::Chapters, chapters: chapters.len(), toc, pages: 0 })
        }
        Source::Html(_) => Ok(BookLayout { mode: ReadMode::Chapters, chapters: 1, toc: Vec::new(), pages: 0 }),
        Source::Transcript(dir) => Ok(serde_json::from_slice(&std::fs::read(dir.join("layout.json")).context("reading the transcript")?)?),
        Source::Pdf(path) => Ok(BookLayout { mode: ReadMode::Images, chapters: 1, toc: Vec::new(), pages: images::pdf_pages(path)? }),
        Source::Comic(path) => {
            Ok(BookLayout { mode: ReadMode::Images, chapters: 1, toc: Vec::new(), pages: images::comic_pages(path)?.len() })
        }
        Source::NeedsConversion => Ok(BookLayout { mode: ReadMode::Unsupported, chapters: 0, toc: Vec::new(), pages: 0 }),
    }
}

/// Raw chapter HTML and the path it lives at inside the book (for resolving images).
fn chapter_source(src: &Source, index: usize) -> anyhow::Result<(String, String)> {
    match src {
        Source::Epub(path) => {
            let mut doc = epub::doc::EpubDoc::new(path)?;
            if !doc.set_current_chapter(index) {
                bail!("no chapter {index}");
            }
            let base = doc.get_current_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let (html, _) = doc.get_current_str().context("reading chapter")?;
            Ok((html, base))
        }
        Source::Text(path) => {
            let chapters = text_chapters(&std::fs::read_to_string(path)?);
            let (title, body) = chapters.get(index).context("no such chapter")?;
            let mut html = String::new();
            if !title.is_empty() {
                html.push_str(&format!("<h2>{}</h2>", escape(title)));
            }
            for para in body.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
                html.push_str(&format!("<p>{}</p>", escape(&para.replace('\n', " "))));
            }
            Ok((html, String::new()))
        }
        Source::Html(path) => Ok((std::fs::read_to_string(path)?, String::new())),
        Source::Transcript(dir) => Ok((
            std::fs::read_to_string(dir.join(format!("{index}.html"))).context("this part isn't transcribed yet")?,
            String::new(),
        )),
        Source::Pdf(_) | Source::Comic(_) | Source::NeedsConversion => bail!("this format has no chapters"),
    }
}

/// Splits plain text at "Chapter ..." lines; the whole text is one chapter if there are none.
fn text_chapters(t: &str) -> Vec<(String, String)> {
    let t = t.replace("\r\n", "\n");
    let mut out: Vec<(String, String)> = vec![(String::new(), String::new())];
    for line in t.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        let heading = trimmed.len() < 60
            && (lower.starts_with("chapter ") || lower.starts_with("book ") || lower == "prologue" || lower == "epilogue");
        if heading {
            out.push((trimmed.to_string(), String::new()));
        } else {
            let body = &mut out.last_mut().unwrap().1;
            body.push_str(line);
            body.push('\n');
        }
    }
    if out[0].1.trim().is_empty() && out.len() > 1 {
        out.remove(0);
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Resolves `href` relative to the chapter at `base`, both paths inside the book.
pub(crate) fn resolve(base: &str, href: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let base_dir = Path::new(base).parent().unwrap_or(Path::new(""));
    for c in base_dir.join(href).components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_string_lossy().to_string()),
            _ => {}
        }
    }
    parts.join("/")
}

/// The chapter's body as a DOM, cleaned up for display.
fn chapter_dom(src: &Source, asset_id: i64, index: usize) -> anyhow::Result<NodeRef> {
    let (html, base) = chapter_source(src, index)?;
    let doc = kuchikiki::parse_html().one(html).document_node;
    let body = doc.select_first("body").map(|b| b.as_node().clone()).unwrap_or(doc);
    // The reader's theme replaces the book's styles; scripts never run.
    let junk: Vec<NodeRef> =
        body.select("script, style, link, meta, title").map(|s| s.collect::<Vec<_>>()).unwrap_or_default().into_iter().map(|n| n.as_node().clone()).collect();
    for n in junk {
        n.detach();
    }
    rewrite_resource_urls(&body, asset_id, &base);
    Ok(body)
}

/// Points every image/link URL in the chapter at the book's own resources.
fn rewrite_resource_urls(body: &NodeRef, asset_id: i64, base: &str) {
    let res_url = |path: &str| {
        format!("/api/assets/{asset_id}/res/{}", path.split('/').map(|p| urlencoding::encode(p).into_owned()).collect::<Vec<_>>().join("/"))
    };
    let external = |v: &str| v.starts_with("http:") || v.starts_with("https:") || v.starts_with("mailto:") || v.starts_with("data:");
    for node in body.inclusive_descendants() {
        let Some(el) = node.as_element() else { continue };
        // Local names match regardless of namespace, so SVG <image>/<a>
        // are covered here even though CSS selectors may miss them.
        let tag: &str = el.name.local.as_ref();
        let is_link = tag == "a";
        if !matches!(tag, "img" | "image" | "a" | "source" | "video" | "audio" | "use") {
            continue;
        }
        let mut attrs = el.attributes.borrow_mut();
        // Responsive images ("a.png 1x, a-2x.png 2x"): the browser prefers these over src, so
        // every candidate has to point into the book too.
        if let Some(set) = attrs.get("srcset").map(str::to_string) {
            let rewritten = set
                .split(',')
                .map(|cand| {
                    let cand = cand.trim();
                    let (url, descriptor) = cand.split_once(char::is_whitespace).unwrap_or((cand, ""));
                    let url = if external(url) { url.to_string() } else { res_url(&resolve(&base, url)) };
                    format!("{url} {descriptor}").trim().to_string()
                })
                .collect::<Vec<_>>()
                .join(", ");
            attrs.insert("srcset", rewritten);
        }
        for name in ["src", "href", "poster"] {
            let Some(v) = attrs.get(name).map(str::to_string) else { continue };
            if external(&v) {
                continue;
            }
            if is_link {
                // Links to other chapters: the UI turns data-href into navigation.
                if let Some((file, frag)) = v.split_once('#').or(Some((v.as_str(), ""))) {
                    let target = if file.is_empty() { base.to_string() } else { resolve(base, file) };
                    attrs.insert("data-href", format!("{target}#{frag}"));
                    attrs.insert("href", "#".to_string());
                }
            } else {
                attrs.insert(name, res_url(&resolve(&base, &v)));
            }
        }
        // Namespaced attributes (SVG's xlink:href): invisible to the plain
        // string getters above, which only see the null namespace.
        let ns_keys: Vec<_> = attrs
            .map
            .iter()
            .filter(|(n, a)| a.prefix.is_some() && n.local.as_ref() == "href")
            .map(|(n, _)| n.clone())
            .collect();
        for key in ns_keys {
            let v = match attrs.map.get(&key) {
                Some(a) => a.value.clone(),
                None => continue,
            };
            if external(&v) {
                continue;
            }
            if is_link {
                if let Some((file, frag)) = v.split_once('#').or(Some((v.as_str(), ""))) {
                    let target = if file.is_empty() { base.to_string() } else { resolve(base, file) };
                    attrs.insert("data-href", format!("{target}#{frag}"));
                    if let Some(a) = attrs.map.get_mut(&key) {
                        a.value = "#".to_string();
                    }
                }
            } else if let Some(a) = attrs.map.get_mut(&key) {
                a.value = res_url(&resolve(&base, &v));
            }
        }
    }
}

fn block_of(node: &NodeRef) -> Option<NodeRef> {
    node.ancestors().find(|a| a.as_element().is_some_and(|e| BLOCKS.contains(&e.name.local.as_ref())))
}

/// Text nodes grouped by the block they belong to, in document order.
fn text_groups(body: &NodeRef) -> Vec<Vec<NodeRef>> {
    let mut groups: Vec<Vec<NodeRef>> = Vec::new();
    let mut current_block: Option<NodeRef> = None;
    for node in body.inclusive_descendants() {
        if node.as_text().is_none() {
            continue;
        }
        let block = block_of(&node);
        let same = match (&block, &current_block) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        };
        if !same || groups.is_empty() {
            groups.push(Vec::new());
            current_block = block;
        }
        groups.last_mut().unwrap().push(node);
    }
    groups
}

/// Wraps every sentence in `<span class="s" data-s="N">` and returns the sentences' text.
fn wrap_sentences(body: &NodeRef) -> Vec<String> {
    let span_name = {
        let tmp = kuchikiki::parse_html().one("<span></span>").document_node;
        tmp.select_first("span").map(|s| s.name.clone()).expect("span parses")
    };
    let mut sentences = Vec::new();
    for group in text_groups(body) {
        let texts: Vec<String> = group.iter().map(|n| n.as_text().unwrap().borrow().clone()).collect();
        let full: String = texts.concat();
        let ranges = text::sentences(&full);
        if ranges.is_empty() {
            continue;
        }
        let first = sentences.len();
        sentences.extend(ranges.iter().map(|r| full[r.clone()].split_whitespace().collect::<Vec<_>>().join(" ")));
        let mut offset = 0;
        for (node, t) in group.iter().zip(&texts) {
            let (start, end) = (offset, offset + t.len());
            offset = end;
            let mut pieces: Vec<NodeRef> = Vec::new();
            let mut pos = start;
            for (i, r) in ranges.iter().enumerate() {
                let (s, e) = (r.start.max(start), r.end.min(end));
                if s >= e {
                    continue;
                }
                if s > pos {
                    pieces.push(NodeRef::new_text(&full[pos..s]));
                }
                let span = NodeRef::new_element(span_name.clone(), None);
                {
                    let el = span.as_element().unwrap();
                    let mut attrs = el.attributes.borrow_mut();
                    attrs.insert("class", "s".into());
                    attrs.insert("data-s", (first + i).to_string());
                }
                span.append(NodeRef::new_text(&full[s..e]));
                pieces.push(span);
                pos = e;
            }
            if pos < end {
                pieces.push(NodeRef::new_text(&full[pos..end]));
            }
            for p in pieces {
                node.insert_before(p);
            }
            node.detach();
        }
    }
    sentences
}

/// Page-image books are one "chapter" of page images, one per page of the book. `rtl`: manga,
/// laid out last page first, so pages turn right to left (each keeps its own number, `data-p`).
pub fn image_pages(src: &Source, asset_id: i64, rtl: bool) -> anyhow::Result<Chapter> {
    let pages = layout(src)?.pages;
    // PDFs are rendered to a size; comics are served as they are.
    let query = if matches!(src, Source::Pdf(_)) { "?w=1400" } else { "" };
    let mut each: Vec<String> = (0..pages)
        .map(|n| {
            format!(
                r#"<div class="img-page" data-p="{n}"><img src="/api/assets/{asset_id}/pages/{n}{query}" alt="Page {}" loading="lazy" decoding="async"></div>"#,
                n + 1
            )
        })
        .collect();
    if rtl {
        each.reverse();
        // (An odd number of pages: a blank one at the back, so the first page opens on the
        // right, as manga does.)
        if pages % 2 == 1 && pages > 1 {
            each.insert(0, r#"<div class="img-page blank"></div>"#.to_string());
        }
    }
    Ok(Chapter { index: 0, html: each.concat(), sentences: 0 })
}

pub fn chapter(src: &Source, asset_id: i64, index: usize) -> anyhow::Result<Chapter> {
    if matches!(src, Source::Pdf(_) | Source::Comic(_)) {
        return image_pages(src, asset_id, false);
    }
    let body = chapter_dom(src, asset_id, index)?;
    let sentences = wrap_sentences(&body);
    let mut html = Vec::new();
    for child in body.children() {
        child.serialize(&mut html)?;
    }
    Ok(Chapter { index, html: String::from_utf8_lossy(&html).into_owned(), sentences: sentences.len() })
}

/// One chapter's sentences, numbered exactly as `chapter` numbers them.
pub fn chapter_sentences(src: &Source, asset_id: i64, index: usize) -> anyhow::Result<Vec<String>> {
    Ok(wrap_sentences(&chapter_dom(src, asset_id, index)?))
}

/// Every chapter's sentences.
pub fn all_sentences(src: &Source, asset_id: i64) -> anyhow::Result<Vec<Vec<String>>> {
    let n = layout(src)?.chapters;
    (0..n).map(|i| chapter_sentences(src, asset_id, i)).collect()
}

/// A file inside an EPUB (images, fonts) by its path in the archive.
pub fn resource(src: &Source, path: &str) -> anyhow::Result<(Vec<u8>, String)> {
    let Source::Epub(epub_path) = src else { bail!("not an EPUB") };
    let mut doc = epub::doc::EpubDoc::new(epub_path)?;
    let bytes = doc.get_resource_by_path(path).context("no such resource")?;
    let mime = doc.get_resource_mime_by_path(path).unwrap_or_else(|| mime_guess::from_path(path).first_or_octet_stream().to_string());
    Ok((bytes, mime))
}

/// Finds the spine index of a chapter file, for links between chapters.
pub fn chapter_of(src: &Source, path: &str) -> Option<usize> {
    let Source::Epub(epub_path) = src else { return None };
    let doc = epub::doc::EpubDoc::new(epub_path).ok()?;
    doc.resource_uri_to_chapter(&PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_across_inline_elements() {
        let doc = kuchikiki::parse_html().one("<body><p>One <em>two.</em> Three.</p><p>Four</p></body>").document_node;
        let body = doc.select_first("body").unwrap().as_node().clone();
        let s = wrap_sentences(&body);
        assert_eq!(s, ["One two.", "Three.", "Four"]);
        let html = body.to_string();
        assert!(html.contains(r#"<span class="s" data-s="0">One </span><em><span class="s" data-s="0">two.</span></em>"#), "{html}");
        assert!(html.contains(r#"<span class="s" data-s="2">Four</span>"#));
    }

    #[test]
    fn resolves_paths() {
        assert_eq!(resolve("OEBPS/text/ch1.xhtml", "../images/a.png"), "OEBPS/images/a.png");
    }

    #[test]
    fn rewrites_svg_image_hrefs() {
        // Calibre-style SVG cover page: the image reference lives in the
        // xlink namespace, which plain attribute getters can't see.
        let doc = kuchikiki::parse_html()
            .one(r#"<body><div><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image width="780" height="1186" xlink:href="cover.jpeg"></image></svg></div></body>"#)
            .document_node;
        let body = doc.select_first("body").unwrap().as_node().clone();
        rewrite_resource_urls(&body, 4, "titlepage.xhtml");
        let html = body.to_string();
        assert!(html.contains("/api/assets/4/res/cover.jpeg"), "{html}");
        assert!(!html.contains("xlink:href=\"cover.jpeg\""), "{html}");
    }

    #[test]
    fn splits_text_chapters() {
        let c = text_chapters("Title page\n\nCHAPTER I\nIt began.\n\nChapter 2\nIt ended.\n");
        assert_eq!(c.len(), 3);
        assert_eq!(c[1].0, "CHAPTER I");
    }
}
