//! Sandboxed end-to-end coverage of MCP-roots project binding (issue #4):
//! a db-less `engram-alpha mcp` binds by the client's roots (cwd fallback),
//! rebinds on roots/list_changed, and an explicit --db still pins the
//! project. Same isolation rules as process_model.rs: scratch ENGRAM_HOME +
//! HOME, fake embeddings, high ports, drop-guarded cores.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_engram-alpha");

struct Sandbox {
    root: PathBuf,
    home: PathBuf,
    port: u16,
}

impl Sandbox {
    /// `port` must be unique per test — the core walks up to 16 ports from it.
    fn new(tag: &str, port: u16) -> Self {
        let root = std::env::temp_dir().join(format!("engram-roots-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home/.engram");
        std::fs::create_dir_all(&home).unwrap();
        Self { root, home, port }
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
            .env("ENGRAM_LEASE_TTL_SECS", "3")
            .env("ENGRAM_BRIDGE_HEARTBEAT_SECS", "1")
            // A starved CI runner can stall the bridge past the real 5s
            // roots window AFTER the client already answered — these tests
            // assert binding semantics, not timeout UX, so give answered
            // requests all the time in the world. spawn_opts pins it back
            // down for the never-answering client shape.
            .env("ENGRAM_ROOTS_TIMEOUT_SECS", "120");
        c
    }

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

    /// The registered project id for `name`, if the core knows it.
    fn project_id(&self, port: u16, name: &str) -> Option<String> {
        let raw = http_get(port, "/projects")?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        v.as_array()?
            .iter()
            .find(|p| p["name"] == name)
            .and_then(|p| p["id"].as_str().map(str::to_string))
    }

    /// Whether `name`'s graph contains a node titled `title`.
    fn graph_has(&self, port: u16, name: &str, title: &str) -> bool {
        let Some(id) = self.project_id(port, name) else {
            return false;
        };
        http_get(port, &format!("/projects/{id}/graph")).is_some_and(|body| body.contains(title))
    }
}

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
        // ENGRAM_TEST_KEEP=1 keeps the sandbox (bridge-stderr.log, core.log)
        // for a post-mortem.
        if std::env::var("ENGRAM_TEST_KEEP").is_err() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

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

fn census(port: u16) -> Vec<serde_json::Value> {
    http_get(port, "/system")
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v["processes"]["clients"].as_array().cloned())
        .unwrap_or_default()
}

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

/// One stdio MCP bridge child plus a client side that can advertise the
/// roots capability, answer the bridge's roots/list requests, and record
/// whether one was ever asked.
struct Bridge {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    /// file:// URIs the client answers roots/list with.
    roots: Vec<String>,
    saw_roots_list: bool,
    /// false = the Windsurf field shape: advertise the roots capability but
    /// NEVER answer roots/list (the bridge must fall back on its timeout).
    answer_roots: bool,
}

impl Bridge {
    fn spawn(sb: &Sandbox, args: &[&str], cwd: &Path, roots: Option<Vec<String>>) -> Self {
        Self::spawn_opts(sb, args, cwd, roots, true)
    }

    fn spawn_opts(
        sb: &Sandbox,
        args: &[&str],
        cwd: &Path,
        roots: Option<Vec<String>>,
        answer_roots: bool,
    ) -> Self {
        let stderr = std::fs::File::create(sb.root.join("bridge-stderr.log")).unwrap();
        let mut cmd = sb.cmd(args, cwd);
        if !answer_roots {
            // The timeout fallback IS the behavior under test here — keep
            // the real window (the tests time their assertions against it).
            cmd.env("ENGRAM_ROOTS_TIMEOUT_SECS", "5");
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .expect("spawning the mcp bridge");
        let reader = BufReader::new(child.stdout.take().expect("bridge stdout"));
        let advertise = roots.is_some();
        // A db-less bridge asks for roots right after initialized; a real
        // client answers promptly, so the fake one must too (the bridge's
        // roots timeout would otherwise fall it back to cwd while the test
        // is busy polling the core).
        let expect_roots_request = advertise && answer_roots && !args.contains(&"--db");
        let mut bridge = Self {
            child,
            reader,
            roots: roots.clone().unwrap_or_default(),
            saw_roots_list: false,
            answer_roots,
        };
        bridge.handshake(advertise);
        if expect_roots_request {
            bridge.answer_next_roots_list();
        }
        bridge
    }

    fn handshake(&mut self, advertise_roots: bool) {
        let caps = if advertise_roots {
            r#"{"roots":{"listChanged":true}}"#
        } else {
            "{}"
        };
        self.send(&format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-03-26","capabilities":{caps},"clientInfo":{{"name":"roots-test","version":"0"}}}}}}"#
        ));
        let init = self.pump_until_response(1);
        assert!(
            init["result"]["serverInfo"].is_object(),
            "initialize reply looks wrong: {init}"
        );
        self.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    }

    fn send(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().expect("bridge stdin");
        writeln!(stdin, "{line}").expect("writing to the bridge");
        stdin.flush().unwrap();
    }

    fn recv(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .expect("reading from the bridge");
        assert!(!line.is_empty(), "bridge closed stdout");
        serde_json::from_str(&line).expect("bridge speaks JSON-RPC")
    }

    /// Read frames until the response for `id` arrives, answering any
    /// roots/list request from the bridge along the way.
    fn pump_until_response(&mut self, id: u64) -> serde_json::Value {
        loop {
            let msg = self.recv();
            if msg["method"] == "roots/list" {
                self.saw_roots_list = true;
                if !self.answer_roots {
                    continue; // the field shape: leave the request hanging
                }
                let req_id = msg["id"].clone();
                let roots: Vec<serde_json::Value> = self
                    .roots
                    .iter()
                    .map(|uri| serde_json::json!({ "uri": uri, "name": "ws" }))
                    .collect();
                self.send(
                    &serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": { "roots": roots }
                    })
                    .to_string(),
                );
                continue;
            }
            if msg["id"] == serde_json::json!(id)
                && (msg.get("result").is_some() || msg.get("error").is_some())
            {
                return msg;
            }
        }
    }

    /// Read frames until the bridge asks roots/list, then answer it.
    fn answer_next_roots_list(&mut self) {
        loop {
            let msg = self.recv();
            if msg["method"] == "roots/list" {
                let req_id = msg["id"].clone();
                let roots: Vec<serde_json::Value> = self
                    .roots
                    .iter()
                    .map(|uri| serde_json::json!({ "uri": uri, "name": "ws" }))
                    .collect();
                self.send(
                    &serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": { "roots": roots }
                    })
                    .to_string(),
                );
                self.saw_roots_list = true;
                return;
            }
        }
    }

    fn tools_list(&mut self, id: u64) -> serde_json::Value {
        self.send(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/list"}}"#
        ));
        self.pump_until_response(id)
    }

    fn add_note(&mut self, id: u64, title: &str) -> serde_json::Value {
        self.send(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "add_note",
                    "arguments": { "type": "Decision", "title": title }
                }
            })
            .to_string(),
        );
        let resp = self.pump_until_response(id);
        assert!(
            resp.get("error").is_none() && resp["result"]["isError"] != serde_json::json!(true),
            "add_note failed: {resp}"
        );
        resp
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

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn canon(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

// ---- tests ----------------------------------------------------------------

#[test]
fn dbless_bridge_binds_by_cwd() {
    let sb = Sandbox::new("cwd", 18980);
    let proj = sb.project("alpha");

    // No --db, no roots capability: the bridge must fall back to its cwd —
    // and still spawn the core when the machine has none.
    let mut bridge = Bridge::spawn(&sb, &["mcp", "--fake-embeddings"], &proj, None);
    let port = sb.wait_core_healthy(Duration::from_secs(30));

    let tools = bridge.tools_list(2).to_string();
    assert!(
        tools.contains("add_note") && tools.contains("search"),
        "bridge proxies the core's tools: {tools}"
    );
    bridge.add_note(3, "cwd binding proof note");
    assert!(
        sb.graph_has(port, "alpha", "cwd binding proof note"),
        "the note landed in the cwd project's graph"
    );
    assert!(!bridge.saw_roots_list, "no roots capability, no roots/list");
}

#[test]
fn roots_capability_binds_to_advertised_root_not_cwd() {
    let sb = Sandbox::new("roots", 19000);
    let alpha = sb.project("alpha");
    let beta = sb.project("beta");

    // cwd is alpha, but the client's roots say beta: beta wins.
    let mut bridge = Bridge::spawn(
        &sb,
        &["mcp", "--fake-embeddings"],
        &alpha,
        Some(vec![file_uri(&beta)]),
    );
    let port = sb.wait_core_healthy(Duration::from_secs(30));

    bridge.add_note(2, "roots binding proof note");
    assert!(bridge.saw_roots_list, "the bridge asked for roots");
    assert!(
        sb.graph_has(port, "beta", "roots binding proof note"),
        "the note landed in the advertised root's graph"
    );
    assert!(
        !sb.graph_has(port, "alpha", "roots binding proof note"),
        "nothing landed in the cwd project"
    );
}

#[test]
fn roots_list_changed_rebinds_and_census_follows() {
    let sb = Sandbox::new("rebind", 19020);
    let alpha = sb.project("alpha");
    let beta = sb.project("beta");

    let mut bridge = Bridge::spawn(
        &sb,
        &["mcp", "--fake-embeddings"],
        &alpha,
        Some(vec![file_uri(&alpha)]),
    );
    let port = sb.wait_core_healthy(Duration::from_secs(30));
    bridge.add_note(2, "before the switch");
    assert!(sb.graph_has(port, "alpha", "before the switch"));

    // The client switches workspaces: list_changed → the bridge re-asks →
    // we answer with beta → subsequent calls land in beta.
    bridge.roots = vec![file_uri(&beta)];
    bridge.send(r#"{"jsonrpc":"2.0","method":"notifications/roots/list_changed"}"#);
    bridge.answer_next_roots_list();

    // The census follows the rebind: exactly one lease, now rooted at beta
    // (the row moves — same lease renewed with the new root, no duplicate).
    let beta_root = canon(&beta);
    assert!(
        eventually(Duration::from_secs(10), || {
            let rows = census(port);
            rows.len() == 1 && rows[0]["root"] == serde_json::json!(beta_root)
        }),
        "census shows one lease rooted at beta: {:?}",
        census(port)
    );

    bridge.add_note(3, "after the switch");
    assert!(
        sb.graph_has(port, "beta", "after the switch"),
        "post-rebind calls land in beta"
    );
    assert!(
        !sb.graph_has(port, "alpha", "after the switch"),
        "alpha stopped receiving writes"
    );
}

/// The Windsurf field bug (JetBrains plugin, mcp-go client): the client
/// advertises roots, completes the handshake, sends tools/list — and never
/// answers our roots/list. The bridge must fall back to cwd on its 5s
/// timeout and keep serving; before the fix the session died silently.
#[test]
fn unanswered_roots_list_falls_back_to_cwd() {
    let sb = Sandbox::new("noanswer", 19060);
    let proj = sb.project("alpha");

    let mut bridge = Bridge::spawn_opts(
        &sb,
        &["mcp", "--fake-embeddings"],
        &proj,
        Some(vec![]),
        false,
    );
    let port = sb.wait_core_healthy(Duration::from_secs(30));

    let asked = Instant::now();
    let tools = bridge.tools_list(2).to_string();
    assert!(
        tools.contains("add_note"),
        "tools answered after the roots timeout: {tools}"
    );
    assert!(
        asked.elapsed() < Duration::from_secs(15),
        "the cwd fallback must ride the ~5s roots timeout, not a 30s+ stall \
         (took {:?})",
        asked.elapsed()
    );
    assert!(bridge.saw_roots_list, "the bridge did ask for roots");

    bridge.add_note(3, "unanswered roots proof note");
    assert!(
        sb.graph_has(port, "alpha", "unanswered roots proof note"),
        "the note landed in the cwd project's graph"
    );
    let alpha_root = canon(&proj);
    assert!(
        eventually(Duration::from_secs(10), || {
            let rows = census(port);
            rows.len() == 1 && rows[0]["root"] == serde_json::json!(alpha_root)
        }),
        "census shows the cwd lease: {:?}",
        census(port)
    );
}

/// A cwd that can't host a project (unwritable — the JetBrains plugin
/// launches the bridge from `/`): the bridge binds the machine core's home
/// graph instead of dying, registers the lease on the home root, and tees
/// its log into ~/.engram/mcp.log (the cwd's .engram is unreachable).
#[test]
fn unwritable_cwd_binds_home_graph() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new("homebind", 19080);
    let jail = sb.root.join("jail");
    std::fs::create_dir_all(&jail).unwrap();
    std::fs::set_permissions(&jail, std::fs::Permissions::from_mode(0o555)).unwrap();

    let mut bridge = Bridge::spawn(&sb, &["mcp", "--fake-embeddings"], &jail, None);
    let port = sb.wait_core_healthy(Duration::from_secs(30));

    let tools = bridge.tools_list(2).to_string();
    assert!(
        tools.contains("add_note"),
        "tools answer against the home graph: {tools}"
    );
    bridge.add_note(3, "home graph fallback proof note");
    assert!(
        http_get(port, "/projects/home/graph")
            .is_some_and(|g| g.contains("home graph fallback proof note")),
        "the note landed in the HOME graph"
    );

    let home_root = sb.home.display().to_string();
    assert!(
        eventually(Duration::from_secs(10), || {
            let rows = census(port);
            rows.len() == 1
                && rows[0]["root"] == serde_json::json!(home_root)
                && rows[0]["kind"] == serde_json::json!("mcp-bridge")
        }),
        "census shows one mcp-bridge lease rooted at the engram home: {:?}",
        census(port)
    );
    assert!(
        sb.home.join("mcp.log").is_file(),
        "the log teed into ~/.engram/mcp.log (the cwd is unwritable)"
    );

    std::fs::set_permissions(&jail, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// After a home-graph fallback, a roots/list_changed naming a real project
/// must rebind the session away from home — the IDE finally telling us the
/// workspace beats the unusable launch cwd.
#[test]
fn list_changed_rebinds_home_to_project() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new("homerebind", 19100);
    let jail = sb.root.join("jail");
    std::fs::create_dir_all(&jail).unwrap();
    std::fs::set_permissions(&jail, std::fs::Permissions::from_mode(0o555)).unwrap();
    let beta = sb.project("beta");

    // Advertises roots but answers an EMPTY list: cwd fallback → unwritable
    // → home graph.
    let mut bridge = Bridge::spawn(&sb, &["mcp", "--fake-embeddings"], &jail, Some(vec![]));
    let port = sb.wait_core_healthy(Duration::from_secs(30));
    bridge.add_note(2, "note born on the home graph");
    assert!(
        http_get(port, "/projects/home/graph")
            .is_some_and(|g| g.contains("note born on the home graph")),
        "pre-rebind writes land in the home graph"
    );

    bridge.roots = vec![file_uri(&beta)];
    bridge.send(r#"{"jsonrpc":"2.0","method":"notifications/roots/list_changed"}"#);
    bridge.answer_next_roots_list();

    let beta_root = canon(&beta);
    assert!(
        eventually(Duration::from_secs(10), || {
            let rows = census(port);
            rows.len() == 1 && rows[0]["root"] == serde_json::json!(beta_root)
        }),
        "census follows the home→project rebind: {:?}",
        census(port)
    );
    bridge.add_note(3, "note after leaving home");
    assert!(
        sb.graph_has(port, "beta", "note after leaving home"),
        "post-rebind writes land in the project graph"
    );
    assert!(
        !http_get(port, "/projects/home/graph")
            .is_some_and(|g| g.contains("note after leaving home")),
        "the home graph stopped receiving writes"
    );

    std::fs::set_permissions(&jail, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The machine-level backup setting (the Windsurf field case): when a db-less
/// session has no roots answer AND an unusable launch cwd, it binds the
/// configured `default_agent_project` instead of the home graph — and the
/// census says which client (clientInfo.name) bound there. Also covers the
/// /settings roundtrip + validation against a real core.
#[test]
fn default_agent_project_binds_unbindable_session() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new("agentdefault", 19120);
    let gamma = sb.project("gamma");

    // Register gamma with the core the way a user would: one `serve` run.
    let status = sb
        .cmd(&["serve", "--fake-embeddings"], &gamma)
        .status()
        .expect("running serve");
    assert!(status.success(), "serve registers the project");
    let port = sb.wait_core_healthy(Duration::from_secs(30));
    let gamma_id = sb
        .project_id(port, "gamma")
        .expect("serve put gamma on the registry");

    // /settings roundtrip + validation.
    assert!(
        http_post(
            port,
            "/settings",
            r#"{"default_agent_project":"no-such-project"}"#
        )
        .is_none(),
        "an unknown project is rejected (400), not stored"
    );
    let unset = http_get(port, "/settings").expect("GET /settings answers on a core");
    let unset: serde_json::Value = serde_json::from_str(&unset).unwrap();
    assert!(
        unset["default_agent_project"].is_null(),
        "a rejected write left the setting unset: {unset}"
    );
    let stored = http_post(port, "/settings", r#"{"default_agent_project":"gamma"}"#)
        .expect("setting a registered project by name succeeds");
    let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(
        stored["default_agent_project"],
        serde_json::json!(gamma_id),
        "stored canonically as the project id"
    );
    assert_eq!(
        stored["default_agent_project_name"],
        serde_json::json!("gamma")
    );
    let read_back = http_get(port, "/settings").unwrap();
    let read_back: serde_json::Value = serde_json::from_str(&read_back).unwrap();
    assert_eq!(
        read_back["default_agent_project"],
        serde_json::json!(gamma_id),
        "the setting survives to the next read: {read_back}"
    );

    // The Windsurf shape: unwritable launch cwd, roots advertised but never
    // answered. cwd rung fails → the setting is the pre-home rung.
    let jail = sb.root.join("jail");
    std::fs::create_dir_all(&jail).unwrap();
    std::fs::set_permissions(&jail, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut bridge = Bridge::spawn_opts(
        &sb,
        &["mcp", "--fake-embeddings"],
        &jail,
        Some(vec![]),
        false,
    );

    let tools = bridge.tools_list(2).to_string();
    assert!(
        tools.contains("add_note"),
        "tools answer against the configured project: {tools}"
    );
    bridge.add_note(3, "default agent project proof note");
    assert!(
        sb.graph_has(port, "gamma", "default agent project proof note"),
        "the note landed in the CONFIGURED project's graph"
    );
    assert!(
        !http_get(port, "/projects/home/graph")
            .is_some_and(|g| g.contains("default agent project proof note")),
        "nothing landed in the home graph"
    );

    // Census legibility: the lease sits on gamma's root and names the MCP
    // client (clientInfo.name from the stdio initialize).
    let gamma_root = canon(&gamma);
    assert!(
        eventually(Duration::from_secs(10), || {
            let rows = census(port);
            rows.len() == 1
                && rows[0]["root"] == serde_json::json!(gamma_root)
                && rows[0]["client"] == serde_json::json!("roots-test")
        }),
        "census shows one lease on gamma naming the client: {:?}",
        census(port)
    );

    std::fs::set_permissions(&jail, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// An unspawnable core (unwritable ENGRAM_HOME: the spawned `core` child
/// dies immediately) must not leave the stdio client staring at silence:
/// the bridge answers initialize promptly (handshake-first — before the fix
/// it sat in spawn_core_and_wait's 180s health poll and impatient clients
/// like Windsurf's mcp-go killed it at ~60s) and the first tool call gets a
/// real JSON-RPC error through the fail path, not a hang.
#[test]
fn unspawnable_core_answers_initialize_then_errors_tools() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new("nospawn", 19140);
    let proj = sb.project("alpha");
    // The core child can't create its home state (daemon.json, home store)
    // — it exits at startup, every time.
    std::fs::set_permissions(&sb.home, std::fs::Permissions::from_mode(0o555)).unwrap();

    let started = Instant::now();
    // Bridge::spawn drives the initialize/initialized handshake and asserts
    // the reply — reaching past it IS the handshake-first proof.
    let mut bridge = Bridge::spawn(&sb, &["mcp", "--fake-embeddings"], &proj, None);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "initialize must answer before any core wait, not after it \
         (took {:?})",
        started.elapsed()
    );

    let resp = bridge.tools_list(2);
    assert!(
        resp.get("error").is_some(),
        "the first tool call reports the dead core as a JSON-RPC error: {resp}"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("engram")),
        "the error names the bridge/core failure, not a transport artifact: {resp}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "the failure surfaces well inside an impatient client's ~60s budget \
         (took {:?})",
        started.elapsed()
    );

    std::fs::set_permissions(&sb.home, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn explicit_db_wins_over_roots() {
    let sb = Sandbox::new("dbwins", 19040);
    let alpha = sb.project("alpha");
    let beta = sb.project("beta");

    // --db pins alpha even though the client advertises roots naming beta.
    let mut bridge = Bridge::spawn(
        &sb,
        &["mcp", "--db", ".engram/graph.db", "--fake-embeddings"],
        &alpha,
        Some(vec![file_uri(&beta)]),
    );
    let port = sb.wait_core_healthy(Duration::from_secs(30));

    bridge.tools_list(2);
    bridge.add_note(3, "explicit db proof note");
    assert!(
        !bridge.saw_roots_list,
        "an explicit --db never asks for roots"
    );
    assert!(
        sb.graph_has(port, "alpha", "explicit db proof note"),
        "the note landed in the --db project"
    );
    assert!(
        sb.project_id(port, "beta").is_none(),
        "the advertised root was never registered"
    );
}
