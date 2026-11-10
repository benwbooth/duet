//! Library Genesis ("LibGen", plus its RandomBook / libgen.pw / libgen.me
//! mirrors, which are the same database): search the listing pages and
//! download individual files through the `/ads.php` → `/get.php` hop. No
//! login, no captcha — plain table scraping, like every other LibGen client.
//! Z-Library and Sci-Hub, by contrast, sit behind bot challenges on every
//! Open SLUM mirror and can't be scraped this way; their books are reached
//! through Anna's Archive search (which indexes them) and the torrent
//! catalogue instead.

use anyhow::Context;
use duet_common::{AssetKind, DownloadTarget, SearchResult};
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

pub fn parse_search(html: &str, base: &str, max: usize) -> Vec<SearchResult> {
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
        // trailing catalogue metadata (ISBNs, flags, ids).
        let title = clean_title(&cells[0]);
        if title.is_empty() {
            continue;
        }
        let author = {
            let a = cells[1].clone();
            if a.is_empty() { None } else { Some(a) }
        };
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
        out.push(SearchResult {
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
        });
        if out.len() >= max {
            break;
        }
    }
    out
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

/// Resolves `/ads.php?md5=…` into the actual `/get.php?md5=…&key=…` URL.
pub async fn resolve_download(http: &reqwest::Client, url: &str) -> anyhow::Result<String> {
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

impl LibGen {
    pub async fn search(http: &reqwest::Client, base_url: &str, q: &str) -> anyhow::Result<Vec<SearchResult>> {
        if q.trim().chars().count() < 2 {
            anyhow::bail!("LibGen needs at least 2 characters");
        }
        let base = base_url.trim_end_matches('/').to_string();
        // Two slices of the same index: topics[]=l first (prose editions;
        // catches the plain novels the default slice buries, e.g. "The
        // Hobbit"), then the unfiltered slice (catches books the topics
        // slice omits, e.g. "King Sorrow"). Merged and deduped by MD5.
        // One at a time.
        let mut merged: Vec<SearchResult> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
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
            for r in parse_search(&html, &base, 50) {
                let md5 = match &r.download {
                    DownloadTarget::Direct { url, .. } => url.rsplit("md5=").next().unwrap_or("").to_string(),
                    _ => String::new(),
                };
                if !md5.is_empty() && seen.contains(&md5) {
                    continue;
                }
                seen.push(md5);
                merged.push(r);
                if merged.len() >= 25 {
                    break;
                }
            }
            if merged.len() >= 25 {
                break;
            }
        }
        if merged.is_empty() {
            anyhow::bail!("LibGen returned no results");
        }
        Ok(merged)
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
        assert_eq!(results[0].title, "Pride & Prejudice");
        assert_eq!(results[0].format.as_deref(), Some("epub"));
        assert_eq!(results[0].size, Some(385_000));
        match &results[0].download {
            DownloadTarget::Direct { url, filename } => {
                assert_eq!(url, "https://libgen.li/ads.php?md5=0123456789abcdef0123456789abcdef");
                assert_eq!(filename, "Austen, Jane - Pride _ Prejudice.epub");
            }
            _ => panic!("expected direct download"),
        }
    }
}
