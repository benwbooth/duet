//! Reading sites that need you logged in (X, Instagram) through the remote Firefox, driven over
//! WebDriver (geckodriver attached to the browser you can see in duet). You log in there once;
//! each briefing then opens a tab of its own, reads your timeline off the page, and closes it,
//! leaving whatever else is open alone.

use std::time::Duration;

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::feeds::{Item, clip};
use crate::State;

/// geckodriver drives one session at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Where a site's login page is, and where its feed is.
pub fn login_url(site: &str) -> Option<&'static str> {
    match site {
        "x" => Some("https://x.com/i/flow/login"),
        "instagram" => Some("https://www.instagram.com/accounts/login/"),
        "reddit" => Some("https://www.reddit.com/login/"),
        "facebook" => Some("https://www.facebook.com/login/"),
        _ => None,
    }
}

struct Session {
    http: reqwest::Client,
    base: String,
    id: String,
}

impl Session {
    async fn start(http: &reqwest::Client, webdriver: &str) -> anyhow::Result<Session> {
        // Clean mode (a manual login excursion) can't be driven: relaunch
        // with Marionette first. Usually a no-op.
        if crate::browser::ensure_driven("about:blank").await? {
            tracing::info!("browser relaunched driven for podcast reading");
        }
        let base = webdriver.trim_end_matches('/').to_string();
        anyhow::ensure!(!base.is_empty(), "Set the browser's WebDriver address (Briefing → Sources)");
        let mut last = String::new();
        // (Another search may be using the browser for a moment.)
        for _ in 0..10 {
            let resp = http
                .post(format!("{base}/session"))
                .json(&json!({ "capabilities": { "alwaysMatch": {} } }))
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .context("the remote browser's WebDriver isn't answering")?;
            let v: Value = resp.json().await.unwrap_or_default();
            if let Some(id) = v["value"]["sessionId"].as_str() {
                return Ok(Session { http: http.clone(), base, id: id.to_string() });
            }
            last = v["value"]["message"].as_str().unwrap_or("no session").to_string();
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        bail!("couldn't get the remote browser: {last}")
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let mut req = self.http.request(method, format!("{}/session/{}{path}", self.base, self.id)).timeout(Duration::from_secs(60));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let v: Value = req.send().await?.json().await?;
        if let Some(err) = v["value"]["error"].as_str() {
            bail!("{err}: {}", v["value"]["message"].as_str().unwrap_or(""));
        }
        Ok(v["value"].clone())
    }

    /// A new tab, made current; returns its handle.
    async fn new_tab(&self) -> anyhow::Result<String> {
        let v = self.call(reqwest::Method::POST, "/window/new", Some(json!({ "type": "tab" }))).await?;
        let handle = v["handle"].as_str().context("no new tab")?.to_string();
        self.call(reqwest::Method::POST, "/window", Some(json!({ "handle": handle }))).await?;
        Ok(handle)
    }

    async fn go(&self, url: &str) -> anyhow::Result<()> {
        self.call(reqwest::Method::POST, "/url", Some(json!({ "url": url }))).await.map(|_| ())
    }

    async fn url(&self) -> anyhow::Result<String> {
        Ok(self.call(reqwest::Method::GET, "/url", None).await?.as_str().unwrap_or("").to_string())
    }

    async fn run(&self, script: &str) -> anyhow::Result<Value> {
        self.call(reqwest::Method::POST, "/execute/sync", Some(json!({ "script": script, "args": [] }))).await
    }

    async fn close_tab(&self) {
        let _ = self.call(reqwest::Method::DELETE, "/window", None).await;
    }

    /// Lets go of the browser (it keeps running: geckodriver only attached to it).
    async fn end(self) {
        let _ = self.http.delete(format!("{}/session/{}", self.base, self.id)).timeout(Duration::from_secs(10)).send().await;
    }
}

/// Opens the site's login page by restarting the browser clean AT that
/// page: no WebDriver involved, so it works from clean mode, and the login
/// flow never sees an automation flag. Duet flips it back to driven the
/// next time it needs to drive anything.
pub async fn open_login(_state: &State, _webdriver: &str, site: &str) -> anyhow::Result<()> {
    let url = login_url(site).context("no login page for that site")?;
    let _one = ONE_AT_A_TIME.lock().await;
    crate::browser::restart(false, url).await?;
    Ok(())
}

/// Whether you're logged in to the site in the remote browser.
pub async fn logged_in(state: &State, webdriver: &str, site: &str) -> anyhow::Result<bool> {
    let _one = ONE_AT_A_TIME.lock().await;
    let s = Session::start(&state.http, webdriver).await?;
    let r = async {
        s.new_tab().await?;
        let ok = match site {
            "x" => {
                s.go("https://x.com/home").await?;
                tokio::time::sleep(Duration::from_secs(4)).await;
                let u = s.url().await?;
                !u.contains("/login") && !u.contains("/i/flow") && u.contains("/home")
            }
            "instagram" => {
                s.go("https://www.instagram.com/").await?;
                tokio::time::sleep(Duration::from_secs(4)).await;
                let v = s.run("return !!document.querySelector('svg[aria-label=\"Home\"], a[href=\"/direct/inbox/\"]') && !location.pathname.startsWith('/accounts/login')").await?;
                v.as_bool().unwrap_or(false)
            }
            "reddit" => {
                s.go("https://www.reddit.com/").await?;
                tokio::time::sleep(Duration::from_secs(3)).await;
                let v = s.run("return !document.querySelector('a[href*=\"/login\"]')").await?;
                v.as_bool().unwrap_or(false)
            }
            _ => false,
        };
        s.close_tab().await;
        Ok(ok)
    }
    .await;
    s.end().await;
    r
}

/// Posts on X: your home timeline ("home"), a list ("list/<id>") or an account.
pub async fn x(state: &State, webdriver: &str, which: &str) -> anyhow::Result<Vec<Item>> {
    let (url, source) = match which.trim() {
        "" | "home" => ("https://x.com/home".to_string(), "X (your timeline)".to_string()),
        l if l.starts_with("list/") => (format!("https://x.com/i/lists/{}", &l[5..]), "X (your list)".to_string()),
        h => (format!("https://x.com/{}", h.trim_start_matches('@')), format!("X · @{}", h.trim_start_matches('@'))),
    };
    // Each post: its text, who, when, its link, likes/reposts, a picture, and whether it's a video.
    let script = r#"
        const out = [];
        for (const a of document.querySelectorAll('article[data-testid="tweet"]')) {
          const text = [...a.querySelectorAll('div[data-testid="tweetText"]')].map(t => t.innerText).join('\n\n');
          const time = a.querySelector('time');
          const link = time && time.closest('a') ? time.closest('a').href : null;
          const user = a.querySelector('div[data-testid="User-Name"]');
          const stats = (a.querySelector('div[role="group"]') || {}).getAttribute ? a.querySelector('div[role="group"]').getAttribute('aria-label') : '';
          const card = a.querySelector('div[data-testid="card.wrapper"] a');
          const img = a.querySelector('div[data-testid="tweetPhoto"] img');
          out.push({ text, link, when: time ? time.getAttribute('datetime') : null,
            who: user ? user.innerText.split('\n').slice(0, 2).join(' ') : '',
            stats: stats || '', card: card ? card.href : null, img: img ? img.src : null,
            video: !!a.querySelector('div[data-testid="videoPlayer"], video') });
        }
        return out;"#;
    let posts = scroll_and_read(state, webdriver, &url, script, 4).await?;
    let mut out = Vec::new();
    for p in posts {
        let link = p["link"].as_str().unwrap_or("").to_string();
        let text = p["text"].as_str().unwrap_or("").to_string();
        if link.is_empty() || text.is_empty() {
            continue;
        }
        let stats = p["stats"].as_str().unwrap_or("");
        let likes = number_before(stats, "like");
        let reposts = number_before(stats, "repost");
        let who = p["who"].as_str().unwrap_or("").to_string();
        // A post sharing an article is about the article.
        let url = p["card"].as_str().filter(|c| !c.contains("x.com/")).map(str::to_string).unwrap_or_else(|| link.clone());
        out.push(Item {
            source: source.clone(),
            title: clip(&format!("{who}: {text}"), 160),
            discussion: (url != link).then(|| link.clone()),
            url,
            text: clip(&format!("{who} posted: {text}"), 1500),
            score: likes.map(|l| l + reposts.unwrap_or(0) * 3),
            comments: number_before(stats, "repl"),
            published: p["when"].as_str().and_then(|w| DateTime::parse_from_rfc3339(w).ok()).map(|d| d.with_timezone(&Utc)),
            image: p["img"].as_str().map(str::to_string),
            video: p["video"].as_bool().unwrap_or(false),
        });
    }
    if out.is_empty() {
        bail!("nothing on the page — log in to X in the remote browser (Briefing → Sources)");
    }
    Ok(out)
}

/// Posts on Instagram: your feed ("home") or an account.
pub async fn instagram(state: &State, webdriver: &str, which: &str) -> anyhow::Result<Vec<Item>> {
    let (url, source) = match which.trim() {
        "" | "home" => ("https://www.instagram.com/".to_string(), "Instagram (your feed)".to_string()),
        h => (format!("https://www.instagram.com/{}/", h.trim_start_matches('@')), format!("Instagram · @{}", h.trim_start_matches('@'))),
    };
    let script = r#"
        const out = [];
        const seen = new Set();
        for (const a of document.querySelectorAll('article, main a[href*="/p/"], main a[href*="/reel/"]')) {
          const art = a.tagName === 'ARTICLE' ? a : null;
          const linkEl = art ? art.querySelector('a[href*="/p/"], a[href*="/reel/"]') : a;
          if (!linkEl) continue;
          const link = linkEl.href.split('?')[0];
          if (seen.has(link)) continue;
          seen.add(link);
          const scope = art || a;
          const time = scope.querySelector('time');
          const img = scope.querySelector('img');
          const who = art ? ((art.querySelector('header a[href^="/"]') || {}).innerText || '') : '';
          const caption = art ? [...art.querySelectorAll('h1, span[dir="auto"]')].map(s => s.innerText).sort((x, y) => y.length - x.length)[0] || '' : '';
          out.push({ link, who, caption: caption || (img ? img.alt : ''), when: time ? time.getAttribute('datetime') : null,
            img: img ? img.src : null, video: link.includes('/reel/') || !!scope.querySelector('video') });
        }
        return out;"#;
    let posts = scroll_and_read(state, webdriver, &url, script, 3).await?;
    let mut out = Vec::new();
    for p in posts {
        let link = p["link"].as_str().unwrap_or("").to_string();
        let caption = p["caption"].as_str().unwrap_or("").to_string();
        if link.is_empty() {
            continue;
        }
        let who = p["who"].as_str().unwrap_or("").to_string();
        let who_src = if who.is_empty() { source.clone() } else { format!("Instagram · @{who}") };
        out.push(Item {
            source: who_src,
            title: clip(if caption.is_empty() { "A post" } else { &caption }, 140),
            url: link,
            discussion: None,
            text: clip(&caption, 1500),
            score: None,
            comments: None,
            published: p["when"].as_str().and_then(|w| DateTime::parse_from_rfc3339(w).ok()).map(|d| d.with_timezone(&Utc)),
            image: p["img"].as_str().map(str::to_string),
            video: p["video"].as_bool().unwrap_or(false),
        });
    }
    if out.is_empty() {
        bail!("nothing on the page — log in to Instagram in the remote browser (Briefing → Sources)");
    }
    Ok(out)
}

/// Opens `url` in a tab of its own, scrolls `pages` screens (the feed loads as you go), reading
/// as it scrolls, and closes the tab.
async fn scroll_and_read(state: &State, webdriver: &str, url: &str, script: &str, pages: usize) -> anyhow::Result<Vec<Value>> {
    let _one = ONE_AT_A_TIME.lock().await;
    let s = Session::start(&state.http, webdriver).await?;
    let r = async {
        s.new_tab().await?;
        s.go(url).await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        let here = s.url().await.unwrap_or_default();
        if here.contains("/login") || here.contains("/i/flow") || here.contains("/accounts/") {
            s.close_tab().await;
            bail!("not logged in — log in through the remote browser (Briefing → Sources)");
        }
        let mut all: Vec<Value> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..=pages {
            if let Ok(Value::Array(found)) = s.run(script).await {
                for f in found {
                    let key = f["link"].as_str().unwrap_or("").to_string();
                    if !key.is_empty() && seen.insert(key) {
                        all.push(f);
                    }
                }
            }
            let _ = s.run("window.scrollBy(0, window.innerHeight * 1.8); return true;").await;
            tokio::time::sleep(Duration::from_millis(2500)).await;
        }
        s.close_tab().await;
        Ok(all)
    }
    .await;
    s.end().await;
    r
}

/// "1,234 likes" → 1234 (and "3.4K" → 3400).
fn number_before(text: &str, word: &str) -> Option<i64> {
    let lower = text.to_lowercase();
    let at = lower.find(word)?;
    let num = lower[..at].split([',', ' ']).filter(|t| !t.is_empty()).next_back()?;
    let num = num.trim();
    let (n, mult) = match num.chars().last()? {
        'k' => (&num[..num.len() - 1], 1000.0),
        'm' => (&num[..num.len() - 1], 1_000_000.0),
        _ => (num, 1.0),
    };
    n.replace(',', "").parse::<f64>().ok().map(|v| (v * mult) as i64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts() {
        assert_eq!(super::number_before("12 replies, 340 reposts, 5600 likes, 1 bookmark", "like"), Some(5600));
        assert_eq!(super::number_before("12 replies, 340 reposts", "repost"), Some(340));
        assert_eq!(super::number_before("12 replies", "repl"), Some(12));
    }
}
