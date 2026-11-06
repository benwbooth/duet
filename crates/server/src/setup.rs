//! Installing and running the local voice stack from the web UI (Settings → Voices):
//! scripts/setup-voices.sh installs fish-speech + Fish Audio S2-Pro and the workers' venv with
//! Qwen3-TTS, and scripts/voices.sh runs the TTS server that duet talks to.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
};

use anyhow::{Context, bail};
use duet_common::VoiceSetup;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::{State, db};

const LOG_LINES: usize = 60;
const VOICE_PORT: u16 = 8880;

#[derive(Default)]
struct Install {
    running: bool,
    step: String,
    progress: f64,
    error: Option<String>,
    log: VecDeque<String>,
}

#[derive(Default)]
pub struct Voices {
    install: Mutex<Install>,
    server: tokio::sync::Mutex<Option<tokio::process::Child>>,
}

fn scripts_dir() -> PathBuf {
    std::env::var_os("DUET_SCRIPTS_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("scripts"))
}

fn fish_dir(state: &State) -> PathBuf {
    state.data_dir.join("fish-speech")
}

fn venv_dir(state: &State) -> PathBuf {
    state.data_dir.join("venv")
}

fn hf_cached(repo: &str) -> bool {
    let hub = std::env::var_os("HF_HUB_CACHE").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("HF_HOME")
            .map(|h| PathBuf::from(h).join("hub"))
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/huggingface/hub"))
    });
    hub.join(format!("models--{}", repo.replace('/', "--"))).join("snapshots").read_dir().is_ok_and(|mut d| d.next().is_some())
}

/// The biggest GPU, from sysfs: (description, VRAM in GB).
pub fn gpu() -> Option<(String, u64)> {
    let mut best: Option<(String, u64)> = None;
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let dev = entry.path().join("device");
        let vendor = std::fs::read_to_string(dev.join("vendor")).unwrap_or_default();
        let vram = std::fs::read_to_string(dev.join("mem_info_vram_total"))
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|b| (b + (1 << 29)) >> 30)
            .unwrap_or(0);
        let name = match vendor.trim() {
            "0x1002" => "AMD GPU",
            "0x10de" => "NVIDIA GPU",
            "0x8086" => "Intel GPU",
            _ => continue,
        };
        if vram > best.as_ref().map(|b| b.1).unwrap_or(0) {
            best = Some((format!("{name}, {vram} GB"), vram));
        }
    }
    best
}

pub async fn status(state: &State) -> VoiceSetup {
    let fish = fish_dir(state);
    let venv = venv_dir(state);
    let ours = {
        let mut s = state.voices.server.lock().await;
        match s.as_mut() {
            Some(child) => child.try_wait().ok().flatten().is_none(),
            None => false,
        }
    };
    let server_running = ours || alive(state).await;
    let gpu = gpu();
    let i = state.voices.install.lock().unwrap();
    VoiceSetup {
        gpu: gpu.as_ref().map(|g| g.0.clone()),
        gpu_vram_gb: gpu.map(|g| g.1),
        fish_installed: fish.join(".venv/bin/python").exists() && fish.join("tools/api_server.py").exists(),
        fish_weights: fish.join("checkpoints/s2-pro/codec.pth").exists(),
        workers_installed: venv.join("bin/python").exists(),
        casting_weights: hf_cached("Qwen/Qwen3-TTS-12Hz-1.7B-VoiceDesign") && hf_cached("Qwen/Qwen3-TTS-12Hz-1.7B-Base"),
        installing: i.running,
        step: i.step.clone(),
        progress: i.progress,
        error: i.error.clone(),
        log: i.log.iter().cloned().collect(),
        server_running,
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

fn record(state: &State, line: &str) {
    let line = strip_ansi(line);
    let mut i = state.voices.install.lock().unwrap();
    // "== [3/6] Fish Audio S2-Pro weights"
    if let Some(rest) = line.strip_prefix("== [") {
        if let Some((frac, title)) = rest.split_once("] ") {
            if let Some((n, total)) = frac.split_once('/') {
                if let (Ok(n), Ok(total)) = (n.parse::<f64>(), total.parse::<f64>()) {
                    i.progress = (n - 1.0) / total;
                }
            }
            i.step = title.to_string();
        }
    }
    if !line.trim().is_empty() {
        i.log.push_back(line);
        while i.log.len() > LOG_LINES {
            i.log.pop_front();
        }
    }
}

async fn pump(state: State, stream: impl AsyncRead + Unpin) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        // Download progress bars rewrite one line with \r; keep the latest part.
        let latest = l.rsplit('\r').find(|p| !p.trim().is_empty()).unwrap_or(&l).to_string();
        record(&state, &latest);
    }
}

pub async fn install(state: &State) -> anyhow::Result<()> {
    {
        let mut i = state.voices.install.lock().unwrap();
        if i.running {
            bail!("already installing");
        }
        *i = Install { running: true, step: "Starting".into(), ..Default::default() };
    }
    // fish-speech and VoiceDesign can't share the GPU with a running voice server.
    stop_server(state).await;
    let script = std::path::absolute(scripts_dir().join("setup-voices.sh"))?;
    let mut child = tokio::process::Command::new("bash")
        .arg(&script)
        .arg(fish_dir(state))
        .arg(venv_dir(state))
        .env("DUET_DATA_DIR", &state.data_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("running {}", script.display()))?;
    let (out, err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    tokio::spawn(pump(state.clone(), out));
    tokio::spawn(pump(state.clone(), err));
    let state = state.clone();
    tokio::spawn(async move {
        let result = child.wait().await;
        let ok = matches!(&result, Ok(s) if s.success());
        {
            let mut i = state.voices.install.lock().unwrap();
            i.running = false;
            if ok {
                i.progress = 1.0;
                i.step = "Installed".into();
            } else {
                let last = i.log.back().cloned().unwrap_or_default();
                i.error = Some(format!("Setup failed: {last}"));
            }
        }
        if ok {
            if let Err(e) = use_installed(&state).await {
                record(&state, &format!("{e:#}"));
            }
        }
    });
    Ok(())
}

/// Points the AI settings at the installed stack and starts the voice server.
async fn use_installed(state: &State) -> anyhow::Result<()> {
    let workers = std::path::absolute(PathBuf::from("workers"))?;
    let settings = {
        let mut s = state.settings.write().await;
        s.ai.python = venv_dir(state).join("bin/python").to_string_lossy().into();
        if workers.join("align.py").exists() {
            s.ai.workers_dir = workers.to_string_lossy().into();
        }
        s.ai.tts_url = format!("http://127.0.0.1:{VOICE_PORT}/v1");
        s.clone()
    };
    db::save_settings(&state.db, &settings).await?;
    if settings.ai.auto_start_voices {
        start_server(state).await?;
    }
    Ok(())
}

/// Whether a voice server answers on duet's port (ours, or one left from an earlier run).
async fn alive(state: &State) -> bool {
    state
        .http
        .post(format!("http://127.0.0.1:{VOICE_PORT}/v1/voices"))
        .json(&serde_json::json!({ "voices": [] }))
        .timeout(std::time::Duration::from_millis(1500))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

pub async fn start_server(state: &State) -> anyhow::Result<()> {
    let mut server = state.voices.server.lock().await;
    if let Some(child) = server.as_mut() {
        if child.try_wait()?.is_none() {
            return Ok(());
        }
    }
    // One is already running (e.g. started before duet restarted): use it.
    if alive(state).await {
        return Ok(());
    }
    let script = std::path::absolute(scripts_dir().join("voices.sh"))?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(state.data_dir.join("voices.log"))?;
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg(&script)
        .arg(fish_dir(state))
        .arg(venv_dir(state))
        .arg(state.data_dir.join("voices"))
        .arg(VOICE_PORT.to_string())
        .arg(state.settings.read().await.ai.voice_idle_minutes.max(0.0).to_string())
        .env("DUET_DATA_DIR", &state.data_dir)
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true);
    // If duet itself is killed, take the voice server down with it rather than leaving it
    // holding the port.
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let child = cmd.spawn().with_context(|| format!("running {}", script.display()))?;
    *server = Some(child);
    Ok(())
}

pub async fn stop_server(state: &State) {
    if let Some(mut child) = state.voices.server.lock().await.take() {
        let _ = child.kill().await;
    }
    // A server duet didn't start (left from an earlier run) is asked to shut itself down.
    if alive(state).await {
        let _ = state
            .http
            .post(format!("http://127.0.0.1:{VOICE_PORT}/v1/shutdown"))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
    }
}

/// Starts the voice server with duet when it's installed and wanted.
pub async fn autostart(state: State) {
    let wanted = state.settings.read().await.ai.auto_start_voices;
    if wanted && status(&state).await.ready() {
        if let Err(e) = start_server(&state).await {
            tracing::warn!("voice server: {e:#}");
        }
    }
}

pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("voices.log")
}
