//! The chat model, wherever it runs: a local OpenAI-compatible server (Ollama, LM Studio,
//! llama.cpp, vLLM), Claude or OpenAI with an API key, or the Claude Code / Codex command-line
//! tools signed in to your own subscription.

use anyhow::{Context, bail};
use duet_common::AiSettings;
use serde_json::json;

use crate::State;

pub const ANTHROPIC_VERSION: &str = "2026-10-21";
pub const OPENAI_URL: &str = "https://api.openai.com/v1";

/// One turn of a conversation: ("user" | "assistant", text).
pub type Turn = (String, String);

pub async fn chat(state: &State, system: &str, turns: &[Turn]) -> anyhow::Result<String> {
    let ai = state.settings.read().await.ai.clone();
    let text = match ai.llm_provider.as_str() {
        "anthropic" => anthropic(state, &ai, system, turns).await?,
        "openai" => {
            let model = if ai.openai_model.is_empty() { bail!("Choose an OpenAI model (Settings → AI)") } else { &ai.openai_model };
            openai_compatible(state, OPENAI_URL, &ai.openai_api_key, model, system, turns).await?
        }
        "service" => {
            if ai.service_model.is_empty() {
                bail!("Choose a model for the service (Settings → AI)");
            }
            openai_compatible(state, &ai.service_url, &ai.service_key, &ai.service_model, system, turns).await?
        }
        "claude_cli" => claude_cli(&ai, system, turns).await?,
        "codex_cli" => codex_cli(&ai, system, turns).await?,
        "gemini_cli" => gemini_cli(&ai, system, turns).await?,
        "opencode_cli" => opencode_cli(&ai, system, turns).await?,
        _ => {
            if ai.llm_url.is_empty() {
                bail!("No chat model set up (Settings → AI)");
            }
            openai_compatible(state, &ai.llm_url, &ai.llm_api_key, &ai.llm_model, system, turns).await?
        }
    };
    // Reasoning models may include their thinking inline.
    Ok(match text.find("</think>") {
        Some(end) => text[end + "</think>".len()..].trim().to_string(),
        None => text.trim().to_string(),
    })
}

async fn openai_compatible(
    state: &State,
    url: &str,
    key: &str,
    model: &str,
    system: &str,
    turns: &[Turn],
) -> anyhow::Result<String> {
    let mut messages = vec![json!({ "role": "system", "content": system })];
    messages.extend(turns.iter().map(|(role, content)| json!({ "role": role, "content": content })));
    let mut body = json!({ "model": model, "messages": messages });
    // Ollama's reasoning models think at length first (20 s+ for a definition); answers here are
    // short and conversational, so skip it.
    if url.contains(":11434") {
        body["reasoning_effort"] = json!("none");
    }
    let resp: serde_json::Value = state
        .http
        .post(format!("{}/chat/completions", url.trim_end_matches('/')))
        .bearer_auth(key)
        .timeout(std::time::Duration::from_secs(300))
        .json(&body)
        .send()
        .await
        .context("the chat model isn't answering")?
        .error_for_status()
        .context("the chat model refused the request (check the model name and key)")?
        .json()
        .await?;
    Ok(resp["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
}

/// Claude through the Messages API (raw HTTP: there's no official Rust SDK).
async fn anthropic(state: &State, ai: &AiSettings, system: &str, turns: &[Turn]) -> anyhow::Result<String> {
    if ai.anthropic_api_key.is_empty() {
        bail!("Add your Anthropic API key (Settings → AI)");
    }
    let model = if ai.anthropic_model.is_empty() { "claude-opus-5-5" } else { &ai.anthropic_model };
    let messages: Vec<_> = turns.iter().map(|(role, content)| json!({ "role": role, "content": content })).collect();
    let mut body = json!({
        "model": model,
        "max_tokens": 16000,
        "system": system,
        "messages": messages,
        "thinking": { "type": "adaptive" },
        // Conversation and casting don't need deep thinking; this keeps answers quick.
        "output_config": { "effort": "low" },
    });
    let mut req = state
        .http
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &ai.anthropic_api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .timeout(std::time::Duration::from_secs(600));
    // If a safety classifier declines, let the server retry on a suitable model instead.
    if ["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5", "claude-sonnet-5-5"].contains(&model) {
        body["fallbacks"] = json!("default");
        req = req.header("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    let resp = req.json(&body).send().await.context("can't reach the Anthropic API")?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.context("unexpected reply from the Anthropic API")?;
    if !status.is_success() {
        bail!("Anthropic API: {}", v["error"]["message"].as_str().unwrap_or(status.as_str()));
    }
    if v["stop_reason"] == "refusal" {
        bail!("Claude declined to answer that");
    }
    Ok(v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join(""))
}

/// The conversation as one prompt, for the command-line tools.
fn transcript(turns: &[Turn]) -> String {
    let (last, earlier) = turns.split_last().map(|(l, e)| (l.1.clone(), e)).unwrap_or_default();
    if earlier.is_empty() {
        return last;
    }
    let mut out = String::from("The conversation so far:\n");
    for (role, content) in earlier {
        out.push_str(&format!("{}: {content}\n", if role == "user" { "Reader" } else { "You" }));
    }
    out.push_str(&format!("\nThe reader now says:\n{last}"));
    out
}

/// A scratch directory so the tools don't see (or touch) any project files.
fn scratch() -> anyhow::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("duet-chat-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Claude Code, signed in to your Claude subscription (`claude -p`).
async fn claude_cli(ai: &AiSettings, system: &str, turns: &[Turn]) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new("claude");
    cmd.args(["-p", "--output-format", "json", "--no-session-persistence"])
        .args(["--disallowed-tools", "Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch"])
        .args(["--append-system-prompt", system]);
    if !ai.cli_model.is_empty() {
        cmd.args(["--model", &ai.cli_model]);
    }
    let out = cmd
        .arg(transcript(turns))
        .current_dir(scratch()?)
        .output()
        .await
        .context("running `claude` (is Claude Code installed and signed in?)")?;
    if !out.status.success() {
        bail!("claude: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("unexpected output from `claude`")?;
    if v["is_error"] == true {
        bail!("claude: {}", v["result"].as_str().unwrap_or("error"));
    }
    Ok(v["result"].as_str().unwrap_or("").to_string())
}

/// Codex, signed in to your ChatGPT subscription (`codex exec`), read-only.
async fn codex_cli(ai: &AiSettings, system: &str, turns: &[Turn]) -> anyhow::Result<String> {
    let dir = scratch()?;
    let answer = dir.join(format!("answer-{}.txt", chrono::Utc::now().timestamp_micros()));
    let prompt = format!("{system}\n\nReply with your answer only; don't run commands or edit files.\n\n{}", transcript(turns));
    let mut cmd = tokio::process::Command::new("codex");
    cmd.args(["exec", "--skip-git-repo-check", "--ephemeral", "--sandbox", "read-only", "--color", "never"])
        .arg("--output-last-message")
        .arg(&answer);
    if !ai.cli_model.is_empty() {
        cmd.args(["--model", &ai.cli_model]);
    }
    let out = cmd
        .arg(prompt)
        .current_dir(&dir)
        .output()
        .await
        .context("running `codex` (is Codex installed and signed in?)")?;
    if !out.status.success() {
        bail!("codex: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("failed"));
    }
    let text = tokio::fs::read_to_string(&answer).await.context("codex gave no answer")?;
    let _ = tokio::fs::remove_file(&answer).await;
    Ok(text)
}

/// Gemini CLI, signed in to your Google account (`gemini -p`).
async fn gemini_cli(ai: &AiSettings, system: &str, turns: &[Turn]) -> anyhow::Result<String> {
    let prompt = format!("{system}\n\nReply with your answer only; don't run commands or edit files.\n\n{}", transcript(turns));
    let mut cmd = tokio::process::Command::new("gemini");
    cmd.arg("-p").arg(prompt);
    if !ai.cli_model.is_empty() {
        cmd.args(["-m", &ai.cli_model]);
    }
    run_cli(cmd, "gemini", "Gemini CLI").await
}

/// OpenCode, with whichever provider accounts you've connected to it (`opencode run`).
async fn opencode_cli(ai: &AiSettings, system: &str, turns: &[Turn]) -> anyhow::Result<String> {
    let prompt = format!("{system}\n\nReply with your answer only; don't run commands or edit files.\n\n{}", transcript(turns));
    let mut cmd = tokio::process::Command::new("opencode");
    // Its own server for the call, rather than relying on OpenCode's background service.
    cmd.args(["run", "--standalone"]);
    if !ai.cli_model.is_empty() {
        cmd.args(["--model", &ai.cli_model]);
    }
    cmd.arg(prompt);
    run_cli(cmd, "opencode", "OpenCode").await
}

async fn run_cli(mut cmd: tokio::process::Command, name: &str, product: &str) -> anyhow::Result<String> {
    let out = cmd
        .current_dir(scratch()?)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .with_context(|| format!("running `{name}` (is {product} installed and signed in?)"))?;
    if !out.status.success() {
        bail!("{name}: {}", strip_ansi(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("failed")));
    }
    Ok(strip_ansi(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Terminal colour codes from CLI output.
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
        } else {
            out.push(c);
        }
    }
    out
}
