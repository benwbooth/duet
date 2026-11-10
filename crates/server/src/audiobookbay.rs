//! Audiobook Bay: a community audiobook torrent index with no official API
//! and no login-free RSS carrying magnets. Searching follows the site's own
//! listing pages (`/?s=query`) and each torrent's detail page (`/abss/slug/`),
//! where the info hash and trackers live; magnets are built from those, the
//! same way qBittorrent's own ABB search plugin works. Detail fetches are
//! politely serialized (one at a time, capped) so a search can't hammer the
//! site into banning the IP.

use std::time::Duration;

use duet_common::{AssetKind, DownloadTarget, SearchResult};
use kuchikiki::traits::TendrilSink;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
/// Adding a well-known tracker set as well makes magnets robust when a
/// page's tracker table is empty; page-listed trackers come first.
const FALLBACK_TRACKERS: [&str; 4] = [
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://tracker.leechers-paradise.org:6969/announce",
    "udp://tracker.coppersurfer.tk:6969/announce",
    "udp://tracker.torrent.eu.org:451/announce",
];

pub struct AudiobookBay;

struct ListingEntry {
    title: String,
    href: String,
    size: Option<i64>,
    format: Option<String>,
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "281.35 MBs" / "1.2 GBs" / "1005.19 MBs" -> bytes.
pub fn parse_file_size(text: &str) -> Option<i64> {
    let lower = text.to_ascii_lowercase();
    for (suffix, mult) in [("gbs", 1_000_000_000f64), ("gib", 1_073_741_824.0), ("gb", 1_000_000_000.0), ("mbs", 1_000_000.0), ("mib", 1_048_576.0), ("mb", 1_000_000.0)] {
        if let Some(pos) = lower.find(suffix) {
            let before = text[..pos].trim_end();
            // The number sits right before the unit, after the last letter.
            let digits: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if let Ok(n) = digits.parse::<f64>() {
                return Some((n * mult) as i64);
            }
        }
    }
    None
}

/// The 40-hex v1 info hash shown on a detail page, if present.
fn find_info_hash(text: &str) -> Option<String> {
    for token in text.split(|c: char| !(c.is_ascii_alphanumeric())) {
        if token.len() == 40 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(token.to_ascii_lowercase());
        }
    }
    None
}

fn find_trackers(html: &str) -> Vec<String> {
    let doc = kuchikiki::parse_html().one(html).document_node;
    let mut out: Vec<String> = doc
        .select("td")
        .into_iter()
        .flatten()
        .map(|td| clean(&td.text_contents()))
        .filter(|t| (t.starts_with("udp://") || (t.starts_with("http://") && t.contains("announce"))) && t.contains("tracker"))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn build_magnet(hash: &str, title: &str, trackers: &[String]) -> String {
    let mut magnet = format!("magnet:?xt=urn:btih:{hash}&dn={}", urlencoding::encode(title));
    for tr in trackers.iter().map(String::as_str).chain(FALLBACK_TRACKERS.iter().copied()) {
        magnet.push_str(&format!("&tr={}", urlencoding::encode(tr)));
    }
    magnet
}

fn parse_listing(html: &str) -> Vec<ListingEntry> {
    let doc = kuchikiki::parse_html().one(html).document_node;
    let mut out = Vec::new();
    for post in doc.select("div.post").into_iter().flatten() {
        let node = post.as_node();
        let Some(link) = node.select_first("div.postTitle a").ok() else { continue };
        let href = link.attributes.borrow().get("href").unwrap_or("").to_string();
        let title = clean(&link.text_contents());
        if href.is_empty() || title.is_empty() {
            continue;
        }
        let text = node.text_contents();
        let size = text.split('\n').filter_map(parse_file_size).next();
        let format = text
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("Format:").map(|f| clean(f).to_lowercase()))
            .filter(|f| !f.is_empty() && f.len() <= 8);
        // Listing order on the site is newest-first; duplicates are different
        // uploads of the same book — keep the first (newest) only.
        if out.iter().any(|e: &ListingEntry| e.href == href) {
            continue;
        }
        out.push(ListingEntry { title, href, size, format });
    }
    out
}

impl AudiobookBay {
    /// One listing fetch. Returns the HTML and whether the site bounced the
    /// query straight home (nginx 301s on some tokens like single words or
    /// "Dr", which looks like an empty page — retrying quoted often works).
    async fn listing(http: &reqwest::Client, base: &str, q: &str) -> anyhow::Result<(String, bool)> {
        let resp = http
            .get(format!("{base}/"))
            .query(&[("s", q)])
            .header("User-Agent", USER_AGENT)
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Audiobook Bay isn't reachable — try again in a minute"))?;
        let resp = resp
            .error_for_status()
            .map_err(|_| anyhow::anyhow!("Audiobook Bay isn't reachable — try again in a minute"))?;
        let bounced = resp.url().query().is_none();
        let html = resp.text().await.unwrap_or_default();
        Ok((html, bounced))
    }

async fn fetch_one(http: reqwest::Client, base: String, entry: ListingEntry) -> Option<SearchResult> {
    let detail_url = if entry.href.starts_with("http") { entry.href.clone() } else { format!("{base}{}", entry.href) };
    let detail = http
        .get(&detail_url)
        .header("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .ok()
        .and_then(|r| r.error_for_status().ok());
    let detail = match detail {
        Some(r) => r.text().await.unwrap_or_default(),
        None => return None,
    };
    let hash = find_info_hash(&detail)?;
    let trackers = find_trackers(&detail);
    let size = entry.size.or_else(|| {
        detail
            .lines()
            .filter(|l| l.contains("File Size") || l.contains("file"))
            .filter_map(parse_file_size)
            .next()
    });
    let (title, author) = match entry.title.rsplit_once(" - ") {
        Some((t, a)) if !a.contains(|c: char| c.is_ascii_digit()) && t.len() > 2 => (t.to_string(), Some(a.to_string())),
        _ => (entry.title.clone(), None),
    };
    Some(SearchResult {
        source: "AudioBook Bay".into(),
        title,
        author,
        kind: AssetKind::Audiobook,
        size,
        seeders: None,
        download: DownloadTarget::Torrent { url: build_magnet(&hash, &entry.title, &trackers) },
        info_url: Some(detail_url),
        format: entry.format,
        aacid: None,
        torrent_url: None,
    })
}
    pub async fn search(http: &reqwest::Client, base_url: &str, q: &str) -> anyhow::Result<Vec<SearchResult>> {
        match tokio::time::timeout(std::time::Duration::from_secs(20), Self::search_inner(http, base_url, q)).await {
            Ok(r) => r,
            Err(_) => anyhow::bail!("Audiobook Bay isn't reachable right now (all mirrors timed out) — try again later, or add it to your Torznab indexer in Settings"),
        }
    }

    async fn search_inner(http: &reqwest::Client, base_url: &str, q: &str) -> anyhow::Result<Vec<SearchResult>> {
        let base = base_url.trim_end_matches('/').to_string();
        // Callers pass a cookie-store client (WordPress session cookies matter).
        let small = ["the", "a", "an", "of", "in", "and", "to", "on", "at", "for", "dr"];
        let words: Vec<String> = q
            .split(|c: char| !c.is_alphanumeric())
            .map(str::to_lowercase)
            .filter(|w| !w.is_empty() && !small.contains(&w.as_str()))
            .collect();
        let title_hits = |entries: &[ListingEntry]| {
            entries
                .iter()
                .filter(|e| {
                    let title = e.title.to_lowercase();
                    words.iter().any(|w| title.contains(w.as_str()))
                })
                .count()
        };
        let mut html = match Self::listing(http, &base, q).await {
            Ok((h, _)) if !parse_listing(&h).is_empty() => h,
            // Bounced or empty: retry in quotes ("Dune" and "Dr" queries
            // need it). A transport failure returns now.
            Ok(_) if !q.contains('"') => match Self::listing(http, &base, &format!("\"{q}\"")).await {
                Ok((h2, _)) if !parse_listing(&h2).is_empty() => h2,
                _ => return Err(anyhow::anyhow!("Audiobook Bay returned no results")),
            },
            Ok(_) => return Err(anyhow::anyhow!("Audiobook Bay returned no results")),
            Err(e) => return Err(e),
        };
        // The site matches keywords loosely: a non-empty listing can still be
        // all unrelated books. Retry quoted when no title matches at all.
        if !words.is_empty() && !q.contains('"') && title_hits(&parse_listing(&html)) == 0 {
            if let Ok((h2, _)) = Self::listing(http, &base, &format!("\"{q}\"")).await {
                if !parse_listing(&h2).is_empty() {
                    html = h2;
                }
            }
        }
        let mut entries = parse_listing(&html);
        // Best title matches first.
        {
            let mut scored: Vec<(usize, ListingEntry)> = entries
                .into_iter()
                .map(|e| {
                    let title = e.title.to_lowercase();
                    let score = words.iter().filter(|w| title.contains(w.as_str())).count();
                    (score, e)
                })
                .collect();
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
            entries = scored.into_iter().map(|(_, e)| e).collect();
        }
        entries.truncate(8);
        // Detail pages strictly one at a time: a single rate-limited site,
        // so duet never holds more than one connection open to it.
        // Bounded so a slow mirror is an error, never a spinner.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut out = Vec::new();
        for entry in entries {
            if std::time::Instant::now() > deadline {
                break;
            }
            if let Some(r) = Self::fetch_one(http.clone(), base.clone(), entry).await {
                out.push(r);
            }
        }
        if out.is_empty() {
            anyhow::bail!("Audiobook Bay isn't reachable right now (all mirrors timed out) — try again later, or add it to your Torznab indexer in Settings");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_file_sizes() {
        assert_eq!(parse_file_size("File Size: 281.35 MBs"), Some(281_350_000));
        assert_eq!(parse_file_size("1005.19 MBs"), Some(1_005_190_000));
    }

    #[test]
    fn finds_hashes_and_magnets() {
        assert_eq!(find_info_hash("Info Hash: 0123456789ABCDEF0123456789abcdef01234567"), Some("0123456789abcdef0123456789abcdef01234567".to_string()));
        let magnet = build_magnet("a".repeat(40).as_str(), "My Book", &["udp://tracker.example:6969/announce".to_string()]);
        assert!(magnet.starts_with("magnet:?xt=urn:btih:"));
        assert!(magnet.contains("&dn=My%20Book"));
    }
}
