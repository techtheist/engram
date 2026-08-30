//! Shared harness for the e2e suites (0.8.13): the process_model.rs sandbox
//! pattern — scratch ENGRAM_HOME + HOME, fake embeddings, high ports, drop
//! guard — plus the pieces the lifecycle/wiring journeys need (per-command
//! env, pid liveness, /health version reads, config-driven bridges).
//!
//! Deliberately a copy, not a refactor: process_model.rs is a watched-flaky
//! suite and stays untouched.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_engram-alpha");

/// The workspace version this test binary was built with — what a takeover
/// must leave advertised on /health.
pub const REAL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One shared patience window for "the spawned core answers /health" —
/// same generosity as process_model.rs (starved CI runners taught it).
pub const CORE_HEALTH_WINDOW: Duration = Duration::from_secs(120);

pub struct Sandbox {
    pub root: PathBuf,
    pub home: PathBuf,
    pub port: u16,
}

impl Sandbox {
    /// `port` must be unique per test — the core walks up to 16 ports from it.
    pub fn new(tag: &str, port: u16) -> Self {
        let root = std::env::temp_dir().join(format!("engram-e2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home/.engram");
        std::fs::create_dir_all(&home).unwrap();
        Self { root, home, port }
    }

    pub fn project(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(dir.join(".engram")).unwrap();
        dir
    }

    pub fn cmd(&self, args: &[&str], cwd: &Path) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(cwd)
            .env("ENGRAM_HOME", &self.home)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("ENGRAM_HTTP_PORT", self.port.to_string())
            .env("ENGRAM_UPDATE_CHECK", "0")
            // The sealing key must come from the sandboxed file fallback —
            // a keyring hit would prompt (macOS) or leak state between runs.
            .env("ENGRAM_KEYRING", "off");
        c
    }

    pub fn core_port(&self) -> Option<u16> {
        let raw = std::fs::read_to_string(self.home.join("daemon.json")).ok()?;
        Some(serde_json::from_str::<serde_json::Value>(&raw).ok()?["port"].as_u64()? as u16)
    }

    pub fn core_pid(&self) -> Option<u32> {
        let raw = std::fs::read_to_string(self.home.join("daemon.json")).ok()?;
        Some(serde_json::from_str::<serde_json::Value>(&raw).ok()?["pid"].as_u64()? as u32)
    }

    pub fn wait_core_healthy(&self, timeout: Duration) -> u16 {
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
pub fn http_get(port: u16, path: &str) -> Option<String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.split_once("\r\n\r\n").map(|(_, b)| b.to_string())
}

pub fn http_post(port: u16, path: &str, body: &str) -> Option<String> {
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

/// The version the core on `port` advertises.
pub fn health_version(port: u16) -> Option<String> {
    let body = http_get(port, "/health")?;
    serde_json::from_str::<serde_json::Value>(&body).ok()?["version"]
        .as_str()
        .map(str::to_string)
}

/// Is the process alive? (`kill -0` — unix only, like the suite.)
pub fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Poll until `pred` holds or the timeout runs out; returns whether it held.
pub fn eventually(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// The registered project id for `name`, if the core knows it.
pub fn project_id(port: u16, name: &str) -> Option<String> {
    let raw = http_get(port, "/projects")?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.as_array()?
        .iter()
        .find(|p| p["name"] == name)
        .and_then(|p| p["id"].as_str().map(str::to_string))
}

/// One stdio MCP child plus the plumbing to speak line-delimited JSON-RPC
/// with it. `spawn_cmd` takes any prepared Command, so a bridge can be
/// launched exactly as a wiring config describes it.
pub struct Bridge {
    pub child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl Bridge {
    pub fn spawn_cmd(mut cmd: Command) -> Self {
        let mut child = cmd
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

    pub fn spawn(sb: &Sandbox, project: &Path) -> Self {
        Self::spawn_cmd(sb.cmd(
            &["mcp", "--db", ".engram/graph.db", "--fake-embeddings"],
            project,
        ))
    }

    fn handshake(&mut self) {
        self.send(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"e2e-test","version":"0"}}}"#,
        );
        let init = self.recv();
        assert!(
            init.contains("serverInfo"),
            "initialize reply looks wrong: {init}"
        );
        self.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    }

    pub fn send(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().expect("bridge stdin");
        writeln!(stdin, "{line}").expect("writing to the bridge");
        stdin.flush().unwrap();
    }

    pub fn recv(&mut self) -> String {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .expect("reading from the bridge");
        assert!(!line.is_empty(), "bridge closed stdout");
        line
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.kill();
    }
}
