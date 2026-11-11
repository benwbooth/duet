//! Anna's Archive: search the mirror's listing pages and resolve member fast
//! downloads. There is no public search API, and the site challenges anonymous
//! visitors (DDoS-Guard), so searching needs the member secret key — the same
//! key the fast-download API uses. The server trades it for a session cookie
//! held in memory (never logged, never written out) keyed by mirror.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Context;
use duet_common::{AssetKind, DownloadTarget, SearchResult};
use kuchikiki::traits::TendrilSink;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
/// Firefox UA, used when presenting the user's browser cookies: the check was
/// solved in Firefox, and the cookies may be bound to that UA.
const USER_AGENT_FF: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";
const SESSION_TTL: Duration = Duration::from_secs(6 * 3600);

pub struct AnnasArchive {
    /// mirror -> (session cookie, logged in at). In memory only.
    sessions: Mutex<HashMap<String, (String, Instant)>>,
    /// Remote-controlled Firefox: the one browser that searches when the
    /// direct request is challenged. Its session outlives searches.
    selenium: crate::selenium::Selenium,
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_md5(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn safe_filename(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || " -_.,'()".contains(c) { c } else { '_' }).collect::<String>().trim().to_string()
}

/// "English [en] · EPUB · 0.4MB · 2014 · 📕 Book (fiction) · ..." -> "epub".
/// Scans segments rather than hard-coding an index, mirroring the site's own
/// parser: the format is a short alphabetic token that starts with a letter
/// (so the year and the size never match) and is not a "[en]" language tag.
pub fn extract_format(meta: &str) -> Option<String> {
    for seg in meta.split('·').map(str::trim) {
        let token = seg.to_lowercase();
        let stripped = token.replace('-', "");
        if seg.contains('[') || token.len() > 6 || stripped.is_empty() {
            continue;
        }
        let mut chars = stripped.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_alphabetic());
        if first_ok && stripped.chars().all(|c| c.is_alphabetic() || c.is_ascii_digit()) {
            return Some(token);
        }
    }
    None
}

/// "0.4MB" / "1.4GB" / "523KB" -> bytes.
pub fn parse_size_token(seg: &str) -> Option<i64> {
    let seg = seg.trim().to_uppercase().replace(' ', "");
    let (num, mult) = if let Some(n) = seg.strip_suffix("GB") {
        (n, 1_000_000_000f64)
    } else if let Some(n) = seg.strip_suffix("MB") {
        (n, 1_000_000f64)
    } else if let Some(n) = seg.strip_suffix("KB") {
        (n, 1_000f64)
    } else {
        return None;
    };
    num.parse::<f64>().ok().map(|n| (n * mult) as i64)
}

/// Finds an Anna's Archive content id (`aacid__<collection>__<ts>__<id>__<uuid>`)
/// in page text. The bulk torrents store each book under this bare name
/// (no extension), so it is how a record maps to its torrent file.
pub fn extract_aacid(text: &str) -> Option<String> {
    let needle = "aacid__";
    let mut start = 0;
    while let Some(pos) = text[start..].find(needle) {
        let offset = start + pos;
        let mut end = offset + needle.len();
        // aacid characters: letters, digits, underscore (and hyphens in some ids)
        for c in text[end..].chars() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                end += c.len_utf8();
            } else {
                break;
            }
        }
        let candidate = &text[offset..end];
        // Real ids are long (aacid__collection__timestamp__id__uuid) and
        // contain at least two double-underscore separators after the prefix.
        if candidate.len() >= 30 && candidate[needle.len()..].contains("__") {
            return Some(candidate.to_string());
        }
        start = offset + needle.len();
        if start >= text.len() {
            break;
        }
    }
    None
}

/// Reads Anna's Archive cookies straight out of the remote Firefox's profile.
/// The `aa-browser` sidecar's `/config` is mounted on the host (Settings →
/// Browser profile folder); after the user passes the bot check in the
/// browser, its `cookies.sqlite` holds the solved session, so Discover can
/// search with zero copy-paste. Returns the `Cookie` header value.
///
/// Per-mirror: DDoS-Guard sets same-named cookies with different values per
/// mirror (verified: `__ddg*` differ across .gl/.gd/.pk), so only the target
/// mirror's cookies are returned — mixing hosts corrupts the session both
/// for direct HTTP (duplicate names) and for WebDriver planting (wrong
/// mirror's value wins on the cookie's domain).
pub async fn harvest_browser_cookies_for(profile_dir: &str, mirror: &str) -> Option<String> {
    let rows = harvest_aa_rows(profile_dir).await?;
    let want = mirror_host(mirror);
    let mut out = Vec::new();
    for (host, name, value) in rows {
        let h = host.trim_start_matches('.').to_lowercase();
        if want.is_empty() || want == h || want.ends_with(&format!(".{h}")) {
            out.push(format!("{name}={value}"));
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out.join("; "))
}

/// "https://annas-archive.gl/search?q=x" -> "annas-archive.gl".
fn mirror_host(mirror: &str) -> String {
    let rest = mirror.split_once("://").map(|(_, r)| r).unwrap_or(mirror);
    rest.split('/').next().unwrap_or("").to_lowercase()
}

async fn harvest_aa_rows(profile_dir: &str) -> Option<Vec<(String, String, String)>> {
    let dir = profile_dir.trim().to_string();
    if dir.is_empty() {
        return None;
    }
    // Firefox may hold the DB open: find it, copy to temp, read the copy.
    let found = tokio::task::spawn_blocking(move || {
        let mut stack = vec![(std::path::PathBuf::from(&dir), 0)];
        while let Some((d, depth)) = stack.pop() {
            if depth > 4 {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push((path, depth + 1));
                } else if path.file_name().map(|n| n == "cookies.sqlite").unwrap_or(false) {
                    return Some(path);
                }
            }
        }
        None
    })
    .await
    .ok()??;
    static HARVEST_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = HARVEST_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("duet-aa-cookies-{}-{n}.sqlite", std::process::id()));
    // Copy the WAL and SHM sidecars too: a fresh solve lives in the WAL
    // and the main file alone reads stale without them.
    let tmp_str = tmp.display().to_string();
    for suffix in ["", "-wal", "-shm"] {
        let src = format!("{}{}", found.display(), suffix);
        let dst = format!("{tmp_str}{suffix}");
        let _ = tokio::fs::copy(&src, &dst).await;
    }
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=ro", tmp.display())).await.ok()?;
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT host, name, value FROM moz_cookies WHERE host LIKE '%annas-archive%'")
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
    pool.close().await;
    for suffix in ["", "-wal", "-shm"] {
        let _ = tokio::fs::remove_file(format!("{tmp_str}{suffix}")).await;
    }
    if rows.is_empty() {
        return None;
    }
    Some(rows)
}

/// Splits an AACID into (collection, second-timestamp).
/// Format: `aacid__<collection>__<YYYYMMDDTHHMMSSZ>__<id>__<uuid>`.
pub fn parse_aacid_parts(aacid: &str) -> Option<(String, String)> {
    let rest = aacid.strip_prefix("aacid__")?;
    // The timestamp is the one segment that is 16 chars of digits/T/Z
    // (e.g. 20260827T033847Z). Find it by scanning __-separated segments.
    for seg in rest.split("__") {
        if seg.len() == 16 && seg.ends_with('Z') && seg.chars().all(|c| c.is_ascii_digit() || c == 'T' || c == 'Z') {
            // Collection is everything before this segment.
            let collection = rest.split("__").next()?.to_string();
            return Some((collection, seg.to_string()));
        }
    }
    None
}

/// Advances a YYYYMMDDTHHMMSSZ timestamp by one second, same format.
pub fn next_second(ts: &str) -> Option<String> {
    let dt = chrono::NaiveDateTime::parse_from_str(ts, "%Y%m%dT%H%M%SZ").ok()?;
    let next = dt + chrono::Duration::seconds(1);
    Some(next.format("%Y%m%dT%H%M%SZ").to_string())
}

/// Derives the shard torrent URL for an AACID — no database needed.
///
/// Shards are 1-second windows on the dump timestamp:
/// `annas_archive_data__aacid__<collection>__<ts>--<next-ts>.torrent`,
/// so collection+timestamp from the AACID names the exact shard that
/// holds the file (the AACID itself is the file name inside it).
pub fn shard_torrent_url(mirror: &str, aacid: &str) -> Option<String> {
    let (collection, ts) = parse_aacid_parts(aacid)?;
    let next = next_second(&ts)?;
    let mirror = mirror.trim_end_matches('/');
    let shard = format!("annas_archive_data__aacid__{collection}__{ts}--{next}.torrent");
    Some(format!("{mirror}/dyn/small_file/torrents/managed_by_aa/annas_archive_data__aacid/{shard}"))
}

fn is_challenge_body(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("ddos-guard") || lower.contains("checking your browser") || lower.contains("just a moment")
}

fn is_challenge(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap, body: &str) -> bool {
    // A 403 here is Anna's bot wall (DDoS-Guard); the header isn't always sent.
    if status == reqwest::StatusCode::FORBIDDEN {
        return true;
    }
    let server = headers.get(reqwest::header::SERVER).and_then(|v| v.to_str().ok()).unwrap_or("");
    if server.to_ascii_lowercase().contains("ddos-guard") {
        return true;
    }
    is_challenge_body(body)
}

impl AnnasArchive {
    pub fn new() -> Self {
        AnnasArchive { sessions: Mutex::new(HashMap::new()), selenium: crate::selenium::Selenium::new() }
    }

    async fn login(&self, http: &reqwest::Client, mirror: &str, key: &str) -> anyhow::Result<String> {
        let resp = http
            .post(format!("{mirror}/account/"))
            .header("User-Agent", USER_AGENT)
            .header("Accept-Language", "en-US,en;q=0.9")
            .form(&[("key", key)])
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .context("reaching Anna's Archive")?;
        let cookie = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|c| c.to_str().ok())
            .map(|c| c.split(';').next().unwrap_or("").trim())
            .filter(|c| c.starts_with("aa_") && c.contains('='))
            .collect::<Vec<_>>()
            .join("; ");
        if cookie.is_empty() {
            anyhow::bail!("Anna's Archive rejected the secret key — check it in Settings → Free sources");
        }
        self.sessions.lock().unwrap().insert(mirror.to_string(), (cookie.clone(), Instant::now()));
        Ok(cookie)
    }

    async fn session(&self, http: &reqwest::Client, mirror: &str, key: &str, force: bool) -> anyhow::Result<Option<String>> {
        if key.is_empty() {
            return Ok(None);
        }
        if !force {
            let cache = self.sessions.lock().unwrap();
            if let Some((cookie, at)) = cache.get(mirror) {
                if at.elapsed() < SESSION_TTL {
                    return Ok(Some(cookie.clone()));
                }
            }
        }
        Ok(Some(self.login(http, mirror, key).await?))
    }

    async fn fetch_search_page(&self, http: &reqwest::Client, mirror: &str, key: &str, browser_cookies: &str, q: &str, force: bool) -> anyhow::Result<String> {
        let url = format!("{mirror}/search?q={}", urlencoding::encode(q));
        // Pass 1 uses any cached session; pass 2 forces a fresh login (an
        // expired session looks exactly like being logged out).
        for i in 0..2 {
            let fresh = force || i == 1;
            let cookie = if !browser_cookies.trim().is_empty() {
                Some(browser_cookies.trim().to_string())
            } else if key.is_empty() {
                if i == 1 {
                    break;
                }
                None
            } else {
                // A bad/expired key must not hard-fail the search: fall back
                // to an anonymous request and let the challenge check below
                // decide. A fresh login is only retried once.
                match self.session(http, mirror, key, fresh).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Anna's Archive key login failed ({e:#}); trying anonymous search");
                        None
                    }
                }
            };
            let mut req = http.get(&url).header("User-Agent", if cookie.is_some() { USER_AGENT_FF } else { USER_AGENT }).header("Accept-Language", "en-US,en;q=0.9").timeout(std::time::Duration::from_secs(25));
            if let Some(cookie) = &cookie {
                req = req.header(reqwest::header::COOKIE, cookie);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                // DDoS-Guard's check page redirect-loops server-side clients
                // (?check=1 → ?check=1…): that loop IS the bot check.
                Err(e) if e.to_string().contains("check=1") => {
                    anyhow::bail!(
                        "BOT_CHECK Anna's Archive served a bot check instead of results. Solve it in the browser when asked, then retry the search."
                    );
                }
                Err(e) => return Err(e).context("reaching Anna's Archive"),
            };
            let status = resp.status();
            let headers = resp.headers().clone();
            let body = resp.text().await.unwrap_or_default();
            tracing::info!("aa fetch {mirror} -> {status} ({} bytes)", body.len());
            if is_challenge(status, &headers, &body) {
                if i == 0 && !key.is_empty() && browser_cookies.trim().is_empty() {
                    continue;
                }
                // BOT_CHECK: prefix lets the API tell the UI a human solve is
                // needed (browser modal); the UI strips it for display.
                anyhow::bail!(
                    "BOT_CHECK Anna's Archive served a bot check instead of results. Solve it in the browser when asked, then retry the search."
                );
            }
            if !status.is_success() {
                anyhow::bail!("Anna's Archive search failed (HTTP {status})");
            }
            return Ok(body);
        }
        anyhow::bail!("Anna's Archive search failed")
    }

    pub async fn search(&self, http: &reqwest::Client, mirror: &str, wd_url: &str, key: &str, browser_cookies: &str, q: &str) -> anyhow::Result<Vec<SearchResult>> {
        let mirror = mirror.trim_end_matches('/').to_string();
        // Fast path: member key or a harvested/pasted session, plain HTTP.
        let direct = self.fetch_search_page(http, &mirror, key, browser_cookies, q, false).await;
        let html = match direct {
            Ok(html) => {
                tracing::debug!("AA search via direct HTTP");
                html
            }
            Err(e) if e.to_string().contains("BOT_CHECK") && !wd_url.trim().is_empty() => {
                // Driven path: the remote-controlled Firefox IS the human's
                // visible browser, so its session is native. A human steps in
                // for manual checks in the viewer.
                tracing::info!("AA search via driven browser");
                self.search_via_selenium(http, wd_url, &mirror, q).await?
            }
            Err(e) => return Err(e),
        };
        let results: Vec<SearchResult> = parse_search(&html, &mirror);
        if results.is_empty() {
            anyhow::bail!("Anna's Archive returned no results — is the browser check solved, and the mirror current?");
        }
        let with_aacid = results.iter().filter(|r| r.aacid.is_some()).count();
        tracing::info!("Anna's Archive search: {} results, {} with AACID", results.len(), with_aacid);
        Ok(results.into_iter().take(25).collect())
    }

    /// Searches through the remote-controlled Firefox: a real session with a
    /// real fingerprint, so checks pass the way they do by hand. When the
    /// manual checkbox is up, the caller surfaces the viewer to a human and
    /// the next search attempt finds the solved session waiting.
    async fn search_via_selenium(&self, http: &reqwest::Client, wd_url: &str, mirror: &str, q: &str) -> anyhow::Result<String> {
        let url = format!("{mirror}/search?q={}", urlencoding::encode(q));
        match self.selenium.fetch_search(http, wd_url, &url).await? {
            crate::selenium::DrivenPage::Results(html) => {
                tracing::info!("selenium html: {} bytes, /md5/ x{}", html.len(), html.matches("/md5/").count());
                Ok(html)
            }
            crate::selenium::DrivenPage::NeedHuman => {
                anyhow::bail!(
                    "BOT_CHECK Anna's Archive served a bot check instead of results. Solve it in the browser when asked, then retry the search."
                );
            }
        }
    }

    /// Fetches a record's detail page and returns its AACID.
    /// Tries plain HTTP first (works with a session or when unchallenged),
    /// then the driven browser (same persistent session as search).
        pub async fn fetch_record_aacid(&self, http: &reqwest::Client, mirror: &str, md5: &str, browser_cookies: &str, wd_url: &str) -> anyhow::Result<Option<String>> {
        if !is_md5(md5) {
            return Ok(None);
        }
        let mirror = mirror.trim_end_matches('/');
        if let Ok(Some(aacid)) = self.fetch_record_direct(http, mirror, md5, browser_cookies).await {
            return Ok(Some(aacid));
        }
        if wd_url.trim().is_empty() {
            return Ok(None);
        }
        let url = format!("{mirror}/md5/{md5}");
        match self.selenium.fetch_search(http, wd_url, &url).await {
            Ok(crate::selenium::DrivenPage::Results(html)) => Ok(extract_aacid(&html)),
            _ => Ok(None),
        }
    }

        async fn fetch_record_direct(&self, http: &reqwest::Client, mirror: &str, md5: &str, browser_cookies: &str) -> anyhow::Result<Option<String>> {
        if !is_md5(md5) {
            return Ok(None);
        }
        let mirror = mirror.trim_end_matches('/');
        let mut req = http
            .get(format!("{mirror}/md5/{md5}"))
            .header("User-Agent", if browser_cookies.trim().is_empty() { USER_AGENT } else { USER_AGENT_FF })
            .header("Accept-Language", "en-US,en;q=0.9")
            .timeout(std::time::Duration::from_secs(20));
        if !browser_cookies.trim().is_empty() {
            req = req.header(reqwest::header::COOKIE, browser_cookies.trim());
        }
        let resp = req.send().await.context("reaching Anna's Archive record")?;
        let body = resp.text().await.unwrap_or_default();
        Ok(extract_aacid(&body))
    }


    /// Resolves a record into its time-limited fast-download URL.
    pub async fn resolve_download(&self, http: &reqwest::Client, mirror: &str, key: &str, md5: &str) -> anyhow::Result<String> {
        if !is_md5(md5) {
            anyhow::bail!("not an Anna's Archive record");
        }
        let mirror = mirror.trim_end_matches('/');
        let data: serde_json::Value = http
            .get(format!("{mirror}/dyn/api/fast_download.json"))
            .query(&[("md5", md5), ("key", key)])
            .header("User-Agent", USER_AGENT)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .context("reaching Anna's Archive")?
            .error_for_status()
            .context("Anna's Archive fast download failed")?
            .json()
            .await?;
        for field in ["download_url", "url", "fast_download_url", "download"] {
            if let Some(u) = data[field].as_str() {
                if !u.is_empty() {
                    return Ok(u.to_string());
                }
            }
        }
        let err = data["error"].as_str().unwrap_or("Anna's Archive fast download failed");
        anyhow::bail!("{err}")
    }
}

/// Scrapes one search page's result cards into results.
pub fn parse_search(html: &str, mirror: &str) -> Vec<SearchResult> {
    let doc = kuchikiki::parse_html().one(html).document_node;
    let mut out = Vec::new();
    for link in doc.select("a").into_iter().flatten() {
        let href = link.attributes.borrow().get("href").unwrap_or("").to_string();
        let class = link.attributes.borrow().get("class").unwrap_or("").to_string();
        if !class.contains("js-vim-focus") {
            continue;
        }
        let md5 = href.rsplit('/').next().unwrap_or("").to_string();
        if !href.starts_with("/md5/") || !is_md5(&md5) {
            continue;
        }
        let title = clean(&link.text_contents());
        if title.is_empty() {
            continue;
        }
        // Walk up to the result card (the site marks cards with "flex pt-3").
        let mut card = link.as_node().clone();
        for _ in 0..6 {
            let Some(parent) = card.parent() else { break };
            card = parent;
            if let Some(el) = card.as_element() {
                let cls = el.attributes.borrow().get("class").unwrap_or("").to_string();
                if cls.split_whitespace().any(|c| c == "flex") && cls.split_whitespace().any(|c| c == "pt-3") {
                    break;
                }
            }
        }
        // Author: the icon span (mdi--user-edit) sits inside its link.
        let mut author: Option<String> = None;
        for span in card.select("span").into_iter().flatten() {
            let cls = span.attributes.borrow().get("class").unwrap_or("").to_string();
            if !cls.contains("mdi--user-edit") {
                continue;
            }
            let mut node = span.as_node().clone();
            for _ in 0..3 {
                let Some(parent) = node.parent() else { break };
                node = parent;
                let text = clean(&node.text_contents());
                if !text.is_empty() && !text.contains('·') {
                    author = Some(text);
                    break;
                }
            }
            break;
        }
        // Metadata line: "English [en] · EPUB · 0.4MB · 2014 · ..."
        let format = card
            .select("div")
            .into_iter()
            .flatten()
            .find(|div| {
                let cls = div.attributes.borrow().get("class").unwrap_or("").to_string();
                cls.contains("font-semibold") && cls.contains("text-sm") && cls.contains("leading-[1.2]")
            })
            .and_then(|div| {
                let text = div.text_contents();
                let size = text.split('·').filter_map(|seg| parse_size_token(seg)).next();
                Some((extract_format(&text), size))
            });
        let (format, size) = format.unwrap_or_default();
        let filename = safe_filename(&match (&author, format.as_deref()) {
            (Some(a), Some(f)) => format!("{a} - {title}.{f}"),
            (_, Some(f)) => format!("{title}.{f}"),
            (Some(a), _) => format!("{a} - {title}"),
            _ => title.clone(),
        });
        let card_text = card.text_contents();
        // AACID may live in text, in an attribute (hidden input, href, etc.)
        // or nowhere — scan visible text plus every descendant attribute.
        let mut aacid = extract_aacid(&card_text);
        // Torrent links live in the card too: prefer per-file shard torrents
        // (annas_archive_data__aacid__... holds bare AACID files); skip
        // single-file tar dumps (pilimi-*, *.tar) — nothing to select in them.
        let mut torrent_url: Option<String> = None;
        let mut bulk_torrent: Option<String> = None;
        for a in card.select("a").into_iter().flatten() {
            let href = a.attributes.borrow().get("href").unwrap_or("").to_string();
            if !(href.ends_with(".torrent") || href.starts_with("magnet:")) {
                continue;
            }
            let abs = if href.starts_with("http") || href.starts_with("magnet:") {
                href.clone()
            } else {
                format!("{mirror}{}", if href.starts_with('/') { href.clone() } else { format!("/{href}") })
            };
            if abs.contains("annas_archive_data__aacid__") {
                torrent_url = Some(abs);
                break;
            }
            if bulk_torrent.is_none() {
                bulk_torrent = Some(abs);
            }
        }
        if torrent_url.is_none() {
            torrent_url = bulk_torrent;
        }
        if aacid.is_none() {
            for el in card.select("*").into_iter().flatten() {
                let attrs: Vec<String> = el.attributes.borrow().map.iter().map(|(_, v)| v.value.clone()).collect();
                for v in attrs {
                    if let Some(found) = extract_aacid(v.as_str()) {
                        aacid = Some(found);
                        break;
                    }
                }
                if aacid.is_some() {
                    break;
                }
            }
        }
        out.push(SearchResult {
            source: "Anna's Archive".into(),
            title,
            author,
            kind: AssetKind::Ebook,
            size,
            seeders: None,
            download: DownloadTarget::Direct { url: format!("{mirror}/md5/{md5}"), filename },
            info_url: Some(format!("{mirror}/md5/{md5}")),
            format,
            aacid,
            torrent_url,
        });
        if out.len() >= 50 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_format() {
        assert_eq!(extract_format("English [en] · EPUB · 0.4MB · 2014 · 📕 Book (fiction)"), Some("epub".into()));
        assert_eq!(extract_format("2024 · 📕"), None);
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size_token("0.4MB"), Some(400_000));
        assert_eq!(parse_size_token("1.4GB"), Some(1_400_000_000));
    }

    #[test]
    fn extracts_aacids() {
        let page = "hidden aacid__zlib3_files__20250827T033847Z__118660007__AZy3Qd44FBV3UY4y6dX8dh end";
        assert_eq!(
            extract_aacid(page).as_deref(),
            Some("aacid__zlib3_files__20250827T033847Z__118660007__AZy3Qd44FBV3UY4y6dX8dh")
        );
        assert_eq!(extract_aacid("no id here"), None);
    }

    #[test]
    fn derives_shard_from_aacid() {
        let aacid = "aacid__zlib3_files__20250827T033847Z__118660007__AZy3Qd44FBV3UY4y6dX8dh";
        assert_eq!(parse_aacid_parts(aacid), Some(("zlib3_files".into(), "20250827T033847Z".into())));
        let url = shard_torrent_url("https://annas-archive.gl", aacid).unwrap();
        assert_eq!(
            url,
            "https://annas-archive.gl/dyn/small_file/torrents/managed_by_aa/annas_archive_data__aacid/annas_archive_data__aacid__zlib3_files__20250827T033847Z--20250827T033848Z.torrent"
        );
        // Minute rollover: seconds carry into the minute.
        assert_eq!(next_second("20250827T033859Z").as_deref(), Some("20250827T033900Z"));
    }

    #[test]
    fn parses_cards() {
        let html = r#"<html><body>
            <div class="flex pt-3"><a class="js-vim-focus" href="/md5/0123456789abcdef0123456789abcdef">Clean Code</a>
            <a href="/search"><span class="icon-[mdi--user-edit]"></span>Robert C. Martin</a>
            <div class="font-semibold text-sm leading-[1.2]">English [en] · EPUB · 0.4MB · 2014</div>
            <input type="hidden" value="aacid__zlib3_files__20250101T000000Z__123__AbCdEfGhIjKlMnOpQrStUv">
            <a href="/dyn/small_file/torrents/managed_by_aa/annas_archive_data__aacid/annas_archive_data__aacid__zlib3_files__20250101T000000Z--20250101T000001Z.torrent">bulk</a>
            <a href="/dyn/small_file/torrents/managed_by_aa/zlib/pilimi-zlib2-15680000-16179999.torrent">tar</a></div>
            </body></html>"#;
        let results = parse_search(html, "https://annas-archive.gl");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Clean Code");
        assert_eq!(results[0].author.as_deref(), Some("Robert C. Martin"));
        assert_eq!(results[0].format.as_deref(), Some("epub"));
        assert_eq!(results[0].size, Some(400_000));
        assert_eq!(results[0].aacid.as_deref(), Some("aacid__zlib3_files__20250101T000000Z__123__AbCdEfGhIjKlMnOpQrStUv"));
        // Shard links win over tar dumps.
        assert_eq!(results[0].torrent_url.as_deref(), Some("https://annas-archive.gl/dyn/small_file/torrents/managed_by_aa/annas_archive_data__aacid/annas_archive_data__aacid__zlib3_files__20250101T000000Z--20250101T000001Z.torrent"));
    }

    #[test]
    fn skips_tar_without_shard() {
        let html = r#"<html><body>
            <div class="flex pt-3"><a class="js-vim-focus" href="/md5/0123456789abcdef0123456789abcdef">Clean Code</a>
            <div class="font-semibold text-sm leading-[1.2]">English [en] · EPUB · 0.4MB</div>
            <a href="/dyn/small_file/torrents/managed_by_aa/zlib/pilimi-zlib2-15680000-16179999.torrent">tar</a></div>
            </body></html>"#;
        let results = parse_search(html, "https://annas-archive.gl");
        assert_eq!(results.len(), 1);
        // A lone tar link is still recorded (caller decides); it just never
        // gets picked for selective download without an AACID shard.
        assert!(results[0].torrent_url.as_deref().is_some_and(|u| u.contains("pilimi")));
    }
}
