//! A small qBittorrent Web API (v2) client.

use anyhow::{Context, bail};
use duet_common::QbitSettings;
use reqwest::StatusCode;
use serde::Deserialize;
use tokio::sync::Mutex;

pub struct Qbit {
    // Holds the SID cookie, so it gets its own client.
    http: reqwest::Client,
    logged_in_as: Mutex<Option<String>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Torrent {
    pub hash: String,
    pub name: String,
    pub progress: f64,
    pub state: String,
    pub size: i64,
    pub category: String,
    #[serde(default)]
    pub content_path: String,
}

impl Torrent {
    pub fn finished(&self) -> bool {
        self.progress >= 1.0
    }
}

impl Qbit {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Qbit { http: reqwest::Client::builder().cookie_store(true).build()?, logged_in_as: Mutex::new(None) })
    }

    fn url(s: &QbitSettings, path: &str) -> String {
        format!("{}/api/v2/{path}", s.url.trim_end_matches('/'))
    }

    async fn login(&self, s: &QbitSettings) -> anyhow::Result<()> {
        let resp = self
            .http
            .post(Self::url(s, "auth/login"))
            // qBittorrent rejects logins whose Referer/Origin doesn't match its own host.
            .header("Referer", &s.url)
            .form(&[("username", &s.username), ("password", &s.password)])
            .send()
            .await
            .context("connecting to qBittorrent")?;
        let body = resp.text().await.unwrap_or_default();
        if body.trim() != "Ok." {
            bail!("qBittorrent login failed ({})", body.trim());
        }
        *self.logged_in_as.lock().await = Some(format!("{}@{}", s.username, s.url));
        Ok(())
    }

    async fn ensure_login(&self, s: &QbitSettings) -> anyhow::Result<()> {
        if !s.enabled {
            bail!("qBittorrent isn't set up (Settings → Downloads)");
        }
        let who = format!("{}@{}", s.username, s.url);
        if self.logged_in_as.lock().await.as_deref() != Some(who.as_str()) {
            self.login(s).await?;
        }
        Ok(())
    }

    /// Sends a request, logging in again once if the session expired.
    async fn send(&self, s: &QbitSettings, make: impl Fn() -> reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        self.ensure_login(s).await?;
        let resp = make().header("Referer", &s.url).send().await?;
        if resp.status() == StatusCode::FORBIDDEN {
            self.login(s).await?;
            return Ok(make().header("Referer", &s.url).send().await?.error_for_status()?);
        }
        Ok(resp.error_for_status()?)
    }

    pub async fn version(&self, s: &QbitSettings) -> anyhow::Result<String> {
        Ok(self.send(s, || self.http.get(Self::url(s, "app/version"))).await?.text().await?)
    }

    pub async fn add(&self, s: &QbitSettings, url: &str, category: &str) -> anyhow::Result<()> {
        // Creating an existing category fails harmlessly.
        let _ = self.send(s, || self.http.post(Self::url(s, "torrents/createCategory")).form(&[("category", category)])).await;
        let resp = self
            .send(s, || {
                self.http.post(Self::url(s, "torrents/add")).multipart(
                    reqwest::multipart::Form::new().text("urls", url.to_string()).text("category", category.to_string()),
                )
            })
            .await?;
        let body = resp.text().await.unwrap_or_default();
        if body.trim() == "Fails." {
            bail!("qBittorrent refused the torrent");
        }
        Ok(())
    }

    pub async fn list(&self, s: &QbitSettings, category: &str) -> anyhow::Result<Vec<Torrent>> {
        let resp = self.send(s, || self.http.get(Self::url(s, "torrents/info")).query(&[("category", category)])).await?;
        Ok(resp.json().await?)
    }
}
