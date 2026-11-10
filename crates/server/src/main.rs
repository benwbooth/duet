//! duet: a self-hosted ebook + audiobook library that reads and plays books in sync.

mod activity;
mod ai;
mod ai_setup;
mod annas_archive;
mod audiobookbay;
mod audiotext;
mod book;
mod cast;
mod comic;
mod db;
mod dev;
mod dict;
mod downloads;
mod editions;
mod error;
mod free;
mod illustrate;
mod images;
mod jobs;
mod library;
mod libgen;
mod metadata;
mod mirrors;
mod llm;
mod podcast;
mod qbit;
mod routes;
mod selenium;
mod setup;
mod torznab;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use clap::Parser;
use duet_common::Settings;
use tokio::sync::{Notify, RwLock};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "DUET_LISTEN", default_value = "127.0.0.1:8484")]
    listen: SocketAddr,
    /// Database, caches, sync maps and generated files.
    #[arg(long, env = "DUET_DATA_DIR", default_value = "data")]
    data_dir: PathBuf,
    /// The built web UI (crates/web/dist).
    #[arg(long, env = "DUET_WEB_DIR", default_value = "crates/web/dist")]
    web_dir: PathBuf,
}

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub data_dir: PathBuf,
    pub settings: RwLock<Settings>,
    /// Direct client: qBittorrent, local AI services, anything on LAN.
    pub http: reqwest::Client,
    /// Site client: external site searches/downloads, via Settings →
    /// proxy when set. Cached by proxy URL; rebuilt when it changes.
    pub site: tokio::sync::Mutex<(String, reqwest::Client)>,
    /// Cookie variant of the site client (WordPress sites like Audiobook Bay
    /// need session cookies). Cached alongside.
    pub site_cookies: tokio::sync::Mutex<(String, reqwest::Client)>,
    /// Wakes the job runner when a job is queued.
    pub jobs_wake: Notify,
    pub qbit: qbit::Qbit,
    /// Anna's Archive session cache (search logins).
    pub annas: annas_archive::AnnasArchive,
    pub voices: setup::Voices,
    pub pull: ai_setup::Pull,
    /// How the workers reach this server (for the chat proxy).
    pub self_url: String,
    /// Fingerprint of the served web UI (wasm mtime + size): the client
    /// polls it and offers a reload when a new build lands, so nobody
    /// runs a stale bundle for days in a long-lived tab.
    pub ui_build: String,
}

pub type State = Arc<AppState>;

/// Builds the site client: direct when no proxy is set, otherwise through
/// it — with localhost and LAN always bypassed (Torznab indexers on the LAN
/// must not go out through the VPN).
fn site_client_for(proxy_url: &str, user_agent: &str, cookies: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().user_agent(user_agent);
    if cookies {
        builder = builder.cookie_store(true);
    }
    if !proxy_url.trim().is_empty() {
        if let Ok(proxy) = reqwest::Proxy::all(proxy_url.trim()) {
            let no_proxy = reqwest::NoProxy::from_string("localhost,127.0.0.1,::1,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16");
            builder = builder.proxy(proxy.no_proxy(no_proxy));
        }
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

impl AppState {
    /// The client for external sites (Discover searches, downloads, the AA
    /// catalogue, mirror refreshes): via the configured proxy when set.
    pub async fn site_client(&self) -> reqwest::Client {
        self.site_client_inner(false).await
    }

    /// Cookie-store variant (Audiobook Bay is WordPress and needs it).
    pub async fn site_cookie_client(&self) -> reqwest::Client {
        self.site_client_inner(true).await
    }

    async fn site_client_inner(&self, cookies: bool) -> reqwest::Client {
        let proxy_url = self.settings.read().await.proxy_url.trim().to_string();
        let cache = if cookies { &self.site_cookies } else { &self.site };
        let mut cache = cache.lock().await;
        if cache.0 == proxy_url {
            return cache.1.clone();
        }
        let ua = format!("duet/{} (+https://github.com/benwbooth/duet)", env!("CARGO_PKG_VERSION"));
        let client = site_client_for(&proxy_url, &ua, cookies);
        *cache = (proxy_url, client.clone());
        client
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "duet=info,tower_http=info".into()),
        )
        .init();
    let args = Args::parse();
    std::fs::create_dir_all(&args.data_dir)?;
    let ui_build = std::fs::metadata(args.web_dir.join("duet_web_bg.wasm"))
        .map(|m| format!("{}-{}", m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0), m.len()))
        .unwrap_or_else(|_| "unknown".to_string());
    let db = db::open(&args.data_dir.join("duet.db")).await?;
    let settings = db::load_settings(&db).await?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("duet/", env!("CARGO_PKG_VERSION"), " (+https://github.com/benwbooth/duet)"))
        .build()?;
    let state = Arc::new(AppState {
        db,
        data_dir: args.data_dir.clone(),
        settings: RwLock::new(settings),
        qbit: qbit::Qbit::new()?,
        annas: annas_archive::AnnasArchive::new(),
        http: http.clone(),
        site: tokio::sync::Mutex::new((String::new(), http.clone())),
        site_cookies: tokio::sync::Mutex::new((String::new(), http)),
        jobs_wake: Notify::new(),
        voices: setup::Voices::default(),
        pull: ai_setup::Pull::default(),
        self_url: format!("http://127.0.0.1:{}", args.listen.port()),
        ui_build,
    });

    tokio::spawn(jobs::run(state.clone()));
    tokio::spawn(downloads::watch_torrents(state.clone()));
    tokio::spawn(library::watch_inbox(state.clone()));
    tokio::spawn(setup::autostart(state.clone()));
    // (Years for books added before years were read, without waiting for a library scan.)
    {
        let st = state.clone();
        tokio::spawn(async move {
            library::backfill_years(&st).await;
            // And each book's details looked up online (year first published, description...).
            metadata::backfill(&st).await;
        });
    }
    tokio::spawn(mirrors::refresh_loop(state.clone()));
    tokio::spawn(podcast::daily(state.clone()));

    let app = routes::router(state, &args.web_dir);
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    tracing::info!("listening on http://{}", args.listen);
    axum::serve(listener, app).await?;
    Ok(())
}
