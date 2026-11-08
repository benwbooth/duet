//! Mirror failover, kept fresh by scraping Open SLUM (the Shadow Library
//! Uptime Monitor, open-slum.org) in the background. The compiled-in lists
//! below are the seed set (what SLUM reported last time); a background task
//! refreshes them every 30 minutes so seized domains drop out and new ones
//! join without a code change. Readers use the freshest list, with the
//! user's Settings primary always first.

use std::sync::{LazyLock, RwLock};

use anyhow::Context;
use kuchikiki::traits::TendrilSink;

/// Last-known SLUM check, one list per source. Audiobook Bay isn't on SLUM,
/// so its list stays compiled-in.
#[derive(Clone, Debug, Default)]
pub struct SlumMirrors {
    annas_archive: Vec<String>,
    libgen: Vec<String>,
    updated_at: Option<String>,
}

static CACHE: LazyLock<RwLock<SlumMirrors>> = LazyLock::new(|| RwLock::new(SlumMirrors::default()));

/// Compiled-in seed sets (SLUM's last report). Used until the first scrape.
pub const ANNAS_ARCHIVE: &[&str] = &[
    "https://annas-archive.gl",
    "https://annas-archive.pk",
    "https://annas-archive.gd",
    "https://annas-archive.li",
];

pub const LIBGEN: &[&str] = &[
    "https://libgen.li",
    "https://libgen.bz",
    "https://libgen.vg",
    "https://libgen.la",
    "https://libgen.gl",
    "https://libgen.lc",
    "https://libgen.gs",
    "https://libgen.me",
    "https://libgen.pw",
];

pub const AUDIOBOOK_BAY: &[&str] = &[
    "https://audiobookbay.lu",
    "https://theaudiobookbay.se",
    "https://audiobookbay.fi",
    "https://audiobookbay.is",
];

/// The current freshest list for a source (fresh SLUM scrape if available,
/// else the seed set).
pub fn annas_archive_list() -> Vec<String> {
    let cache = CACHE.read().unwrap();
    if cache.annas_archive.is_empty() { ANNAS_ARCHIVE.iter().map(|s| s.to_string()).collect() } else { cache.annas_archive.clone() }
}

pub fn libgen_list() -> Vec<String> {
    let cache = CACHE.read().unwrap();
    if cache.libgen.is_empty() { LIBGEN.iter().map(|s| s.to_string()).collect() } else { cache.libgen.clone() }
}

/// The configured primary first, then the known set, de-duplicated and
/// trailing-slash normalised.
pub fn ordered(configured: &str, known: &[impl AsRef<str>]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for candidate in [configured.to_string()]
        .into_iter()
        .chain(known.iter().map(|s| s.as_ref().to_string()))
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
    {
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// Scrapes open-slum.org and returns per-source mirrors, or None when the
/// page structure doesn't look like SLUM (so a redesign can't nuke lists).
pub fn parse_slum(html: &str) -> Option<SlumMirrors> {
    let doc = kuchikiki::parse_html().one(html).document_node;
    let mut annas_archive = Vec::new();
    let mut libgen = Vec::new();
    let mut seen_annas = false;
    let mut seen_libgen = false;
    for card in doc.select("div.site-card").into_iter().flatten() {
        let node = card.as_node();
        let title = node
            .select_first("a.site-card-title")
            .ok()
            .map(|e| e.text_contents().trim().to_lowercase())
            .unwrap_or_default();
        let mut domains: Vec<String> = Vec::new();
        for item in node.select("li.domain-item-dense").into_iter().flatten() {
            let item_node = item.as_node();
            let href = item_node
                .select_first("a.domain-link")
                .ok()
                .and_then(|e| e.attributes.borrow().get("href").map(str::to_string))
                .unwrap_or_default();
            let status = item_node
                .select_first("a.status-badge")
                .ok()
                .map(|e| e.text_contents().trim().to_uppercase())
                .unwrap_or_default();
            // Live mirrors only: UP, behind a bot challenge, or degraded.
            // A download endpoint that answers 403 is healthy for our purposes.
            if !href.is_empty() && matches!(status.as_str(), "UP" | "PROTECTED" | "DEGRADED") {
                domains.push(href.trim_end_matches('/').to_string());
            }
        }
        match title.as_str() {
            "annas" | "anna" => {
                // SLUM also lists the GitLab host and the download endpoints;
                // only the web mirrors serve /search.
                annas_archive.extend(domains.into_iter().filter(|d| (d.contains("annas-archive.") && !d.contains("software.")) || d.ends_with("annas-archive.li")));
                seen_annas = true;
            }
            "libgen" | "randombook" => {
                // RandomBook's libgen.* mirrors serve the same database.
                for d in domains {
                    if d.contains("libgen.") || d.contains("libgen") {
                        libgen.push(d);
                    }
                }
                seen_libgen = true;
            }
            _ => {}
        }
    }
    if !seen_annas || !seen_libgen {
        return None;
    }
    annas_archive.sort();
    annas_archive.dedup();
    libgen.sort();
    libgen.dedup();
    Some(SlumMirrors { annas_archive, libgen, updated_at: Some(chrono::Utc::now().to_rfc3339()) })
}

/// One refresh pass. Failures keep the existing lists.
pub async fn refresh(http: &reqwest::Client) -> anyhow::Result<()> {
    let html = http
        .get("https://open-slum.org/")
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .context("reaching Open SLUM")?
        .error_for_status()?
        .text()
        .await?;
    let Some(fresh) = parse_slum(&html) else {
        anyhow::bail!("Open SLUM page structure changed; keeping existing mirror lists")
    };
    tracing::info!(
        "Open SLUM: {} Anna's Archive and {} LibGen mirrors",
        fresh.annas_archive.len(),
        fresh.libgen.len()
    );
    *CACHE.write().unwrap() = fresh;
    Ok(())
}

/// Refreshes immediately, then every 30 minutes forever.
pub async fn refresh_loop(state: crate::State) {
    loop {
        let site = state.site_client().await;
        if let Err(e) = refresh(&site).await {
            tracing::warn!("mirror refresh: {e:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
    }
}

/// Snapshot for the API: the lists duet is actually trying now.
pub fn status() -> serde_json::Value {
    let cache = CACHE.read().unwrap();
    serde_json::json!({
        "updated_at": cache.updated_at,
        "annas_archive": annas_archive_list(),
        "libgen": libgen_list(),
        "audiobookbay": AUDIOBOOK_BAY,
        "slum": "https://open-slum.org/",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puts_the_configured_first_and_dedups() {
        let list = ordered("https://libgen.bz/", LIBGEN);
        assert_eq!(list[0], "https://libgen.bz");
        assert_eq!(list.iter().filter(|m| m.as_str() == "https://libgen.bz").count(), 1);
    }

    #[test]
    fn parses_slum_cards() {
        let html = r#"<div class="site-card">
            <a class="site-card-title">annas</a>
            <ul>
              <li class="domain-item-dense">
                <a class="domain-link" href="https://annas-archive.gl" title="https://annas-archive.gl">annas-archive.gl</a>
                <a class="status-badge compact protected">PROTECTED</a>
              </li>
              <li class="domain-item-dense">
                <a class="domain-link" href="https://annas-archive.pk">annas-archive.pk</a>
                <a class="status-badge compact up">UP</a>
              </li>
              <li class="domain-item-dense">
                <a class="domain-link" href="https://annas-archive.li">annas-archive.li</a>
                <a class="status-badge compact down">DOWN</a>
              </li>
            </ul></div>"#;
        let parsed = parse_slum(html);
        // LibGen card missing: the whole snapshot is rejected.
        assert!(parsed.is_none());
    }

    #[test]
    fn parses_slum_both_cards() {
        let li = |title: &str, domain: &str| format!(
            r#"<div class="site-card"><a class="site-card-title">{title}</a><ul>
              <li class="domain-item-dense"><a class="domain-link" href="https://{domain}">x</a><a class="status-badge">UP</a></li>
            </ul></div>"#
        );
        let html = li("annas", "annas-archive.gl") + &li("libgen", "libgen.li");
        let parsed = parse_slum(&html).expect("both cards present");
        assert_eq!(parsed.annas_archive, vec!["https://annas-archive.gl".to_string()]);
        assert_eq!(parsed.libgen, vec!["https://libgen.li".to_string()]);
        assert!(parsed.updated_at.is_some());
    }
}
