//! Guided chat-model setup: finds local model servers, downloads models into Ollama with
//! progress, and runs a test question so the user knows it works.

use std::{sync::Mutex, time::Duration};

use anyhow::{Context, bail};
use duet_common::{ChatSetup, ChatTest, DetectedServer, LocalModel, PullStatus};
use futures_util::StreamExt;
use serde_json::json;

use crate::{State, setup};

/// Where the common local model servers listen by default.
const KNOWN: [(&str, &str, &str); 4] = [
    ("Ollama", "http://127.0.0.1:11434/v1", "ollama"),
    ("LM Studio", "http://127.0.0.1:1234/v1", "openai"),
    ("llama.cpp server", "http://127.0.0.1:8081/v1", "openai"),
    ("vLLM", "http://127.0.0.1:8000/v1", "openai"),
];

#[derive(Default)]
pub struct Pull(Mutex<Option<PullStatus>>);

async fn probe(http: &reqwest::Client, name: &str, url: &str, kind: &str) -> DetectedServer {
    let mut server = DetectedServer { name: name.into(), url: url.into(), kind: kind.into(), running: false, models: Vec::new() };
    let timeout = Duration::from_millis(1500);
    if kind == "ollama" {
        // Ollama's own API also reports sizes.
        let base = url.trim_end_matches("/v1");
        if let Ok(r) = http.get(format!("{base}/api/tags")).timeout(timeout).send().await {
            if let Ok(v) = r.json::<serde_json::Value>().await {
                server.running = true;
                for m in v["models"].as_array().into_iter().flatten() {
                    server.models.push(LocalModel {
                        name: m["name"].as_str().unwrap_or_default().into(),
                        size_gb: m["size"].as_f64().map(|b| (b / 1e8).round() / 10.0),
                        parameters: m["details"]["parameter_size"].as_str().map(str::to_string),
                    });
                }
            }
        }
    } else if let Ok(r) = http.get(format!("{url}/models")).timeout(timeout).send().await {
        if let Ok(v) = r.json::<serde_json::Value>().await {
            server.running = true;
            for m in v["data"].as_array().into_iter().flatten() {
                if let Some(id) = m["id"].as_str() {
                    server.models.push(LocalModel { name: id.into(), size_gb: None, parameters: None });
                }
            }
        }
    }
    server.models.sort_by(|a, b| a.name.cmp(&b.name));
    server
}

pub async fn detect(state: &State) -> ChatSetup {
    let configured = state.settings.read().await.ai.llm_url.clone();
    let mut probes: Vec<(String, String, String)> =
        KNOWN.iter().map(|(n, u, k)| (n.to_string(), u.to_string(), k.to_string())).collect();
    let same = |a: &str, b: &str| a.trim_end_matches('/').replace("localhost", "127.0.0.1") == b.trim_end_matches('/').replace("localhost", "127.0.0.1");
    if !configured.is_empty() && !probes.iter().any(|p| same(&p.1, &configured)) {
        probes.push(("Your server".into(), configured, "openai".into()));
    }
    let servers = futures_util::future::join_all(probes.iter().map(|(n, u, k)| probe(&state.http, n, u, k))).await;
    let gpu = setup::gpu();
    ChatSetup {
        nixos: std::path::Path::new("/etc/NIXOS").exists(),
        claude_cli: on_path("claude"),
        codex_cli: on_path("codex"),
        gpu: gpu.as_ref().map(|g| g.0.clone()),
        gpu_vram_gb: gpu.map(|g| g.1),
        servers,
        pull: state.pull.0.lock().unwrap().clone(),
    }
}

/// Downloads a model into Ollama in the background; progress shows up in `detect`.
pub async fn pull(state: &State, url: &str, model: &str) -> anyhow::Result<()> {
    {
        let mut p = state.pull.0.lock().unwrap();
        if p.as_ref().is_some_and(|p| !p.done) {
            bail!("already downloading {}", p.as_ref().unwrap().model);
        }
        *p = Some(PullStatus { model: model.into(), status: "Starting".into(), progress: 0.0, done: false, error: None });
    }
    let base = url.trim_end_matches('/').trim_end_matches("/v1").to_string();
    let resp = state
        .http
        .post(format!("{base}/api/pull"))
        .json(&json!({ "model": model, "stream": true }))
        .send()
        .await
        .context("Ollama isn't answering")?
        .error_for_status()?;
    let state = state.clone();
    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut buf = Vec::new();
        let mut error = None;
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                error = Some("download interrupted".to_string());
                break;
            };
            buf.extend_from_slice(&chunk);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else { continue };
                if let Some(e) = v["error"].as_str() {
                    error = Some(e.to_string());
                }
                let mut p = state.pull.0.lock().unwrap();
                if let Some(p) = p.as_mut() {
                    if let Some(s) = v["status"].as_str() {
                        p.status = s.to_string();
                    }
                    if let (Some(done), Some(total)) = (v["completed"].as_f64(), v["total"].as_f64()) {
                        if total > 0.0 {
                            p.progress = done / total;
                        }
                    }
                }
            }
        }
        if let Some(p) = state.pull.0.lock().unwrap().as_mut() {
            p.done = true;
            p.error = error.clone();
            if error.is_none() {
                p.progress = 1.0;
                p.status = "Downloaded".into();
            }
        }
    });
    Ok(())
}

/// Asks the configured model one short question and times it.
pub async fn test(state: &State, question: &str) -> anyhow::Result<ChatTest> {
    let started = std::time::Instant::now();
    let answer = crate::llm::chat(state, "Answer in one or two short sentences.", &[("user".into(), question.to_string())]).await?;
    let seconds = started.elapsed().as_secs_f64();
    // Roughly four characters to a token.
    Ok(ChatTest { tokens_per_second: Some(answer.len() as f64 / 4.0 / seconds.max(0.01)), answer, seconds })
}

fn on_path(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}
