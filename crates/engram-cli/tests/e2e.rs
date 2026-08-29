//! Lane-1 e2e (0.8.13): user journeys over the real binary — the field
//! scenarios that unit tests can't see because they live between processes,
//! binaries, and restarts. Every test is a story a user actually lived.
//!
//! Version-update mechanics run one binary as two versions via
//! `ENGRAM_TEST_VERSION` (see engram_core::advertised_version) — the
//! handshake compares advertised versions, so an override on the "old" side
//! reproduces a stale-binary core without building the workspace twice.

mod e2e_common;

use e2e_common::*;
use std::time::Duration;

/// Issue #8, the second report, verbatim: wipe a project's `.engram/` under
/// a live core, `serve` again — the store must come back WITHOUT recycling
/// the core (the hub evicts the deleted-inode engine and reopens fresh).
#[test]
fn wiped_store_is_recreated_without_recycling_the_core() {
    let sb = Sandbox::new("wipe", 19200);
    let proj = sb.project("alpha");

    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "first serve failed: {out:?}");
    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    let core_pid = sb.core_pid().unwrap();
    assert!(proj.join(".engram/graph.tepin").exists());

    // The wipe. Recreating the bare dir is what setup/a fresh checkout give.
    std::fs::remove_dir_all(proj.join(".engram")).unwrap();
    std::fs::create_dir_all(proj.join(".engram")).unwrap();

    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "serve after wipe failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("store ready"),
        "serve confirms the fresh store: {stdout}"
    );
    assert!(
        proj.join(".engram/graph.tepin").exists(),
        "the store is back on disk"
    );
    assert_eq!(
        sb.core_pid(),
        Some(core_pid),
        "the SAME core served the fresh store — no recycle needed"
    );
    // And it serves data, not the deleted inode: a write lands and reads back.
    let id = project_id(port, "alpha").expect("alpha registered");
    http_post(
        port,
        &format!("/projects/{id}/nodes"),
        r#"{"type":"Decision","title":"fresh store accepts writes","durability":"stable","source":"claude"}"#,
    )
    .expect("write lands in the fresh store");
}

/// `stop` means stopped: when the command returns, the core PROCESS is gone
/// (locks release at process exit — health-silence alone proved nothing),
/// the port is bindable, and both advertisements are cleaned up.
#[test]
fn stop_waits_for_process_exit_and_frees_the_port() {
    let sb = Sandbox::new("stopexit", 19220);
    let proj = sb.project("alpha");

    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "serve failed: {out:?}");
    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    let pid = sb.core_pid().unwrap();

    let stop = sb.cmd(&["stop"], &proj).output().unwrap();
    assert!(stop.status.success(), "stop failed: {stop:?}");

    // No polling here on purpose: these must hold the moment stop RETURNS.
    assert!(!pid_alive(pid), "stop returned while the core still lived");
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "the port is free the moment stop returns"
    );
    assert!(!sb.home.join("daemon.json").exists());
    assert!(!proj.join(".engram/daemon.json").exists());
}

/// The update journey: a core from an "old" binary is running, the user's
/// next `serve` runs the new binary — the old core is retired, a current one
/// takes its place, and the project's data survives the changeover.
#[test]
fn serve_replaces_older_core_and_data_survives() {
    let sb = Sandbox::new("upgrade", 19240);
    let proj = sb.project("alpha");

    // Act one: the "old" installation.
    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .env("ENGRAM_TEST_VERSION", "0.0.1")
        .output()
        .unwrap();
    assert!(out.status.success(), "old-version serve failed: {out:?}");
    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    assert_eq!(
        health_version(port).as_deref(),
        Some("0.0.1"),
        "the sandbox core advertises the old version"
    );
    let old_pid = sb.core_pid().unwrap();
    let id = project_id(port, "alpha").expect("alpha registered");
    http_post(
        port,
        &format!("/projects/{id}/nodes"),
        r#"{"type":"Decision","title":"written before the update","durability":"stable","source":"claude"}"#,
    )
    .expect("node lands pre-update");

    // Act two: the binary was updated; the user runs serve.
    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "post-update serve failed: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("stopped an older engram core"),
        "serve narrates the takeover: {stderr}"
    );
    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    assert_eq!(
        health_version(port).as_deref(),
        Some(REAL_VERSION),
        "the replacement core runs the new binary"
    );
    assert_ne!(sb.core_pid(), Some(old_pid), "a new process took over");

    let id = project_id(port, "alpha").expect("alpha survives the update");
    let graph = http_get(port, &format!("/projects/{id}/graph")).unwrap();
    assert!(
        graph.contains("written before the update"),
        "pre-update data is intact: {graph}"
    );
}

/// The reverse must be a no-op: a core NEWER than the invoking binary is
/// converged on, never downgraded — an old stray binary can't demote the
/// machine core.
#[test]
fn older_binary_never_downgrades_a_newer_core() {
    let sb = Sandbox::new("nodowngrade", 19260);
    let proj = sb.project("alpha");

    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .env("ENGRAM_TEST_VERSION", "99.0.0")
        .output()
        .unwrap();
    assert!(out.status.success());
    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    let pid = sb.core_pid().unwrap();

    // This binary (REAL_VERSION < 99.0.0) plays the stale leftover install.
    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "older serve still converges: {out:?}");
    assert_eq!(
        health_version(port).as_deref(),
        Some("99.0.0"),
        "the newer core is untouched"
    );
    assert_eq!(sb.core_pid(), Some(pid), "same core, no restart");
}

/// The issue #8 tail as the field hit it: after an update, the CLIENT is
/// what comes back first — it relaunches its MCP bridge, and that bridge's
/// bind must retire the old core and bring up a current one on its own.
#[test]
fn relaunched_bridge_replaces_older_core() {
    let sb = Sandbox::new("bridgeup", 19280);
    let proj = sb.project("alpha");

    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .env("ENGRAM_TEST_VERSION", "0.0.1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    assert_eq!(health_version(port).as_deref(), Some("0.0.1"));

    // The relaunched bridge runs the updated binary (no override).
    let mut bridge = Bridge::spawn(&sb, &proj);
    // Its bind retires the old core and spawns a current one; the held
    // tools/list answers once the new core serves the session.
    bridge.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let tools = bridge.recv();
    assert!(
        tools.contains("add_note"),
        "the bridge serves tools through the replacement core: {tools}"
    );
    assert!(
        eventually(Duration::from_secs(30), || {
            sb.core_port().and_then(health_version).as_deref() == Some(REAL_VERSION)
        }),
        "the machine core now runs the new binary"
    );
    bridge.kill();
}

/// Stability churn: repeated serve/stop cycles leave no residue, and two
/// serves racing from a cold start converge on exactly one core with one
/// registration.
#[test]
fn stability_churn_and_concurrent_serves() {
    let sb = Sandbox::new("churn", 19300);
    let proj = sb.project("alpha");

    for round in 0..3 {
        let out = sb
            .cmd(&["serve", "--fake-embeddings"], &proj)
            .output()
            .unwrap();
        assert!(out.status.success(), "serve round {round} failed: {out:?}");
        sb.wait_core_healthy(CORE_HEALTH_WINDOW);
        let pid = sb.core_pid().unwrap();
        let stop = sb.cmd(&["stop"], &proj).output().unwrap();
        assert!(stop.status.success(), "stop round {round} failed: {stop:?}");
        assert!(!pid_alive(pid), "round {round}: core lingered past stop");
    }

    // Cold-start race: two serves at once.
    let mut a = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut b = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(a.wait().unwrap().success(), "racing serve A failed");
    assert!(b.wait().unwrap().success(), "racing serve B failed");

    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    let projects: serde_json::Value =
        serde_json::from_str(&http_get(port, "/projects").unwrap()).unwrap();
    let alphas = projects
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"] == "alpha")
        .count();
    assert_eq!(alphas, 1, "one registration despite the race: {projects}");

    // Exactly one core: the advertised pid lives, and no second engram core
    // holds a neighboring port with our home graph.
    let pid = sb.core_pid().unwrap();
    assert!(pid_alive(pid));
    let twins = (0..16u16)
        .filter_map(|off| {
            let p = sb.port + off;
            (p != port).then(|| http_get(p, "/health")).flatten()
        })
        .filter(|body| body.contains("home.tepin"))
        .count();
    assert_eq!(twins, 0, "no split-brain second core on nearby ports");
}
