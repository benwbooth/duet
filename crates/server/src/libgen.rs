//! Library Genesis ("LibGen", plus its RandomBook / libgen.pw / libgen.me
//! mirrors, which are the same database): search the JSON API
//! (`/api/search/by-params`) on current mirrors, falling back to the old
//! listing pages, and download individual files through the links API
//! (`/api/download/links-by-id`) or the legacy `/ads.php` → `/get.php`
//! hop. No login, no captcha.
//! Z-Library and Sci-Hub, by contrast, sit behind bot challenges on every
//! Open SLUM mirror and can't be scraped this way; their books are reached
//! through Anna's Archive search (which indexes them) and the torrent
//! catalogue instead.

use anyhow::Context;
use duet_common::{AssetKind, DownloadTarget, SearchResult, text::{normalize, significant_words, title_key}};
use kuchikiki::traits::TendrilSink;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

pub struct LibGen;

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_md5(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// "630 kB" / "5 MB" -> bytes.
pub fn parse_size(text: &str) -> Option<i64> {
    let t = text.trim().replace('\u{00a0}', " ");
    let lower = t.to_ascii_lowercase();
    for (suffix, mult) in [("mib", 1_048_576f64), ("kb", 1_000.0), ("mb", 1_000_000.0), ("gb", 1_000_000_000.0), ("kib", 1_024.0)] {
        if let Some(pos) = lower.find(suffix) {
            let before = lower[..pos].trim_end();
            let digits: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if let Ok(n) = digits.replace(',', ".").parse::<f64>() {
                return Some((n * mult) as i64);
            }
        }
    }
    None
}

pub fn parse_search(html: &str, base: &str, max: usize) -> Vec<(SearchResult, String)> {
    let doc = kuchikiki::parse_html().one(html).document_node;
    let mut out = Vec::new();
    for row in doc.select("table#tablelibgen tbody tr").into_iter().flatten() {
        let node = row.as_node();
        let cells: Vec<String> = node
            .select("td")
            .into_iter()
            .flatten()
            .map(|td| clean(&td.text_contents()))
            .collect();
        if cells.len() < 9 {
            continue;
        }
        // Title: first cell (<b> or the edition link), minus LibGen's
        // trailing catalogue metadata (ISBNs, flags, ids) and any
        // doubled/garbled text the catalogue holds.
        let author = {
            let a = cells[1].trim().trim_end_matches([',', ';']).trim().to_string();
            if a.is_empty() { None } else { Some(a) }
        };
        let avec: Vec<String> = author.iter().cloned().collect();
        let title = deglitch_title(&split_author_prefix(&clean_title(&cells[0]), &avec));
        if title.is_empty() {
            continue;
        }
        let format = {
            let f = cells[7].trim().to_lowercase();
            if f.is_empty() || f.len() > 8 { None } else { Some(f) }
        };
        let size = parse_size(&cells[6]);
        // The Md5 rides in the Mirrors cell: /ads.php?md5=… (also linked to
        // RandomBook/libgen.pw/libgen.me/Anna's Archive, all the same book).
        let node_ref = row.as_node();
        let mut md5: Option<String> = None;
        for a in node_ref.select("a").into_iter().flatten() {
            let href = a.attributes.borrow().get("href").unwrap_or("").to_string();
            if let Some(pos) = href.find("md5=") {
                let candidate = href[pos + 4..].split('&').next().unwrap_or("");
                if is_md5(candidate) {
                    md5 = Some(candidate.to_string());
                    break;
                }
            }
        }
        let Some(md5) = md5 else { continue };
        let filename = safe_filename(&match (&author, &format) {
            (Some(a), Some(f)) => format!("{a} - {title}.{f}"),
            (_, Some(f)) => format!("{title}.{f}"),
            (Some(a), _) => format!("{a} - {title}"),
            _ => title.clone(),
        });
        // cells[4] is the catalogue language ("English"); defensive:
        // only keep short alphabetic values.
        let lang = cells
            .get(4)
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty() && l.len() <= 12 && l.chars().all(|c| c.is_ascii_alphabetic()))
            .unwrap_or_default();
        out.push((
            SearchResult {
                source: "LibGen".into(),
                title,
                author,
                kind: AssetKind::Ebook,
                size,
                seeders: None,
                download: DownloadTarget::Direct { url: format!("{base}/ads.php?md5={md5}"), filename },
                info_url: Some(format!("{base}/file.php?md5={md5}")),
                format,
                aacid: None,
                torrent_url: None,
            },
            lang,
        ));
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Source-data glitches in new-API titles:
/// - "Author - Title" prefix duplicating the author field
///   ("King, Stephen - The Institute" with author ["King, Stephen"]).
/// - a repeated base stuck on the end
///   ("The Institute (v5.1)The Institute").
/// Both break Discover's edition grouping (different title keys split one
/// book into several cards), so strip them at the mapping stage.
fn split_author_prefix<'a>(title: &'a str, authors: &[String]) -> &'a str {
    if let Some((pre, rest)) = title.split_once(" - ") {
        let np = normalize(pre);
        if !np.is_empty() && authors.iter().any(|a| normalize(a) == np) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return rest;
            }
        }
    }
    title
}

fn deglitch_title(t: &str) -> String {
    let t = t.trim();
    // Trailing repeat of the base ("The Institute (v5.1)The Institute").
    let base = t.split(['(', '[', ':']).next().unwrap_or(t).trim();
    if !base.is_empty() && base.len() < t.len() {
        if let Some(stripped) = t.strip_suffix(base) {
            let s = stripped.trim_end();
            if !s.is_empty() {
                return s.to_string();
            }
        }
        // Whole-title doubling ("The InstituteThe Institute").
        if t.len() % 2 == 0 {
            let (a, b) = t.split_at(t.len() / 2);
            if a == b {
                return a.trim().to_string();
            }
        }
    }
    t.to_string()
}

fn safe_filename(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || " -_.,'()".contains(c) { c } else { '_' }).collect::<String>().trim().to_string()
}

/// LibGen appends catalogue metadata to titles ("King Sorrow b f 7770763",
/// "Title 0062200607; 9780062200600 b f 10115181"): ISBN/ID runs and
/// trailing flag+id tails. Strip them so editions of one book group together.
pub fn clean_title(t: &str) -> String {
    let mut ws: Vec<&str> = t.split_whitespace().collect();
    // Strip catalogue metadata tails until stable. LibGen appends badge
    // groups like "b", "l 2992546", "f 2652387" (and ISBN runs): each pass
    // removes one trailing flag+id group or ISBN-ish token. Bare numbers
    // ("1984", "Room 101") have no flag letters, so they stay.
    loop {
        let n = ws.len();
        if n >= 2
            && ws[n - 1].chars().all(|c| c.is_ascii_digit())
            && !ws[n - 1].is_empty()
            && ws[n - 2].len() == 1
            && ws[n - 2].chars().all(|c| c.is_ascii_lowercase())
        {
            ws.pop();
            while ws.last().is_some_and(|w| w.len() == 1 && w.chars().all(|c| c.is_ascii_lowercase())) {
                ws.pop();
            }
            continue;
        }
        if ws.last().is_some_and(|w| w.chars().filter(|c| c.is_ascii_digit()).count() >= 7) {
            ws.pop();
            continue;
        }
        break;
    }
    ws.join(" ")
}

/// Resolves a LibGen download into the actual file URL.
/// Two eras: the new JSON mirrors serve `/api/download/links-by-id?id=…`
/// (signed links, resolved fresh every attempt); the old ones serve
/// `/ads.php?md5=…` → `/get.php?md5=…&key=…`.
pub async fn resolve_download(http: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    if url.contains("/api/download/links-by-id") {
        #[derive(serde::Deserialize)]
        struct LinksResult {
            #[serde(default)]
            links: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct LinksResp {
            result: LinksResult,
            #[serde(default, rename = "isError")]
            is_error: bool,
        }
        let raw = http
            .get(url)
            .header("User-Agent", USER_AGENT)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .context("reaching LibGen")?
            .error_for_status()?
            .text()
            .await?;
        let resp: LinksResp = serde_json::from_str(&raw).context("reading LibGen links")?;
        if resp.is_error {
            anyhow::bail!("LibGen has no download for this book right now");
        }
        let link = resp.result.links.into_iter().find(|l| !l.trim().is_empty()).context("LibGen gave no download links")?;
        return Ok(link);
    }
    let origin = reqwest::Url::parse(url).context("bad LibGen download URL")?;
    let base = format!("{}://{}{}", origin.scheme(), origin.host_str().unwrap_or_default(), origin.port_or_known_default().map(|p| format!(":{p}")).unwrap_or_default());
    let html = http
        .get(url)
        .header("User-Agent", USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .context("reaching LibGen")?
        .error_for_status()?
        .text()
        .await?;
    let start = html.find("get.php?").context("LibGen didn't show a download link — it may be busy or the mirror changed")?;
    let link = html[start..]
        .split(|c: char| c == '"' || c == '\'' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_string();
    if link.starts_with("http") {
        Ok(link)
    } else if link.starts_with('/') {
        Ok(format!("{base}{link}"))
    } else {
        Ok(format!("{base}/{link}"))
    }
}

/// One upload per (book, author, format, language): catalogues list
/// dozens of identical uploads (e.g. 37 "The Institute" epubs with
/// different MD5s/ids). Keeping them all buries real results and splits
/// Discover cards; the first (most relevant) link wins. Then rank: fully
/// query-covered records first, then most query words in the title, then
/// English — keeping source order within each tier, since API/table order
/// shuffles between calls.
fn finalize(paired: Vec<(SearchResult, String)>, query: &str) -> Vec<SearchResult> {
    let mut seen: Vec<String> = Vec::new();
    let mut unique: Vec<(SearchResult, String)> = Vec::new();
    for (r, lang) in paired {
        let akey = r.author.as_deref().map(|a| {
            let mut ws: Vec<String> = normalize(a).split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect();
            ws.sort_unstable();
            ws.join(" ")
        }).unwrap_or_default();
        let dkey = format!(
            "{}|{}|{}|{}",
            title_key(&r.title),
            akey,
            r.format.as_deref().unwrap_or(""),
            lang
        );
        if seen.contains(&dkey) {
            continue;
        }
        seen.push(dkey);
        unique.push((r, lang));
    }
    let qwords = significant_words(query);
    let words_of = |s: &str| -> Vec<String> { normalize(s).split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect() };
    let mut idx: Vec<usize> = (0..unique.len()).collect();
    idx.sort_by_key(|&i| {
        let r = &unique[i].0;
        let tw = words_of(&r.title);
        let aw = r.author.as_deref().map(words_of).unwrap_or_default();
        let hits = qwords.iter().filter(|w| tw.iter().any(|x| x == *w)).count();
        let covered = qwords.iter().all(|w| tw.iter().any(|x| x == w) || aw.iter().any(|x| x == w));
        let eng = if unique[i].1 == "eng" || unique[i].1 == "english" { 0 } else { 1 };
        ((!covered) as u8, std::cmp::Reverse(hits), eng, i)
    });
    idx.truncate(25);
    idx.into_iter().map(|i| unique[i].0.clone()).collect()
}

impl LibGen {
    /// The new mirrors (libgen.pw/.me/.xyz) are a JS app backed by a JSON
    /// API; the classic `index.php` table pages are dead there (nginx
    /// parking page). This hits the API directly — no JS rendering needed.
    fn search_json(html: &str, base: &str) -> Vec<(SearchResult, String)> {
        #[derive(serde::Deserialize)]
        struct ApiBook {
            #[serde(default)]
            id: String,
            #[serde(default)]
            title: String,
            #[serde(default)]
            author: Vec<String>,
            #[serde(default, rename = "fileExtension")]
            file_extension: Option<String>,
            #[serde(default, rename = "fileSize")]
            file_size: Option<i64>,
            #[serde(default)]
            language: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct ApiResult {
            #[serde(default)]
            books: Vec<ApiBook>,
        }
        #[derive(serde::Deserialize)]
        struct ApiResp {
            result: ApiResult,
        }
        let Ok(resp) = serde_json::from_str::<ApiResp>(html) else { return Vec::new() };
        let mut paired: Vec<(SearchResult, String)> = Vec::new();
        for b in resp.result.books {
            if b.title.trim().is_empty() || b.id.trim().is_empty() {
                continue;
            }
            let authors: Vec<String> = b.author
                .into_iter()
                .map(|a| clean(&a).trim_end_matches([',', ';']).trim().to_string())
                .filter(|a| !a.is_empty())
                .collect();
            let title = deglitch_title(split_author_prefix(b.title.trim(), &authors));
            if title.is_empty() {
                continue;
            }
            let format = b.file_extension.map(|f| f.trim().to_lowercase()).filter(|f| !f.is_empty() && f.len() <= 8);
            let lang = b.language.map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty());
            let author = if authors.is_empty() { None } else { Some(authors.join(", ")) };
            let filename = safe_filename(&match (&author, &format) {
                (Some(a), Some(f)) => format!("{a} - {title}.{f}"),
                (_, Some(f)) => format!("{title}.{f}"),
                (Some(a), _) => format!("{a} - {title}"),
                _ => title.clone(),
            });
            paired.push((SearchResult {
                source: "LibGen".into(),
                title,
                author,
                kind: AssetKind::Ebook,
                size: b.file_size,
                seeders: None,
                download: DownloadTarget::Direct {
                    url: format!("{base}/api/download/links-by-id?id={}", b.id),
                    filename,
                },
                info_url: Some(format!("{base}/book/{}", b.id)),
                format,
                aacid: None,
                torrent_url: None,
            }, lang.unwrap_or_default()));
        }
        paired
    }

    pub async fn search(http: &reqwest::Client, base_url: &str, q: &str) -> anyhow::Result<Vec<SearchResult>> {
        if q.trim().chars().count() < 2 {
            anyhow::bail!("LibGen needs at least 2 characters");
        }
        let base = base_url.trim_end_matches('/').to_string();
        // New JSON API first (current mirrors); the classic table scrape is
        // kept for any old mirror still serving it.
        for collection in ["main", "fiction"] {
            let url = format!("{base}/api/search/by-params?query={}&collection={collection}&from=0", urlencoding::encode(q));
            let text = match http.get(&url).header("User-Agent", USER_AGENT).timeout(std::time::Duration::from_secs(20)).send().await {
                Ok(r) => match r.error_for_status() {
                    Ok(r) => match r.text().await {
                        Ok(t) => t,
                        Err(_) => continue,
                    },
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            let out = finalize(Self::search_json(&text, &base), q);
            if !out.is_empty() {
                return Ok(out);
            }
        }
        // Two slices of the same index: topics[]=l first (prose editions;
        // catches the plain novels the default slice buries, e.g. "The
        // Hobbit"), then the unfiltered slice (catches books the topics
        // slice omits, e.g. "King Sorrow"). Merged and deduped by MD5.
        // One at a time.
        let mut merged: Vec<(SearchResult, String)> = Vec::new();
        for topics in [Some("l"), None] {
            let mut url = reqwest::Url::parse(&format!("{base}/index.php")).context("bad LibGen address")?;
            {
                let mut qp = url.query_pairs_mut();
                qp.append_pair("req", q);
                qp.append_pair("columns[]", "t");
                qp.append_pair("columns[]", "a");
                qp.append_pair("objects[]", "f");
                if let Some(t) = topics {
                    qp.append_pair("topics[]", t);
                }
                qp.append_pair("res", "50").append_pair("filesuns", "all");
            }
            let html = http.get(url).header("User-Agent", USER_AGENT).send().await.context("reaching LibGen")?.error_for_status()?.text().await?;
            merged.extend(parse_search(&html, &base, 100));
            if merged.len() >= 60 {
                break;
            }
        }
        if merged.is_empty() {
            anyhow::bail!("LibGen returned no results");
        }
        Ok(finalize(merged, q))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("630 kB"), Some(630_000));
        assert_eq!(parse_size("5 MB"), Some(5_000_000));
    }

    #[test]
    fn strips_catalogue_metadata() {
        assert_eq!(clean_title("King Sorrow b f 7770763"), "King Sorrow");
        assert_eq!(clean_title("King Sorrow 0062200607; 0062200623; 9780062200600; 9780062200624 b f 10115181"), "King Sorrow");
        assert_eq!(clean_title("The Sorrow King 0982628161; 9780982628164 b l 740943 f 472265"), "The Sorrow King");
        assert_eq!(clean_title("Pride & Prejudice"), "Pride & Prejudice");
        assert_eq!(clean_title("1984"), "1984");
        assert_eq!(clean_title("Room 101"), "Room 101");
        assert_eq!(clean_title("It b l 2992546"), "It");
        assert_eq!(clean_title("It b l 2992546 f 2652387"), "It");
    }

    #[test]
    fn splits_author_prefixed_titles() {
        let authors = vec!["King, Stephen".to_string()];
        assert_eq!(split_author_prefix("King, Stephen - The Institute", &authors), "The Institute");
        // Prefix that isn't the author stays.
        assert_eq!(split_author_prefix("Stephen King Omnibus 2017-2023 Part 5", &authors), "Stephen King Omnibus 2017-2023 Part 5");
        // Empty remainder keeps the original.
        assert_eq!(split_author_prefix("King, Stephen - ", &authors), "King, Stephen - ");
    }

    #[test]
    fn deglitches_repeated_titles() {
        assert_eq!(deglitch_title("The Institute (v5.1)The Institute"), "The Institute (v5.1)");
        assert_eq!(deglitch_title("The Institute"), "The Institute");
        assert_eq!(deglitch_title("The Institute: A Novel"), "The Institute: A Novel");
    }

    #[test]
    fn json_dedupes_uploads_and_ranks_exact_first() {
        let base = "https://libgen.pw";
        let q = "the Institute Stephen King";
        let raw = serde_json::json!({
            "result": {"books": [
                {"id": "1", "title": "Stephen King Omnibus 2017-2023 Part 5",
                 "author": ["Stephen King"], "fileExtension": "epub", "fileSize": 9, "language": "eng"},
                {"id": "2", "title": "King, Stephen - The Institute",
                 "author": ["King, Stephen"], "fileExtension": "azw3", "fileSize": 7, "language": "eng"},
                {"id": "3", "title": "The Institute (v5.1)The Institute",
                 "author": ["Stephen King"], "fileExtension": "epub", "fileSize": 8, "language": "eng"},
                {"id": "4", "title": "The Institute",
                 "author": ["Stephen King"], "fileExtension": "epub", "fileSize": 6, "language": "eng"},
                {"id": "5", "title": "The Institute",
                 "author": ["King, Stephen"], "fileExtension": "epub", "fileSize": 5, "language": "eng"},
                {"id": "6", "title": "The Institute",
                 "author": ["Stephen King"], "fileExtension": "mobi", "fileSize": 4, "language": "eng"},
            ]}
        })
        .to_string();
        let out = finalize(LibGen::search_json(&raw, base), q);
        // Name variants merge: one epub (first upload wins), one mobi, one
        // azw3 — plus the omnibus, sunk last for missing "institute".
        assert_eq!(out.len(), 4);
        // Fully query-covered records first despite API order.
        assert_eq!(out[0].title, "The Institute");
        assert_eq!(out[0].format.as_deref(), Some("azw3"));
        assert!(out[3].title.contains("Omnibus"));
        // No garbled titles survive.
        assert!(out.iter().all(|r| !r.title.ends_with("The Institute") || r.title == "The Institute"));
        assert!(out.iter().all(|r| !r.title.contains(" - The Institute")));

        // Same treatment for the table path: doubled titles deglitch and
        // same-book/different-md5 uploads merge.
        let table = r#"<table id="tablelibgen"><tbody>
            <tr><td>The Institute (v5.1)The Institute</td><td>Stephen King</td><td></td>
            <td>2019</td><td>English</td><td>0</td><td>2 MB</td><td>epub</td>
            <td><a href="/ads.php?md5=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa">1</a></td></tr>
            <tr><td>The Institute</td><td>Stephen King</td><td></td>
            <td>2019</td><td>English</td><td>0</td><td>3 MB</td><td>epub</td>
            <td><a href="/ads.php?md5=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb">1</a></td></tr>
            <tr><td>The Institute</td><td>Stephen King</td><td></td>
            <td>2019</td><td>English</td><td>0</td><td>1 MB</td><td>mobi</td>
            <td><a href="/ads.php?md5=cccccccccccccccccccccccccccccccc">1</a></td></tr>
            </tbody></table>"#;
        let tout = finalize(parse_search(table, base, 100), q);
        assert_eq!(tout.len(), 2);
        assert!(tout.iter().all(|r| r.title == "The Institute (v5.1)" || r.title == "The Institute"));
    }

    #[test]
    fn parses_rows() {
        let html = r#"<table id="tablelibgen"><tbody><tr>
            <td><a href="edition.php?id=1">Pride &amp; Prejudice</a></td>
            <td>Austen, Jane</td><td></td><td>1813</td><td>English</td><td>0</td>
            <td><a href="/file.php?id=1">385 kB</a></td><td>epub</td>
            <td><a href="/ads.php?md5=0123456789abcdef0123456789abcdef">1</a>
                <a href="https://randombook.org/book/0123456789abcdef0123456789abcdef">2</a></td>
            </tr></tbody></table>"#;
        let results = parse_search(html, "https://libgen.li", 25);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.title, "Pride & Prejudice");
        assert_eq!(results[0].0.format.as_deref(), Some("epub"));
        assert_eq!(results[0].0.size, Some(385_000));
        assert_eq!(results[0].1, "english");
        match &results[0].0.download {
            DownloadTarget::Direct { url, filename } => {
                assert_eq!(url, "https://libgen.li/ads.php?md5=0123456789abcdef0123456789abcdef");
                assert_eq!(filename, "Austen, Jane - Pride _ Prejudice.epub");
            }
            _ => panic!("expected direct download"),
        }
    }
}
