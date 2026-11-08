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
const SESSION_TTL: Duration = Duration::from_secs(6 * 3600);

pub struct AnnasArchive {
    /// mirror -> (session cookie, logged in at). In memory only.
    sessions: Mutex<HashMap<String, (String, Instant)>>,
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

fn is_challenge(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap, body: &str) -> bool {
    // A 403 here is Anna's bot wall (DDoS-Guard); the header isn't always sent.
    if status == reqwest::StatusCode::FORBIDDEN {
        return true;
    }
    let server = headers.get(reqwest::header::SERVER).and_then(|v| v.to_str().ok()).unwrap_or("");
    if server.to_ascii_lowercase().contains("ddos-guard") {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("ddos-guard") || lower.contains("checking your browser") || lower.contains("just a moment")
}

impl AnnasArchive {
    pub fn new() -> Self {
        AnnasArchive { sessions: Mutex::new(HashMap::new()) }
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

    async fn fetch_search_page(&self, http: &reqwest::Client, mirror: &str, key: &str, q: &str, force: bool) -> anyhow::Result<String> {
        let url = format!("{mirror}/search?q={}", urlencoding::encode(q));
        // Pass 1 uses any cached session; pass 2 forces a fresh login (an
        // expired session looks exactly like being logged out).
        for i in 0..2 {
            let fresh = force || i == 1;
            if i == 1 && key.is_empty() {
                break;
            }
            let cookie = self.session(http, mirror, key, fresh).await?;
            let mut req = http.get(&url).header("User-Agent", USER_AGENT).header("Accept-Language", "en-US,en;q=0.9").timeout(std::time::Duration::from_secs(25));
            if let Some(cookie) = &cookie {
                req = req.header(reqwest::header::COOKIE, cookie);
            }
            let resp = req.send().await.context("reaching Anna's Archive")?;
            let status = resp.status();
            let headers = resp.headers().clone();
            let body = resp.text().await.unwrap_or_default();
            if is_challenge(status, &headers, &body) {
                if i == 0 && !key.is_empty() {
                    continue;
                }
                if key.is_empty() {
                    anyhow::bail!(
                        "Anna's Archive served a bot check instead of results. Add your member secret key in Settings → Free sources — signed-in searches skip the check"
                    );
                }
                anyhow::bail!("Anna's Archive served a bot check instead of results — try again, or re-enter the secret key");
            }
            if !status.is_success() {
                anyhow::bail!("Anna's Archive search failed (HTTP {status})");
            }
            return Ok(body);
        }
        anyhow::bail!("Anna's Archive search failed")
    }

    pub async fn search(&self, http: &reqwest::Client, mirror: &str, key: &str, q: &str) -> anyhow::Result<Vec<SearchResult>> {
        let mirror = mirror.trim_end_matches('/').to_string();
        let html = self.fetch_search_page(http, &mirror, key, q, false).await?;
        let results: Vec<SearchResult> = parse_search(&html, &mirror);
        if results.is_empty() {
            anyhow::bail!("Anna's Archive returned no results — is the secret key right, and the mirror current?");
        }
        Ok(results.into_iter().take(25).collect())
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
    fn parses_cards() {
        let html = r#"<html><body>
            <div class="flex pt-3"><a class="js-vim-focus" href="/md5/0123456789abcdef0123456789abcdef">Clean Code</a>
            <a href="/search"><span class="icon-[mdi--user-edit]"></span>Robert C. Martin</a>
            <div class="font-semibold text-sm leading-[1.2]">English [en] · EPUB · 0.4MB · 2014</div></div>
            </body></html>"#;
        let results = parse_search(html, "https://annas-archive.gl");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Clean Code");
        assert_eq!(results[0].author.as_deref(), Some("Robert C. Martin"));
        assert_eq!(results[0].format.as_deref(), Some("epub"));
        assert_eq!(results[0].size, Some(400_000));
    }
}
