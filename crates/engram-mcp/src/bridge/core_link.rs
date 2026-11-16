//! Talking to the core: health pings, stall patience, client leases.

/// A ping slower than this is logged even when it lands.
pub(crate) const SLOW_PING: std::time::Duration = std::time::Duration::from_secs(2);

/// One MCP ping with a 10 s ceiling: how long it took, or why it failed.
pub(crate) async fn ping_core(
    peer: &rmcp::service::Peer<rmcp::RoleClient>,
) -> Result<std::time::Duration, String> {
    let start = std::time::Instant::now();
    let ping = peer.send_request(rmcp::model::ClientRequest::PingRequest(
        rmcp::model::PingRequest {
            method: Default::default(),
            extensions: Default::default(),
        },
    ));
    match tokio::time::timeout(std::time::Duration::from_secs(10), ping).await {
        Ok(Ok(_)) => Ok(start.elapsed()),
        Ok(Err(e)) => Err(format!("failed ({e})")),
        Err(_) => Err("unanswered after 10s".into()),
    }
}

/// How far past the beat a gap must run to read as the machine sleeping.
pub(crate) const SLEEP_GAP: std::time::Duration = std::time::Duration::from_secs(30);

/// The time slept through, when a heartbeat gap is too long for a busy
/// runtime alone — the bridge does nothing between beats.
pub(crate) fn slept_through(
    gap: std::time::Duration,
    beat: std::time::Duration,
) -> Option<std::time::Duration> {
    (gap > beat + SLEEP_GAP).then(|| gap - beat)
}

/// How long a core that answers `/health` may leave this session's pings
/// unanswered before the bridge opens a new session. `ENGRAM_BRIDGE_STALL_SECS`
/// overrides the 120 s default for tests.
pub(crate) fn stall_patience() -> std::time::Duration {
    env_secs("ENGRAM_BRIDGE_STALL_SECS", 120)
}

/// How long a bridge waits for a lost core to come back before exiting.
/// `ENGRAM_BRIDGE_RECONNECT_SECS` overrides the 90 s default for tests.
pub(crate) fn reconnect_window() -> std::time::Duration {
    env_secs("ENGRAM_BRIDGE_RECONNECT_SECS", 90)
}

pub(crate) fn env_secs(key: &str, default: u64) -> std::time::Duration {
    std::time::Duration::from_secs(
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default),
    )
}

/// `http://127.0.0.1:8787/projects/x/mcp` → `http://127.0.0.1:8787`.
pub(crate) fn origin_of(url: &str) -> Option<String> {
    let after_scheme = url.find("://")? + 3;
    let end = url[after_scheme..]
        .find('/')
        .map_or(url.len(), |i| after_scheme + i);
    Some(url[..end].to_string())
}

/// The bridge's heartbeat cadence — core-liveness pings AND lease renewals
/// ride it. 15s in production (the core's 45s lease TTL is three beats);
/// `ENGRAM_BRIDGE_HEARTBEAT_SECS` shortens it for sandboxed tests, which
/// also shorten the TTL and would otherwise see live leases flap.
pub(crate) fn heartbeat_secs() -> u64 {
    std::env::var("ENGRAM_BRIDGE_HEARTBEAT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(15)
}

pub(crate) async fn register_lease(
    client: &reqwest::Client,
    base_url: &str,
    root: &str,
    client_name: Option<&str>,
) -> Option<String> {
    let resp = client
        .post(format!("{base_url}/clients"))
        .json(&serde_json::json!({
            "pid": std::process::id(),
            "kind": "mcp-bridge",
            "root": root,
            "client": client_name,
        }))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    body["lease_id"].as_str().map(str::to_string)
}

/// Renew a lease, carrying the (possibly rebound) root — and the client name
/// once known — so the census row follows the binding. Older cores ignore
/// the body — the renewal still counts. False = the lease is gone (expired,
/// core restarted).
pub(crate) async fn ping_lease(
    client: &reqwest::Client,
    base_url: &str,
    id: &str,
    root: Option<&str>,
    client_name: Option<&str>,
) -> bool {
    let mut req = client
        .post(format!("{base_url}/clients/{id}/ping"))
        .timeout(std::time::Duration::from_secs(5));
    if root.is_some() || client_name.is_some() {
        req = req.json(&serde_json::json!({ "root": root, "client": client_name }));
    }
    req.send().await.is_ok_and(|r| r.status().is_success())
}

pub(crate) async fn delete_lease(client: &reqwest::Client, base_url: &str, id: &str) {
    let _ = client
        .delete(format!("{base_url}/clients/{id}"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await;
}
