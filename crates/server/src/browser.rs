//! The visible remote Firefox, restarted in place with or without the
//! Marionette flag. Same profile either way, so all cookies survive.
//!
//! duet drives this browser (Marionette on :2828) for bot-walled searches.
//! Logins that refuse automation-flagged browsers (X via Google) need it
//! clean. Switching is a plain process restart inside the aa-browser
//! container — no container recreate — and duet ensures driven mode itself
//! before every search, so clean mode is only ever a temporary excursion.

use anyhow::Context;

/// How the visible browser is currently running.
#[derive(Debug, PartialEq)]
pub enum Mode {
    /// Up with Marionette: duet can drive it.
    Driven,
    /// Up without Marionette: clean, for picky logins.
    Clean,
    /// No visible Firefox at all (or the container is unreachable).
    Down,
}

async fn exec(args: &[&str]) -> anyhow::Result<String> {
    let out = tokio::process::Command::new("docker")
        .args(args)
        .output()
        .await
        .context("couldn't reach the browser container (is docker up?)")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Process up? Marionette port open? Both read straight from the container.
pub async fn mode() -> Mode {
    let probe = exec(&["exec", "aa_browser", "sh", "-c",
        "pgrep -x firefox >/dev/null && echo UP; ss -tln 2>/dev/null | grep -q 127.0.0.1:2828 && echo PORT",
    ])
    .await
    .unwrap_or_default();
    let up = probe.contains("UP");
    let port = probe.contains("PORT");
    match (up, port) {
        (true, true) => Mode::Driven,
        (true, false) => Mode::Clean,
        _ => Mode::Down,
    }
}

/// Restart the visible browser with or without `--marionette`, polling for
/// readiness instead of fixed sleeps (kill ~1-2s, cold start ~8-15s).
/// `start_url` is what it opens at (usually the AA mirror or pending search).
pub async fn restart(marionette: bool, start_url: &str) -> anyhow::Result<Mode> {
    // TERM lets Firefox flush session and cookies; poll for the exit.
    let _ = exec(&[
        "exec", "aa_browser", "sh", "-c",
        // Every Firefox, however it was started (`--marionette about:blank`
        // too); one left running would just take the new URL itself.
        "pkill -TERM -x firefox; for d in /config/.mozilla/firefox /config/.config/mozilla/firefox; do rm -f $d/*.default-release/lock $d/*.default-release/.parentlock; done",
    ])
    .await?;
    for _ in 0..20 {
        let left = exec(&["exec", "aa_browser", "sh", "-c", "pgrep -x firefox || true"]).await.unwrap_or_default();
        if left.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let mut args: Vec<&str> = vec![
        "exec", "-d", "--user", "abc",
        "-e", "DISPLAY=:1", "-e", "HOME=/config",
        "-e", "XDG_RUNTIME_DIR=/config/.XDG", "-e", "WAYLAND_DISPLAY=wayland-1",
        "-e", "XCURSOR_THEME=breeze_cursors", "-e", "XCURSOR_SIZE=24",
        "aa_browser", "/usr/bin/firefox",
    ];
    let flag;
    if marionette {
        flag = "--marionette".to_string();
        args.push(&flag);
    }
    args.push(start_url);
    exec(&args).await.context("couldn't relaunch the browser")?;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let m = mode().await;
        if (marionette && m == Mode::Driven) || (!marionette && m == Mode::Clean) {
            if marionette {
                // The window sometimes comes back fullscreen (no toolbar,
                // auto-hiding on hover): F11 it back to a normal window.
                let _ = exit_fullscreen().await;
            }
            if marionette {
                // The driver holds a control connection to the old browser
                // instance; a restart strands it (and a stranded driver can
                // wedge completely). Fresh driver for the fresh browser.
                let _ = exec(&["restart", "geckodriver"]).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            return Ok(m);
        }
    }
    let m = mode().await;
    anyhow::bail!("browser didn't reach {} mode (now {:?})", if marionette { "driven" } else { "clean" }, m);
}

/// If the visible window is stuck fullscreen (no toolbar, auto-hiding on
/// hover), one F11 keypress through WebDriver restores the normal window.
async fn exit_fullscreen() -> anyhow::Result<()> {
    let http = reqwest::Client::new();
    let resp: serde_json::Value = http
        .post("http://127.0.0.1:4444/session")
        .json(&serde_json::json!({ "capabilities": { "alwaysMatch": { "browserName": "firefox" } } }))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .json()
        .await?;
    let Some(session) = resp["value"]["sessionId"].as_str() else {
        return Ok(());
    };
    let full: bool = http
        .post(format!("http://127.0.0.1:4444/session/{session}/execute/sync"))
        .json(&serde_json::json!({ "script": "return window.fullScreen", "args": [] }))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v["value"].as_bool())
        .unwrap_or(false);
    if full {
        // F11 toggles window fullscreen off.
        let _ = http
            .post(format!("http://127.0.0.1:4444/session/{session}/actions"))
            .json(&serde_json::json!({ "actions": [{ "type": "key", "id": "f11", "actions": [{ "type": "keyDown", "value": "\\uE031" }, { "type": "keyUp", "value": "\\uE031" }] }] }))
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await;
    }
    let _ = http.delete(format!("http://127.0.0.1:4444/session/{session}")).timeout(std::time::Duration::from_secs(10)).send().await;
    Ok(())
}

/// Make sure duet can drive: relaunch with Marionette unless already there.
/// Returns true when it restarted (the search that follows pays ~15-30s).
pub async fn ensure_driven(start_url: &str) -> anyhow::Result<bool> {
    if mode().await == Mode::Driven {
        return Ok(false);
    }
    tracing::info!("visible browser isn't drivable — restarting it with Marionette");
    restart(true, start_url).await?;
    Ok(true)
}
