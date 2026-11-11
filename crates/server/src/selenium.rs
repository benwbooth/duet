//! Remote-controlled Firefox (geckodriver + aa-browser sidecars) for
//! bot-walled searches.
//!
//! The driver speaks plain WebDriver REST over HTTP — no extra crates.
//! One session is opened per search and closed after: the *browser* (and
//! its solved cookies) persist in aa-browser regardless, so a dropped
//! session can never wedge future searches.

use anyhow::Context;
use serde_json::{Value, json};

/// What a driven page load found.
pub enum DrivenPage {
    /// The results page HTML.
    Results(String),
    /// A manual check is showing; a human must click through in the viewer.
    NeedHuman,
}

pub struct Selenium {
    /// One browser, one session at a time: concurrent searches queue here
    /// instead of colliding on the driver's single session.
    lock: tokio::sync::Mutex<()>,
}

impl Selenium {
    pub fn new() -> Self {
        Selenium { lock: tokio::sync::Mutex::new(()) }
    }

    /// Loads `url` in the remote browser and waits for result links.
    /// Returns the page HTML, or NeedHuman when the manual check is up.
    /// Serialized: concurrent searches queue instead of colliding.
    /// The remote browser IS the human's visible browser (same profile), so
    /// its session is native — nothing to plant. A human clicks through
    /// manual checks in the viewer; the next attempt finds them solved.
    pub async fn fetch_search(&self, http: &reqwest::Client, wd_url: &str, url: &str) -> anyhow::Result<DrivenPage> {
        let _guard = self.lock.lock().await;
        // Clean mode (user's login excursion) can't be driven: relaunch
        // with Marionette first. Usually a no-op.
        if crate::browser::ensure_driven(url).await? {
            tracing::info!("browser relaunched driven for this search");
        }
        let wd = wd_url.trim_end_matches('/');
        let session = Self::create(http, wd).await?;
        let page = Self::fetch_once(http, wd, &session, url).await;
        // Best-effort cleanup: the browser (and its cookies) survives;
        // only the control channel closes.
        let _ = Self::delete(http, wd, &session).await;
        page
    }
    async fn fetch_once(http: &reqwest::Client, wd: &str, session: &str, url: &str) -> anyhow::Result<DrivenPage> {
        // A fresh navigation: with the solved session the results just load.
        let _: Value = http
            .post(format!("{wd}/session/{session}/url"))
            .json(&json!({"url": url}))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .context("selenium navigate failed (is the sidecar running?)")?
            .error_for_status()
            .context("selenium navigate failed")?
            .json()
            .await?;
        // Poll for either result links or the manual check (up to ~45s).
        // /md5/ links mean a search rendered; a record page has none, so a
        // fully loaded page without check markers counts as done too — the
        // caller decides empty vs. useful.
        for _ in 0..15 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let html = Self::source(http, wd, session).await?;
            if html.contains("/md5/") {
                return Ok(DrivenPage::Results(html));
            }
            if is_check_page(&html) {
                return Ok(DrivenPage::NeedHuman);
            }
        }
        let html = Self::source(http, wd, session).await.unwrap_or_default();
        if is_check_page(&html) {
            return Ok(DrivenPage::NeedHuman);
        }
        if html.len() < 500 {
            anyhow::bail!("Anna's Archive didn't load in time — retry the search");
        }
        Ok(DrivenPage::Results(html))
    }

    async fn open(http: &reqwest::Client, wd: &str) -> anyhow::Result<String> {
        let resp: Value = http
            .post(format!("{wd}/session"))
            .json(&json!({
                "capabilities": {"alwaysMatch": {"browserName": "firefox"}},
            }))
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .context("selenium session failed (is geckodriver up at the WebDriver address?)")?
            .error_for_status()
            .context("selenium session failed")?
            .json()
            .await?;
        let id = resp["value"].get("sessionId").and_then(Value::as_str).context("selenium gave no session")?.to_string();
        std::fs::write(Self::session_file(), &id).ok();
        Ok(id)
    }

    async fn create(http: &reqwest::Client, wd: &str) -> anyhow::Result<String> {
        // Geckodriver allows exactly one session. A previous server life
        // restarted mid-search leaves its session behind holding the slot;
        // the id on disk lets this life clean it up and retry once.
        match Self::open(http, wd).await {
            Ok(id) => return Ok(id),
            Err(first) => {
                if let Some(stale) = Self::recorded_session() {
                    let _ = http.delete(format!("{wd}/session/{stale}")).timeout(std::time::Duration::from_secs(10)).send().await;
                    std::fs::remove_file(Self::session_file()).ok();
                    if let Ok(id) = Self::open(http, wd).await {
                        return Ok(id);
                    }
                }
                // Either an unknown orphan (manual test sessions do this)
                // or the sidecar is down: say exactly that, with the fix.
                anyhow::bail!(
                    "selenium failed ({first:#}). If geckodriver reports \"Session already started\", \
                     an orphaned session is holding its single slot — restart the aa-browser and \
                     geckodriver sidecars, then retry the search"
                );
            }
        }
    }

    /// Where the active session id is recorded, so a restarted server can
    /// clean up a session its previous life left behind.
    fn session_file() -> std::path::PathBuf {
        std::env::temp_dir().join("duet-selenium-session")
    }

    fn recorded_session() -> Option<String> {
        std::fs::read_to_string(Self::session_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    async fn delete(http: &reqwest::Client, wd: &str, session: &str) -> anyhow::Result<()> {
        let r = http
            .delete(format!("{wd}/session/{session}"))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
        // The browser (and its cookies) survive; drop the recorded id too.
        std::fs::remove_file(Self::session_file()).ok();
        r?.error_for_status()?;
        Ok(())
    }

    async fn source(http: &reqwest::Client, wd: &str, session: &str) -> anyhow::Result<String> {
        let resp: Value = http
            .get(format!("{wd}/session/{session}/source"))
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .context("selenium read failed")?
            .error_for_status()
            .context("selenium read failed")?
            .json()
            .await?;
        Ok(resp["value"].as_str().unwrap_or_default().to_string())
    }
}

fn is_check_page(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("ddos-guard") || lower.contains("checking your browser") || lower.contains("just a moment") || lower.contains("ddg-captcha")
}
