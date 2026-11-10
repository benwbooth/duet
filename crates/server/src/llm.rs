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
        "claude_cli" => claude_cli(&ai, system, turns, false).await?,
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

/// Like [`chat`], but the model may search and read the web to answer, where it can (Claude
/// Code, or Claude through the API). Other models answer from what they're given.
pub async fn chat_web(state: &State, system: &str, turns: &[Turn]) -> anyhow::Result<String> {
    let ai = state.settings.read().await.ai.clone();
    let text = match ai.llm_provider.as_str() {
        "claude_cli" => claude_cli(&ai, system, turns, true).await?,
        "anthropic" => anthropic_web(state, &ai, system, turns).await?,
        _ => return chat(state, system, turns).await,
    };
    Ok(match text.find("</think>") {
        Some(end) => text[end + "</think>".len()..].trim().to_string(),
        None => text.trim().to_string(),
    })
}

/// Claude with the web search and fetch server tools. A long search can come back paused
/// (`pause_turn`); it's sent back to carry on.
async fn anthropic_web(state: &State, ai: &AiSettings, system: &str, turns: &[Turn]) -> anyhow::Result<String> {
    if ai.anthropic_api_key.is_empty() {
        bail!("Add your Anthropic API key (Settings → AI)");
    }
    let model = if ai.anthropic_model.is_empty() { "claude-opus-5-5" } else { &ai.anthropic_model };
    let mut messages: Vec<serde_json::Value> =
        turns.iter().map(|(role, content)| json!({ "role": role, "content": content })).collect();
    for _ in 0..4 {
        let mut body = json!({
            "model": model,
            "max_tokens": 16000,
            "system": system,
            "messages": messages,
            "thinking": { "type": "adaptive" },
            "output_config": { "effort": "low" },
            "tools": [
                { "type": "web_search_20260209", "name": "web_search", "max_uses": 5 },
                { "type": "web_fetch_20260209", "name": "web_fetch", "max_uses": 5 },
            ],
        });
        let mut req = state
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &ai.anthropic_api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .timeout(std::time::Duration::from_secs(600));
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
        if v["stop_reason"] == "pause_turn" {
            messages.push(json!({ "role": "assistant", "content": v["content"] }));
            continue;
        }
        return Ok(v["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join(""));
    }
    bail!("the web search took too long")
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
async fn claude_cli(ai: &AiSettings, system: &str, turns: &[Turn], web: bool) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new("claude");
    cmd.args(["-p", "--output-format", "json", "--no-session-persistence"]);
    if web {
        cmd.args(["--disallowed-tools", "Bash,Edit,Write,NotebookEdit"]).args(["--allowed-tools", "WebSearch,WebFetch"]);
    } else {
        cmd.args(["--disallowed-tools", "Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch"]);
    }
    cmd
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

/// A picture shown to a model that can see, with a question about it: the comic reader's
/// vision model (a local one on the graphics card by default), or the chat model when it's one
/// that can look at pictures. Answers in JSON when asked to.
pub async fn vision(state: &State, system: &str, prompt: &str, image: &[u8], mime: &str) -> anyhow::Result<String> {
    use base64::Engine;
    let ai = state.settings.read().await.ai.clone();
    let b64 = base64::engine::general_purpose::STANDARD.encode(image);
    let text = if ai.vision_provider == "chat" {
        match ai.llm_provider.as_str() {
            "anthropic" => anthropic_vision(state, &ai, system, prompt, &b64, mime).await?,
            "openai" => openai_vision(state, OPENAI_URL, &ai.openai_api_key, &ai.openai_model, system, prompt, &b64, mime).await?,
            "service" => openai_vision(state, &ai.service_url, &ai.service_key, &ai.service_model, system, prompt, &b64, mime).await?,
            "claude_cli" | "codex_cli" => cli_vision(&ai, system, prompt, image, mime).await?,
            "local" if !ai.llm_url.is_empty() => openai_vision(state, &ai.llm_url, &ai.llm_api_key, &ai.llm_model, system, prompt, &b64, mime).await?,
            _ => bail!("This chat model can't look at pictures: choose a vision model for comics (Settings → AI)"),
        }
    } else {
        if ai.vision_url.is_empty() || ai.vision_model.is_empty() {
            bail!("No vision model set up for comics (Settings → AI)");
        }
        if ai.vision_url.contains(":11434") {
            ollama_vision(state, &ai.vision_url, &ai.vision_model, system, prompt, &b64).await?
        } else {
            openai_vision(state, &ai.vision_url, "", &ai.vision_model, system, prompt, &b64, mime).await?
        }
    };
    Ok(match text.find("</think>") {
        Some(end) => text[end + "</think>".len()..].trim().to_string(),
        None => text.trim().to_string(),
    })
}

/// Ollama's own API, so the model can be asked for JSON, without thinking first, and to leave
/// the graphics card soon after (the voices need it).
async fn ollama_vision(state: &State, url: &str, model: &str, system: &str, prompt: &str, b64: &str) -> anyhow::Result<String> {
    let base = url.trim_end_matches('/').trim_end_matches("/v1");
    let body = json!({
        "model": model,
        "stream": false,
        "format": "json",
        "think": false,
        "keep_alive": "3m",
        "options": { "temperature": 0.1, "num_ctx": 16384 },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": prompt, "images": [b64] },
        ],
    });
    let resp = state
        .http
        .post(format!("{base}/api/chat"))
        .timeout(std::time::Duration::from_secs(600))
        .json(&body)
        .send()
        .await
        .context("the vision model isn't answering (is Ollama running?)")?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("Ollama doesn't have {model} yet: pull it in Settings → AI (or `ollama pull {model}`)");
    }
    let v: serde_json::Value = resp.error_for_status().context("the vision model refused the request")?.json().await?;
    Ok(v["message"]["content"].as_str().unwrap_or("").to_string())
}

#[allow(clippy::too_many_arguments)]
async fn openai_vision(state: &State, url: &str, key: &str, model: &str, system: &str, prompt: &str, b64: &str, mime: &str) -> anyhow::Result<String> {
    if model.is_empty() {
        bail!("Choose a model (Settings → AI)");
    }
    let body = json!({
        "model": model,
        "temperature": 0.1,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{b64}") } },
            ] },
        ],
    });
    let resp: serde_json::Value = state
        .http
        .post(format!("{}/chat/completions", url.trim_end_matches('/')))
        .bearer_auth(key)
        .timeout(std::time::Duration::from_secs(600))
        .json(&body)
        .send()
        .await
        .context("the vision model isn't answering")?
        .error_for_status()
        .context("the vision model refused the request (check the model name, and that it can see pictures)")?
        .json()
        .await?;
    Ok(resp["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
}

async fn anthropic_vision(state: &State, ai: &AiSettings, system: &str, prompt: &str, b64: &str, mime: &str) -> anyhow::Result<String> {
    if ai.anthropic_api_key.is_empty() {
        bail!("Add your Anthropic API key (Settings → AI)");
    }
    let model = if ai.anthropic_model.is_empty() { "claude-opus-5-5" } else { &ai.anthropic_model };
    let body = json!({
        "model": model,
        "max_tokens": 16000,
        "system": system,
        "messages": [{ "role": "user", "content": [
            { "type": "image", "source": { "type": "base64", "media_type": mime, "data": b64 } },
            { "type": "text", "text": prompt },
        ] }],
        "output_config": { "effort": "low" },
    });
    let resp = state
        .http
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &ai.anthropic_api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .timeout(std::time::Duration::from_secs(600))
        .json(&body)
        .send()
        .await
        .context("can't reach the Anthropic API")?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.context("unexpected reply from the Anthropic API")?;
    if !status.is_success() {
        bail!("Anthropic API: {}", v["error"]["message"].as_str().unwrap_or(status.as_str()));
    }
    Ok(v["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""))
}

/// Claude Code or Codex, shown the picture as a file in their scratch folder.
async fn cli_vision(ai: &AiSettings, system: &str, prompt: &str, image: &[u8], mime: &str) -> anyhow::Result<String> {
    let dir = scratch()?;
    let ext = match mime {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "jpg",
    };
    let file = dir.join(format!("page-{}.{ext}", chrono::Utc::now().timestamp_micros()));
    tokio::fs::write(&file, image).await?;
    let name = file.file_name().unwrap().to_string_lossy().to_string();
    let result = if ai.llm_provider == "claude_cli" {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.args(["-p", "--output-format", "json", "--no-session-persistence"])
            .args(["--allowedTools", "Read"])
            .args(["--disallowed-tools", "Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch"])
            .args(["--append-system-prompt", system]);
        if !ai.cli_model.is_empty() {
            cmd.args(["--model", &ai.cli_model]);
        }
        let out = cmd
            .arg(format!("The page is the picture {name} in this folder: look at it (Read), then answer.\n\n{prompt}"))
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .context("running `claude` (is Claude Code installed and signed in?)");
        out.and_then(|out| {
            if !out.status.success() {
                bail!("claude: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("unexpected output from `claude`")?;
            if v["is_error"] == true {
                bail!("claude: {}", v["result"].as_str().unwrap_or("error"));
            }
            Ok(v["result"].as_str().unwrap_or("").to_string())
        })
    } else {
        let answer = dir.join(format!("answer-{}.txt", chrono::Utc::now().timestamp_micros()));
        let mut cmd = tokio::process::Command::new("codex");
        cmd.args(["exec", "--skip-git-repo-check", "--ephemeral", "--sandbox", "read-only", "--color", "never"])
            .arg("--output-last-message")
            .arg(&answer)
            .arg("--image")
            .arg(&file);
        if !ai.cli_model.is_empty() {
            cmd.args(["--model", &ai.cli_model]);
        }
        let out = cmd
            .arg(format!("{system}\n\nReply with your answer only; don't run commands or edit files.\n\n{prompt}"))
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .context("running `codex` (is Codex installed and signed in?)");
        let r = match out {
            Ok(o) if o.status.success() => tokio::fs::read_to_string(&answer).await.context("codex gave no answer"),
            Ok(o) => Err(anyhow::anyhow!("codex: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("failed"))),
            Err(e) => Err(e),
        };
        let _ = tokio::fs::remove_file(&answer).await;
        r
    };
    let _ = tokio::fs::remove_file(&file).await;
    result
}
