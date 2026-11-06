//! duet: a self-hosted ebook + audiobook library that reads and plays books in sync.

mod ai;
mod ai_setup;
mod audiotext;
mod book;
mod cast;
mod db;
mod dev;
mod dict;
mod downloads;
mod error;
mod free;
mod images;
mod jobs;
mod library;
mod llm;
mod qbit;
mod routes;
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
    pub http: reqwest::Client,
    /// Wakes the job runner when a job is queued.
    pub jobs_wake: Notify,
    pub qbit: qbit::Qbit,
    pub voices: setup::Voices,
    pub pull: ai_setup::Pull,
    /// How the workers reach this server (for the chat proxy).
    pub self_url: String,
}

pub type State = Arc<AppState>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "duet=info,tower_http=info".into()),
        )
        .init();
    let args = Args::parse();
    std::fs::create_dir_all(&args.data_dir)?;
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
        http,
        jobs_wake: Notify::new(),
        voices: setup::Voices::default(),
        pull: ai_setup::Pull::default(),
        self_url: format!("http://127.0.0.1:{}", args.listen.port()),
    });

    tokio::spawn(jobs::run(state.clone()));
    tokio::spawn(downloads::watch_torrents(state.clone()));
    tokio::spawn(setup::autostart(state.clone()));

    let app = routes::router(state, &args.web_dir);
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    tracing::info!("listening on http://{}", args.listen);
    axum::serve(listener, app).await?;
    Ok(())
}
