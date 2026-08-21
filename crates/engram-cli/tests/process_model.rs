//! Sandboxed end-to-end coverage of the 0.8.8 process model: one machine
//! core, light `serve`/`mcp` access points, client leases, orchestrated stop.
//!
//! Every test gets its own scratch ENGRAM_HOME + HOME, fake embeddings, and a
//! high port range, so a real core on 8787 (and the user's actual graphs) are
//! never touched. Cores are stopped by a drop guard even when a test panics.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_engram-alpha");

struct Sandbox {
    root: PathBuf,
    home: PathBuf,
    port: u16,
    extra_env: Vec<(&'static str, String)>,
}

impl Sandbox {
    /// `port` must be unique per test — the core walks up to 16 ports from it.
    fn new(tag: &str, port: u16) -> Self {
        let root = std::env::temp_dir().join(format!("engram-pm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home/.engram");
        std::fs::create_dir_all(&home).unwrap();
        Self {
            root,
            home,
            port,
            extra_env: Vec::new(),
        }
    }

    /// Extra environment for every command (the detached core inherits it).
    fn with_env(mut self, key: &'static str, value: &str) -> Self {
        self.extra_env.push((key, value.to_string()));
        self
    }

    fn project(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(dir.join(".engram")).unwrap();
        dir
    }

    fn cmd(&self, args: &[&str], cwd: &Path) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(cwd)
            .env("ENGRAM_HOME", &self.home)
            .env("HOME", self.root.join("home"))
            .env("ENGRAM_HTTP_PORT", self.port.to_string())
            .env("ENGRAM_UPDATE_CHECK", "0")
            // Short lease TTL so expiry is testable, with a heartbeat well
            // under it so LIVE leases never flap between renewals.
            .env("ENGRAM_LEASE_TTL_SECS", "3")
            .env("ENGRAM_BRIDGE_HEARTBEAT_SECS", "1");
        for (key, value) in &self.extra_env {
            c.env(key, value);
        }
        c
    }

    /// The port the core actually landed on, from its advertisement.
    fn core_port(&self) -> Option<u16> {
        let raw = std::fs::read_to_string(self.home.join("daemon.json")).ok()?;
        Some(serde_json::from_str::<serde_json::Value>(&raw).ok()?["port"].as_u64()? as u16)
    }

    fn core_pid(&self) -> Option<u32> {
        let raw = std::fs::read_to_string(self.home.join("daemon.json")).ok()?;
        Some(serde_json::from_str::<serde_json::Value>(&raw).ok()?["pid"].as_u64()? as u32)
    }

    fn wait_core_healthy(&self, timeout: Duration) -> u16 {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Some(port) = self.core_port()
                && http_get(port, "/health").is_some()
            {
                return port;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!("core never became healthy in {}", self.root.display());
    }
}

/// Whatever happens in the test, the sandbox's core must not outlive it.
impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(port) = self.core_port() {
            let _ = http_post(port, "/shutdown", "{}");
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                if http_get(port, "/health").is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if let Some(pid) = self.core_pid() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Minimal localhost GET, HTTP/1.0 so the body follows the blank line.
fn http_get(port: u16, path: &str) -> Option<String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.split_once("\r\n\r\n").map(|(_, b)| b.to_string())
}

fn http_post(port: u16, path: &str, body: &str) -> Option<String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    write!(
        stream,
        "POST {path} HTTP/1.0\r\nHost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    (text.starts_with("HTTP/1.0 2") || text.starts_with("HTTP/1.1 2"))
        .then(|| text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()))?
}

fn system_json(port: u16) -> serde_json::Value {
    serde_json::from_str(&http_get(port, "/system").expect("/system answers")).unwrap()
}

fn census_len(port: u16) -> usize {
    system_json(port)["processes"]["clients"]
        .as_array()
        .map(|c| c.len())
        .unwrap_or(0)
}

fn models_state(port: u16) -> String {
    system_json(port)["models_state"]["state"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Poll until `pred` holds or the timeout runs out; returns whether it held.
fn eventually(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// One stdio MCP bridge child plus the plumbing to speak line-delimited
/// JSON-RPC with it.
struct Bridge {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl Bridge {
    fn spawn(sb: &Sandbox, project: &Path) -> Self {
        let mut child = sb
            .cmd(
                &["mcp", "--db", ".engram/graph.db", "--fake-embeddings"],
                project,
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawning the mcp bridge");
        let reader = BufReader::new(child.stdout.take().expect("bridge stdout"));
        let mut bridge = Self { child, reader };
        bridge.handshake();
        bridge
    }

    fn handshake(&mut self) {
        self.send(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"pm-test","version":"0"}}}"#,
        );
        let init = self.recv();
        assert!(
            init.contains("serverInfo"),
            "initialize reply looks wrong: {init}"
        );
        self.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    }

    fn send(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().expect("bridge stdin");
        writeln!(stdin, "{line}").expect("writing to the bridge");
        stdin.flush().unwrap();
    }

    fn recv(&mut self) -> String {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .expect("reading from the bridge");
        assert!(!line.is_empty(), "bridge closed stdout");
        line
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.kill();
    }
}

// ---- tests ----------------------------------------------------------------

#[test]
fn serve_spawns_core_registers_project_and_exits() {
    let sb = Sandbox::new("serve", 18800);
    let proj = sb.project("alpha");

    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .expect("running serve");
    assert!(out.status.success(), "serve failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("pane: http://127.0.0.1:"),
        "serve prints the pane URL: {stdout}"
    );

    // serve exited; the core it spawned survives and serves /health.
    let port = sb.wait_core_healthy(Duration::from_secs(10));
    let first_pid = sb.core_pid().unwrap();

    // The repo-local advertisement exists and carries the core's port
    // (plugins regex-scrape `port` out of this file).
    let repo_ad = std::fs::read_to_string(proj.join(".engram/daemon.json")).unwrap();
    let ad: serde_json::Value = serde_json::from_str(&repo_ad).unwrap();
    assert_eq!(ad["port"].as_u64(), Some(port as u64));

    // The project is registered with the core.
    let projects = http_get(port, "/projects").unwrap();
    assert!(projects.contains("alpha"), "core knows alpha: {projects}");

    // A second serve converges on the same core instead of starting another.
    let out2 = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .unwrap();
    assert!(out2.status.success());
    assert_eq!(sb.core_pid(), Some(first_pid), "still the same core");
}

#[test]
fn serve_http_only_blocks_while_core_lives_and_exits_when_it_dies() {
    let sb = Sandbox::new("httponly", 18820);
    let proj = sb.project("alpha");

    let mut child = sb
        .cmd(&["serve", "--http-only", "--fake-embeddings"], &proj)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let port = sb.wait_core_healthy(Duration::from_secs(15));

    // Deprecated blocking mode: still running well after the core is up.
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        child.try_wait().unwrap().is_none(),
        "--http-only serve must block while the core is healthy"
    );

    http_post(port, "/shutdown", "{}").expect("core accepts /shutdown");
    let exited = eventually(Duration::from_secs(15), || {
        child.try_wait().unwrap().is_some()
    });
    assert!(exited, "--http-only serve exits when the core dies");
}

#[test]
fn mcp_spawns_core_bridges_tools_and_census_tracks_expiry() {
    let sb = Sandbox::new("mcp", 18840);
    let proj = sb.project("alpha");

    // No core exists: the bridge must start one and proxy to it.
    let mut a = Bridge::spawn(&sb, &proj);
    let port = sb.wait_core_healthy(Duration::from_secs(30));

    a.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let tools = a.recv();
    assert!(
        tools.contains("add_note") && tools.contains("search"),
        "bridge proxies the core's tools: {tools}"
    );

    // Two bridges → two leases in the census.
    let mut b = Bridge::spawn(&sb, &proj);
    assert!(
        eventually(Duration::from_secs(10), || census_len(port) == 2),
        "two bridges show up in /system: {}",
        system_json(port)["processes"]
    );
    let clients = system_json(port)["processes"]["clients"].clone();
    assert!(
        clients.as_array().unwrap().iter().all(|c| {
            c["kind"].as_str() == Some("mcp-bridge")
                && c["pid"].as_u64().is_some()
                && c["root"].as_str().is_some_and(|r| r.contains("alpha"))
                && c["connected_at"].as_i64().is_some()
                && c["last_seen"].as_i64().is_some()
        }),
        "lease rows carry the /system contract: {clients}"
    );

    // A killed bridge can't deregister — its lease must EXPIRE
    // (ENGRAM_LEASE_TTL_SECS=3 in the sandbox).
    b.kill();
    assert!(
        eventually(Duration::from_secs(15), || census_len(port) == 1),
        "the killed bridge's lease expires"
    );
    drop(a);
}

#[test]
fn stop_orchestrates_shutdown_and_releases_the_tepin_lock() {
    let sb = Sandbox::new("stop", 18860);
    let proj = sb.project("alpha");
    let db = proj.join(".engram/graph.tepin");

    let out = sb
        .cmd(
            &["serve", "--db", ".engram/graph.tepin", "--fake-embeddings"],
            &proj,
        )
        .output()
        .unwrap();
    assert!(out.status.success(), "serve failed: {out:?}");
    let port = sb.wait_core_healthy(Duration::from_secs(10));

    // Make the core actually open (and lock) the store.
    let projects: serde_json::Value =
        serde_json::from_str(&http_get(port, "/projects").unwrap()).unwrap();
    let id = projects
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "alpha")
        .and_then(|p| p["id"].as_str())
        .expect("alpha registered")
        .to_string();
    http_get(port, &format!("/projects/{id}/graph")).expect("core opens the store");
    assert!(db.exists(), "the core created the tepin store");

    let stop = sb.cmd(&["stop"], &proj).output().unwrap();
    assert!(stop.status.success(), "stop failed: {stop:?}");
    let report = String::from_utf8_lossy(&stop.stdout);
    assert!(
        report.contains("stopped machine core"),
        "stop reports what it stopped: {report}"
    );

    assert!(
        eventually(Duration::from_secs(10), || http_get(port, "/health")
            .is_none()),
        "the core is gone"
    );
    assert!(
        !sb.home.join("daemon.json").exists(),
        "machine advertisement removed"
    );
    assert!(
        !proj.join(".engram/daemon.json").exists(),
        "repo advertisement removed"
    );

    // The lock is released: a direct open of the tepin store now succeeds.
    let store = engram_core::open_store(&db).expect("tepin store opens after stop");
    drop(store);
}

#[test]
fn doctor_reports_core_held_store_as_healthy() {
    let sb = Sandbox::new("doctor", 18880);
    let proj = sb.project("alpha");

    // Issue #6's shape: the core auto-spawned (not `serve` from the repo)
    // and holding the repo's tepin store open.
    let mut bridge = Bridge::spawn(&sb, &proj);
    let port = sb.wait_core_healthy(Duration::from_secs(30));
    // The bridge registers the project at bind time, AFTER the stdio
    // handshake (handshake-first) — wait for alpha instead of assuming
    // registration preceded the initialize answer.
    assert!(
        eventually(Duration::from_secs(20), || http_get(port, "/projects")
            .is_some_and(|p| p.contains("alpha"))),
        "the bridge registers alpha once bound"
    );
    let projects: serde_json::Value =
        serde_json::from_str(&http_get(port, "/projects").unwrap()).unwrap();
    let id = projects
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "alpha")
        .and_then(|p| p["id"].as_str())
        .unwrap()
        .to_string();
    http_get(port, &format!("/projects/{id}/graph")).expect("core opens the store");

    let out = sb.cmd(&["doctor"], &proj).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("FAIL"),
        "doctor must not fail on a healthy core-held setup:\n{stdout}"
    );
    assert!(
        stdout.contains("machine core healthy"),
        "the daemon check finds the core regardless of how it started:\n{stdout}"
    );
    assert!(
        stdout.contains("held open by the machine core"),
        "the store check asks the core instead of direct-opening:\n{stdout}"
    );
    assert!(out.status.success(), "doctor exits zero: {out:?}");
    bridge.kill();
}

#[test]
fn idle_unload_flips_state_and_search_reloads_single_flight() {
    // A 2s idle window (vs the 900s default) makes the whole cycle testable.
    let sb = Sandbox::new("idle", 18920).with_env("ENGRAM_IDLE_UNLOAD_SECS", "2");
    let proj = sb.project("alpha");

    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .expect("running serve");
    assert!(out.status.success(), "serve failed: {out:?}");
    let port = sb.wait_core_healthy(Duration::from_secs(10));

    // A graph write is HTTP activity AND model use — both clocks re-arm and
    // any boot-window unload (the 2s test window can elapse during startup)
    // is reloaded through the write's own embedding.
    http_post(
        port,
        "/nodes",
        r#"{"type":"Decision","title":"idle unload proof note","durability":"stable","source":"user"}"#,
    )
    .expect("node created");
    assert_eq!(
        models_state(port),
        "loaded",
        "a write leaves models resident"
    );

    // No bridge lease, no further counted activity → the state flips. The
    // polling here is itself the proof that /system (and the /health checks
    // inside core_port waits) are exempt from the HTTP-activity clock —
    // stamping them would keep models warm forever.
    let unload_since = engram_core_now();
    assert!(
        eventually(Duration::from_secs(15), || models_state(port)
            == "unloaded_idle"),
        "models_state flips to unloaded_idle: {}",
        system_json(port)["models_state"]
    );
    let since = system_json(port)["models_state"]["since"]
        .as_i64()
        .unwrap_or(0);
    assert!(
        since >= unload_since - 1,
        "unloaded_idle.since is the unload instant, not the boot instant"
    );

    // First demand reloads — single-flight: two concurrent searches both
    // land while the slot is empty, exactly one load runs, both answer.
    let handles: Vec<_> = (0..2)
        .map(|_| std::thread::spawn(move || http_get(port, "/search?q=idle%20unload%20proof")))
        .collect();
    for h in handles {
        let body = h.join().unwrap().expect("search answers after reload");
        assert!(
            body.contains("idle unload proof note"),
            "search finds the pre-unload note after reloading: {body}"
        );
    }
    assert_eq!(
        models_state(port),
        "loaded",
        "the on-demand reload flipped the state back"
    );
}

#[test]
fn bridge_lease_prevents_idle_unload_until_it_expires() {
    let sb = Sandbox::new("idlelease", 18940).with_env("ENGRAM_IDLE_UNLOAD_SECS", "2");
    let proj = sb.project("alpha");

    let mut bridge = Bridge::spawn(&sb, &proj);
    let port = sb.wait_core_healthy(Duration::from_secs(30));
    assert!(
        eventually(Duration::from_secs(10), || census_len(port) == 1),
        "the bridge holds a lease"
    );

    // One model demand makes models resident regardless of whether the 2s
    // test window already elapsed during the core's quiet boot. Its HTTP and
    // model-use stamps go stale 2s later — from then on the renewing lease
    // is the only thing between the models and the unload.
    http_get(port, "/search?q=warmup").expect("search answers");
    assert_eq!(models_state(port), "loaded");

    // Hold three idle windows with a silent-but-connected bridge: the live
    // lease must keep models resident the whole time (strict spec — a
    // connected agent keeps models warm even when it says nothing).
    let held = Instant::now();
    while held.elapsed() < Duration::from_secs(6) {
        assert_eq!(
            models_state(port),
            "loaded",
            "a live lease prevents idle unload"
        );
        std::thread::sleep(Duration::from_millis(500));
    }

    // Kill the bridge (no clean deregister): the lease expires on its 3s
    // TTL, every clock goes quiet, and the unload follows.
    bridge.kill();
    assert!(
        eventually(Duration::from_secs(20), || models_state(port)
            == "unloaded_idle"),
        "unload follows lease expiry: {}",
        system_json(port)["models_state"]
    );
}

/// Unix seconds, without dragging a clock dependency into the test.
fn engram_core_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[test]
fn status_renders_core_projects_and_clients() {
    let sb = Sandbox::new("status", 18900);
    let proj = sb.project("alpha");

    // No core yet: clean not-running output, both shapes.
    let out = sb.cmd(&["status"], &proj).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("not running"));
    let out = sb.cmd(&["status", "--json"], &proj).output().unwrap();
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(v["running"], serde_json::json!(false));

    // With a core and one bridge.
    let mut bridge = Bridge::spawn(&sb, &proj);
    let port = sb.wait_core_healthy(Duration::from_secs(30));
    assert!(eventually(Duration::from_secs(10), || census_len(port) >= 1));

    let out = sb.cmd(&["status"], &proj).output().unwrap();
    assert!(out.status.success(), "status failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "engram core",
        "pid",
        "models: loaded",
        "alpha",
        "mcp-bridge",
    ] {
        assert!(
            stdout.contains(needle),
            "status misses {needle:?}:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("ago") || stdout.contains("just now"),
        "humane durations, not unix timestamps:\n{stdout}"
    );

    let out = sb.cmd(&["status", "--json"], &proj).output().unwrap();
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(v["running"], serde_json::json!(true));
    assert!(v["system"]["processes"]["core"]["pid"].as_u64().is_some());
    assert_eq!(v["system"]["models_state"]["state"], "loaded");
    bridge.kill();
}
