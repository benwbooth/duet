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
pub struct TorrentFile {
    pub name: String,
    #[serde(default)]
    pub index: i64,
    /// qBittorrent file priority: 0 = skip, 1 = download.
    #[serde(default)]
    pub priority: i64,
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
    /// Unix seconds when qBittorrent added it.
    #[serde(default)]
    pub added_on: i64,
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
        let status = resp.status();
        // qBittorrent 4.x answers 200 "Ok."; newer versions may answer 204 and name the cookie
        // QBT_SID_<port>. A session cookie is the real sign of success.
        let got_cookie = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .any(|c| c.to_str().is_ok_and(|c| c.starts_with("SID=") || c.starts_with("QBT_SID")));
        let body = resp.text().await.unwrap_or_default();
        match status.as_u16() {
            200..=299 if got_cookie || body.trim() == "Ok." => {}
            401 => bail!("qBittorrent rejected the username or password"),
            403 => bail!(
                "qBittorrent refused the login. It bans an address for a while after several failed attempts, \
                 and it rejects requests whose Referer doesn't match its address ({})",
                s.url
            ),
            _ => bail!("qBittorrent login failed (HTTP {status}: {})", body.trim()),
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
        self.add_paused(s, url, category, false).await
    }

    /// Adds a torrent, optionally starting it paused so file selection can
    /// happen before a single byte downloads. Always use paused for big
    /// multi-file torrents (AA shards): nothing should move until exactly
    /// one file is wanted.
    pub async fn add_paused(&self, s: &QbitSettings, url: &str, category: &str, paused: bool) -> anyhow::Result<()> {
        // Creating an existing category fails harmlessly.
        let _ = self.send(s, || self.http.post(Self::url(s, "torrents/createCategory")).form(&[("category", category)])).await;
        let resp = self
            .send(s, || {
                let mut form = reqwest::multipart::Form::new()
                    .text("urls", url.to_string())
                    .text("category", category.to_string());
                if paused {
                    form = form.text("paused", "true");
                }
                self.http.post(Self::url(s, "torrents/add")).multipart(form)
            })
            .await?;
        let body = resp.text().await.unwrap_or_default();
        if body.trim() == "Fails." {
            bail!("qBittorrent refused the torrent");
        }
        Ok(())
    }

    pub async fn resume(&self, s: &QbitSettings, hash: &str) -> anyhow::Result<()> {
        let h = hash.to_string();
        self.send(s, move || self.http.post(Self::url(s, "torrents/resume")).form(&[("hashes", h.clone())])).await?;
        Ok(())
    }

    pub async fn list(&self, s: &QbitSettings, category: &str) -> anyhow::Result<Vec<Torrent>> {
        let resp = self.send(s, || self.http.get(Self::url(s, "torrents/info")).query(&[("category", category)])).await?;
        Ok(resp.json().await?)
    }

    /// Removes a torrent from qBittorrent but keeps its files on disk
    /// (duet-managed downloads are already imported into the library).
    pub async fn remove(&self, s: &QbitSettings, hash: &str) -> anyhow::Result<()> {
        self.send(s, || {
            self.http
                .post(Self::url(s, "torrents/delete"))
                .form(&[("hashes", hash), ("deleteFiles", "false")])
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    /// Exports a torrent's .torrent bytes (useful for magnets once qBittorrent
    /// has fetched their metadata).
    pub async fn export(&self, s: &QbitSettings, hash: &str) -> anyhow::Result<Vec<u8>> {
        let resp = self.send(s, || self.http.get(Self::url(s, "torrents/export")).query(&[("hash", hash)])).await?;
        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn add_and_select(&self, s: &QbitSettings, url: &str, file_name: &str, category: &str) -> anyhow::Result<()> {
        // Paused: with hundred-GB shards, not one byte moves until exactly
        // one file is wanted. Only resume after the selection sticks.
        self.add_paused(s, url, category, true).await?;
        // Wait for the torrent to appear (and its metadata to arrive).
        let mut hash: Option<String> = None;
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            let torrents = self.list(s, category).await?;
            let shard_hint = file_name.split("__").next().unwrap_or("").to_string();
            // Prefer a torrent whose name contains the collection/timestamp hint;
            // fall back to the most recent (newly added) torrent.
            if let Some(t) = torrents.iter().find(|t| t.name.contains(&shard_hint) || t.name.contains("annas_archive_data")) {
                hash = Some(t.hash.clone());
            } else if let Some(t) = torrents.iter().max_by_key(|t| t.added_on) {
                hash = Some(t.hash.clone());
            }
            if hash.is_some() {
                // Check metadata/files are ready.
                let h = hash.as_deref().unwrap();
                if let Ok(files) = self.files(s, h).await {
                    if !files.is_empty() {
                        self.select_file(s, h, file_name).await?;
                        // Verify the selection landed: exactly the wanted file
                        // at priority 1, everything else skipped. Only then
                        // let it run.
                        let after = self.files(s, h).await?;
                        let mut wanted = 0;
                        for f in &after {
                            let base = f.name.rsplit('/').next().unwrap_or(&f.name);
                            if base == file_name || f.name == file_name {
                                wanted += (f.priority == 1) as usize;
                            } else if f.priority != 0 {
                                anyhow::bail!("could not isolate the wanted file inside the shard torrent (left paused, nothing downloaded)");
                            }
                        }
                        if wanted != 1 {
                            anyhow::bail!("file not found inside the shard torrent (left paused, nothing downloaded)");
                        }
                        self.resume(s, h).await?;
                        return Ok(());
                    }
                }
            }
        }
        anyhow::bail!("could not fetch torrent metadata for {file_name} within 2 minutes — the shard may have no seeders right now (nothing was downloaded; the paused torrent is left in place)")
    }

    pub async fn files(&self, s: &QbitSettings, hash: &str) -> anyhow::Result<Vec<TorrentFile>> {
        let h = hash.to_string();
        let resp = self.send(s, move || self.http.get(Self::url(s, "torrents/files")).query(&[("hash", h.clone())])).await?;
        Ok(resp.json().await?)
    }

    pub async fn select_file(&self, s: &QbitSettings, hash: &str, file_name: &str) -> anyhow::Result<()> {
        let files = self.files(s, hash).await?;
        let (target_id, all_ids): (i64, Vec<i64>) = {
            let mut target: Option<i64> = None;
            let mut ids = Vec::new();
            for f in &files {
                ids.push(f.index);
                let base = f.name.rsplit('/').next().unwrap_or(&f.name);
                if base == file_name || f.name == file_name {
                    target = Some(f.index);
                }
            }
            (target.context("file not found inside the shard torrent")?, ids)
        };
        // qBittorrent file priorities: 1 = download, 0 = skip. Set all others
        // to 0 first, then the wanted file to 1 (two calls keep it atomic-ish).
        let others: Vec<String> = all_ids.iter().filter(|&&id| id != target_id).map(|id| id.to_string()).collect();
        let hash_s = hash.to_string();
        if !others.is_empty() {
            let others_joined = others.join("|");
            let hash_c = hash_s.clone();
            let prio = "0".to_string();
            self.send(s, move || self.http.post(Self::url(s, "torrents/filePrio")).form(&[("hash", hash_c.clone()), ("id", others_joined.clone()), ("priority", prio.clone())])).await?;
        }
        let target_s = target_id.to_string();
        let hash_c2 = hash_s.clone();
        let prio1 = "1".to_string();
        self.send(s, move || self.http.post(Self::url(s, "torrents/filePrio")).form(&[("hash", hash_c2.clone()), ("id", target_s.clone()), ("priority", prio1.clone())])).await?;
        Ok(())
    }
}
