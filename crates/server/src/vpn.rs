//! On-demand VPN exit-IP renewal.
//!
//! The shared `pia-vpn` container holds the tunnel; every sidecar
//! (qBittorrent, proxy, browsers, geckodriver) shares its network
//! namespace, so they all go stale when the tunnel drops. Renewing means:
//! restart the tunnel, wait for it to come back (polling the exit IP
//! through the proxy), then recreate the dependents. Takes a minute or
//! two; searches/downloads fail while the tunnel is down.

use std::sync::{LazyLock, Mutex};

static STATUS: LazyLock<Mutex<VpnStatus>> = LazyLock::new(|| Mutex::new(VpnStatus::default()));

/// What the UI shows under the button.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct VpnStatus {
    /// idle | running | done | error
    pub state: String,
    pub old_ip: Option<String>,
    pub new_ip: Option<String>,
    pub message: String,
}

impl Default for VpnStatus {
    fn default() -> Self {
        VpnStatus { state: "idle".into(), old_ip: None, new_ip: None, message: String::new() }
    }
}

/// Current public exit IP through the proxy (what sites see). None when
/// the proxy/tunnel is down.
pub async fn current_ip(site: &reqwest::Client) -> Option<String> {
    exit_ip(site).await
}

pub fn status() -> VpnStatus {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

fn set(f: impl FnOnce(&mut VpnStatus)) {
    if let Ok(mut s) = STATUS.lock() {
        f(&mut s);
    }
}

/// Starts a renewal unless one is already running. Returns the status.
pub async fn renew(site: reqwest::Client, cfg: duet_common::VpnSettings) {
    if status().state == "running" {
        return;
    }
    if !cfg.enabled || cfg.compose_dir.trim().is_empty() {
        set(|s| {
            s.state = "error".into();
            s.message = "VPN renewal isn't configured — set it up in Settings → VPN.".into();
        });
        return;
    }
    set(|s| {
        *s = VpnStatus { state: "running".into(), old_ip: None, new_ip: None, message: "Restarting the VPN tunnel…".into() };
    });
    tokio::spawn(run_renew(site, cfg));
}

async fn cmd(prog: &str, args: &[&str], dir: &std::path::Path, timeout_secs: u64) -> anyhow::Result<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::process::Command::new(prog).args(args).current_dir(dir).output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("{prog} timed out"))?
    .map_err(|e| anyhow::anyhow!("couldn't run {prog}: {e}"))?;
    if !out.status.success() {
        anyhow::bail!("{} failed: {}", prog, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn exit_ip(site: &reqwest::Client) -> Option<String> {
    site.get("https://api.ipify.org").timeout(std::time::Duration::from_secs(8)).send().await.ok()?.error_for_status().ok()?.text().await.ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Exit IP from inside the VPN container's own network namespace.
/// The proxy sidecar shares the OLD namespace after a restart, so it stays
/// dead until the dependents are recreated — polling through it would never
/// succeed. `docker exec` runs in the fresh namespace instead.
async fn exit_ip_inside(container: &str) -> Option<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new("docker")
            .args(["exec", container, "curl", "--connect-timeout", "8", "--max-time", "20", "-s", "https://api.ipify.org"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if ip.is_empty() { None } else { Some(ip) }
}

async fn run_renew(site: reqwest::Client, cfg: duet_common::VpnSettings) {
    let fail = |m: String| set(|s| {
        s.state = "error".into();
        s.message = m;
    });
    let raw = cfg.compose_dir.trim().to_string();
    // Accept ~/ as well as absolute paths.
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        std::env::var("HOME").map(|h| format!("{h}/{rest}")).unwrap_or(raw)
    } else {
        raw
    };
    let dir = std::path::PathBuf::from(expanded);
    let old_ip = exit_ip(&site).await;
    set(|s| {
        s.old_ip = old_ip.clone();
        s.message = format!("Tunnel restarting (was {})…", old_ip.as_deref().unwrap_or("unknown"));
    });
    // A reconnect usually lands a new exit IP on a different PIA server.
    // (`docker restart` returns once the container is up; OpenVPN connects
    // asynchronously, so the wait below is what actually gates on the tunnel.)
    if let Err(e) = cmd("docker", &["restart", cfg.container.trim()], &dir, 120).await {
        fail(format!("VPN restart failed: {e:#}"));
        return;
    }
    set(|s| {
        s.message = "Tunnel restarting — waiting for the new connection…".into();
    });
    let mut new_ip = None;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if let Some(ip) = exit_ip_inside(cfg.container.trim()).await {
            if old_ip.as_ref().is_none_or(|o| &ip != o) {
                new_ip = Some(ip);
                break;
            }
            new_ip = Some(ip);
        }
    }
    let Some(ip) = new_ip else {
        fail("VPN didn't come back within ~4 minutes — check `docker logs pia_vpn`".into());
        return;
    };
    // Dependents share the old namespace: recreate them on the new one.
    // (The browsers keep their profiles/volumes; WebDriver starts clean.)
    set(|s| {
        s.new_ip = Some(ip.clone());
        s.message = format!("Tunnel back on {ip} — restarting sidecars…");
    });
    let dependents: Vec<&str> = cfg.dependents.split_whitespace().collect();
    if dependents.is_empty() {
        fail("No sidecars configured — add them in Settings → VPN.".into());
        return;
    }
    let mut args = vec!["compose", "up", "-d"];
    args.extend(&dependents);
    if let Err(e) = cmd("docker", &args, &dir, 240).await {
        fail(format!("VPN is on {ip}, but dependents failed to restart: {e:#} — run `docker compose up -d {}` in ~/src/home-services", dependents.join(" ")));
        return;
    }
    // The proxy sidecar is fresh now too — confirm the whole path.
    let via_proxy = exit_ip(&site).await;
    set(|s| {
        s.state = "done".into();
        s.message = match via_proxy {
            Some(p) if p == ip => format!("Exit IP {} → {ip}. Proxy, browsers and qBittorrent are back.", old_ip.as_deref().unwrap_or("?")),
            Some(p) => format!("Exit IP {} → {ip} (proxy reports {p} — re-check in a minute). Sidecars restarted.", old_ip.as_deref().unwrap_or("?")),
            None => format!("Exit IP {} → {ip}. Sidecars restarted, but the proxy isn't answering yet — wait a minute and retry a search.", old_ip.as_deref().unwrap_or("?")),
        };
    });
}
