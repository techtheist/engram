//! `engram-alpha doctor` — diagnose this repo's Engram installation: store
//! integrity, embedding model presence, daemon-vs-DB path match (the
//! wrong-cwd empty-DB failure class, PLAN §10), and per-assistant wiring.

use std::path::Path;
use std::time::Duration;

use crate::setup;
use engram_core::harness::{prerename_codex_toml, prerename_mcp_json};

#[derive(Default)]
struct Report {
    failures: usize,
    warnings: usize,
}

impl Report {
    fn ok(&mut self, msg: &str) {
        println!("   ok   {msg}");
    }
    /// Informational — a normal state worth naming, counts as neither.
    fn note(&mut self, msg: &str) {
        println!("   --   {msg}");
    }
    fn warn(&mut self, msg: &str) {
        self.warnings += 1;
        println!("  warn  {msg}");
    }
    fn fail(&mut self, msg: &str) {
        self.failures += 1;
        println!("  FAIL  {msg}");
    }
}

pub fn run(db: &Path) -> anyhow::Result<()> {
    let repo = std::env::current_dir()?;
    let db_abs = if db.is_absolute() {
        db.to_path_buf()
    } else {
        repo.join(db)
    };
    let db_canon = std::fs::canonicalize(&db_abs).unwrap_or_else(|_| db_abs.clone());
    let mut r = Report::default();

    println!("graph store");
    if db_canon.is_file() {
        check_store(&mut r, &db_canon);
    } else {
        r.warn(&format!(
            "no graph at {} — `engram-alpha serve` (from the repo root) creates it",
            db_abs.display()
        ));
    }

    println!("embedding model");
    check_model(&mut r);

    println!("daemon");
    check_daemon(&mut r, &db_canon);

    println!("wiring");
    check_wiring(&mut r, &repo, &db_abs);

    println!("version");
    check_version(&mut r);

    println!();
    if r.failures > 0 {
        anyhow::bail!("{} failure(s), {} warning(s)", r.failures, r.warnings);
    }
    if r.warnings > 0 {
        println!("{} warning(s), no failures", r.warnings);
    } else {
        println!("all checks passed");
    }
    Ok(())
}

/// Always ask GitHub whether a newer release is published — doctor is an
/// explicit diagnostic gesture, so the network round-trip is expected here
/// (`serve` throttles the same query to once a day). An unreachable GitHub
/// is a note, not a warning: offline is a supported condition.
fn check_version(r: &mut Report) {
    match crate::update::newer_release() {
        Ok(Some(tag)) => r.warn(&format!(
            "this binary is v{} but {tag} is published — run `engram-alpha update`",
            env!("CARGO_PKG_VERSION")
        )),
        Ok(None) => r.ok(&format!(
            "v{} is the newest published release",
            env!("CARGO_PKG_VERSION")
        )),
        Err(e) => r.note(&format!("update check unavailable ({e})")),
    }
}

fn check_store(r: &mut Report, db: &Path) {
    // Thin client (PLAN §7C): when a healthy daemon owns this store — on
    // TepinDB it is the ONLY process allowed to — read the same facts from
    // its /system instead of opening the file underneath it.
    if daemon_store_report(r, db) {
        return;
    }
    // Ask the core BEFORE touching the file (issue #6): a store the core
    // holds open must never be direct-opened underneath it — on tepin the
    // open would fail on the core's lock and mis-report health as failure.
    if core_owns(db) {
        return r.ok(
            "store is held open by the machine core (healthy) — it serves this repo's pane and MCP",
        );
    }
    let store = match engram_core::open_store(db) {
        Ok(s) => s,
        Err(e) => return r.fail(&format!("cannot open {}: {e}", db.display())),
    };
    match store.health() {
        Ok(h) => {
            match h.journal_mode.as_deref() {
                Some(m) if m.eq_ignore_ascii_case("wal") => r.ok("journal_mode = wal"),
                Some(m) => r.fail(&format!("journal_mode is {m}, expected wal")),
                None => r.ok("backend has no journal mode (redb: fsync-per-commit)"),
            }
            if h.integrity_ok {
                r.ok("store integrity check passed");
            } else {
                r.fail(&format!(
                    "integrity check: {}",
                    h.detail.as_deref().unwrap_or("failed")
                ));
            }
        }
        Err(e) => r.fail(&format!("store health: {e}")),
    }
    match store.embed_version() {
        Ok(v) if v >= engram_core::EMBED_COMPOSITION => {
            r.ok("embeddings use the current composition (title/body/tags/code_refs)");
        }
        Ok(_) => r.warn(
            "stored embeddings predate the full-field composition — start `engram-alpha serve` (real embeddings) once to reindex",
        ),
        Err(e) => r.fail(&format!("embed version: {e}")),
    }
    match store.stats() {
        Ok(s) => {
            r.ok(&format!(
                "{} nodes ({} embedded) on the {} backend",
                s.nodes, s.embedded, s.backend
            ));
            if s.embedded < s.nodes {
                r.warn(&format!(
                    "{} node(s) lack embeddings — semantic search misses them",
                    s.nodes - s.embedded
                ));
            }
        }
        Err(e) => r.fail(&format!("store stats failed: {e}")),
    }
}

/// Does the machine core have this store registered and open right now?
/// (`/projects` reports `db` + `open` per project — `open:true` means the
/// core's hub holds the engine, and with it a tepin store's exclusive lock.)
/// Compare the stores the paths OPEN, not the strings (the 0.8.5 lesson):
/// the registry may say `graph.db` while both processes resolve and hold
/// `graph.tepin`.
fn core_owns(db: &Path) -> bool {
    let Some(port) = crate::machine_core() else {
        return false;
    };
    let Some(projects) = http_get(port, "/projects")
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
    else {
        return false;
    };
    let canon = |p: &Path| {
        let r = engram_core::resolve_db_path(p);
        std::fs::canonicalize(&r).unwrap_or(r)
    };
    let target = canon(db);
    projects.as_array().is_some_and(|list| {
        list.iter().any(|p| {
            p["open"].as_bool() == Some(true)
                && p["db"]
                    .as_str()
                    .is_some_and(|d| canon(Path::new(d)) == target)
        })
    })
}

/// The daemon-served version of `check_store`, reporting from `/system` so
/// doctor never contends for the file. Returns whether it handled the check.
fn daemon_store_report(r: &mut Report, db: &Path) -> bool {
    let Some(port) = crate::daemon_for(db) else {
        return false;
    };
    let Some(system) = http_get(port, "/system")
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
    else {
        return false;
    };
    let store = &system["store"];
    let backend = store["backend"].as_str().unwrap_or("unknown");
    match store["journal_mode"].as_str().unwrap_or_default() {
        "" => r.ok("backend has no journal mode (redb: fsync-per-commit)"),
        m if m.eq_ignore_ascii_case("wal") => r.ok("journal_mode = wal"),
        m => r.fail(&format!("journal_mode is {m}, expected wal")),
    }
    if store["integrity_ok"].as_bool() == Some(true) {
        r.ok("store integrity check passed (via the owning daemon)");
    } else {
        r.fail("integrity check failed (via the owning daemon)");
    }
    if store["embed_composition_current"].as_bool() == Some(true) {
        r.ok("embeddings use the current composition (title/body/tags/code_refs)");
    } else {
        r.warn("stored embeddings predate the full-field composition — restart the daemon with real embeddings to reindex");
    }
    let (nodes, embedded) = (
        store["nodes"].as_i64().unwrap_or(-1),
        store["embedded"].as_i64().unwrap_or(-1),
    );
    r.ok(&format!(
        "{nodes} nodes ({embedded} embedded) on the {backend} backend"
    ));
    if embedded >= 0 && embedded < nodes {
        r.warn(&format!(
            "{} node(s) lack embeddings — semantic search misses them",
            nodes - embedded
        ));
    }
    true
}

fn check_model(r: &mut Report) {
    let cached = setup::home_file(".cache/engram").is_some_and(|dir| {
        std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_some())
    });
    if cached {
        r.ok("local embedding model cached (~/.cache/engram)");
    } else {
        r.note("model not downloaded yet — the first real-embedding run fetches it (~30 MB)");
    }
}

/// The daemon is the MACHINE CORE (0.8.8 process model): find it via
/// `~/.engram/daemon.json` + /health, regardless of the cwd this doctor runs
/// from and regardless of how the core was started (issue #6 — the old check
/// read only the repo-local file and mis-called an auto-spawned core "a
/// daemon serving a different db"). A repo-local daemon.json is only an
/// advertisement pointer now; a legacy per-repo daemon still counts.
fn check_daemon(r: &mut Report, db: &Path) {
    if let Some(port) = crate::machine_core() {
        r.ok(&format!(
            "machine core healthy on port {port} — pane: http://127.0.0.1:{port}"
        ));
        return;
    }
    // Transitional: a pre-0.8.8 per-repo daemon owning this store.
    if let Some(port) = crate::daemon_for(db) {
        r.ok(&format!(
            "daemon healthy on port {port}, serving this repo's DB"
        ));
        return;
    }
    // Nothing healthy anywhere: name a stale advertisement when one exists.
    let stale_repo_file = db
        .parent()
        .is_some_and(|dir| dir.join("daemon.json").exists());
    if stale_repo_file {
        r.warn("stale .engram/daemon.json (nothing healthy advertised) — `engram-alpha serve` restarts the core and refreshes it");
    } else {
        r.note("core not running — `engram-alpha serve` starts it (and the pane)");
    }
}

/// Minimal localhost POST (HTTP/1.0, JSON body) — enough to register a repo
/// with a running core. Returns the response body on any 2xx.
pub(crate) fn http_post(port: u16, path: &str, json_body: &str) -> Option<String> {
    http_post_timeout(port, path, json_body, Duration::from_secs(5))
}

/// POST with a caller-chosen read timeout — an import re-embeds every node
/// under the daemon's engine lock, which outlives the quick-probe default.
pub(crate) fn http_post_timeout(
    port: u16,
    path: &str,
    json_body: &str,
    read_timeout: Duration,
) -> Option<String> {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(read_timeout)).ok()?;
    write!(
        stream,
        "POST {path} HTTP/1.0\r\nHost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{json_body}",
        json_body.len()
    )
    .ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let ok = text.starts_with("HTTP/1.0 2") || text.starts_with("HTTP/1.1 2");
    ok.then(|| {
        text.split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default()
    })
}

/// Minimal localhost GET. HTTP/1.0 keeps the reply un-chunked, so everything
/// after the blank line is the body. Shared with the thin-client resolution
/// in main.rs (mcp bridge, daemon-aware brief).
pub(crate) fn http_get(port: u16, path: &str) -> Option<String> {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
}

/// Corporate proxies intercept loopback HTTP unless excluded (issue #2). Our
/// own bridge client opts out of proxies entirely, but other local MCP/HTTP
/// clients on this machine (IDE plugins, other assistants) follow the env.
fn check_proxy_env(r: &mut Report) {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let proxy_set = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .iter()
    .find_map(|k| get(k).map(|_| *k));
    let Some(var) = proxy_set else { return };
    let no_proxy = get("NO_PROXY")
        .or_else(|| get("no_proxy"))
        .unwrap_or_default();
    let loopback_excluded = no_proxy.split(',').map(str::trim).any(|e| {
        e == "*" || e == "127.0.0.1" || e == "localhost" || e == "loopback" || e == "127.0.0.0/8"
    });
    if loopback_excluded {
        r.ok(&format!("{var} is set and NO_PROXY excludes loopback"));
    } else {
        r.warn(&format!(
            "{var} is set without a NO_PROXY loopback exclusion — engram-alpha's own MCP bridge \
             ignores proxies, but other local clients may route 127.0.0.1 through the proxy; \
             consider NO_PROXY=127.0.0.1,localhost"
        ));
    }
}

fn check_wiring(r: &mut Report, repo: &Path, db_abs: &Path) {
    check_proxy_env(r);
    let gitignored = std::fs::read_to_string(repo.join(".gitignore")).is_ok_and(|s| {
        s.lines()
            .any(|l| matches!(l.trim(), ".engram/" | ".engram"))
    });
    if gitignored {
        r.ok(".engram/ is git-ignored");
    } else {
        r.warn(".engram/ is not in .gitignore — the graph is personal, keep it out of the repo");
    }

    let detected = setup::detect_agents();
    if detected.is_empty() {
        r.note("no supported assistants detected on this machine");
        return;
    }
    for agent in detected {
        match agent {
            "claude" => check_claude(r, repo, db_abs),
            "codex" => check_codex(r),
            other => {
                if setup::is_wired(repo, other) {
                    r.ok(&format!("{other}: wired"));
                } else {
                    r.warn(&format!(
                        "{other}: detected but not wired — `engram-alpha setup --cli {other}`"
                    ));
                }
            }
        }
    }
}

fn check_claude(r: &mut Report, repo: &Path, db_abs: &Path) {
    match std::fs::read_to_string(repo.join(".mcp.json")) {
        Err(_) => r.warn("claude: no .mcp.json — `engram-alpha setup --cli claude`"),
        Ok(raw) => {
            let problems = mcp_json_problems(&raw, db_abs);
            if problems.is_empty() {
                r.ok("claude: .mcp.json registers this repo's graph");
            }
            for p in problems {
                r.fail(&format!("claude: .mcp.json {p}"));
            }
            if prerename_mcp_json(&raw) {
                r.warn(
                    "claude: .mcp.json launches the pre-rename `engram` binary — re-run `engram-alpha setup --cli claude --mcp-only` to re-point it (pre-rename support ended in v0.5.0)",
                );
            }
        }
    }
    let hook = [".claude/settings.json", ".claude/settings.local.json"]
        .iter()
        .any(|p| {
            std::fs::read_to_string(repo.join(p))
                .is_ok_and(|s| s.contains("engram-brief") || s.contains("session-brief"))
        });
    if hook {
        r.ok("claude: session-brief hook registered");
    } else {
        r.note(
            "claude: no repo-level brief hook (fine if the Engram Claude Code plugin provides it)",
        );
    }
}

/// Problems with a `.mcp.json` engram entry; empty = healthy.
fn mcp_json_problems(raw: &str, db_abs: &Path) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return vec!["is not valid JSON".into()];
    };
    let Some(entry) = v.get("mcpServers").and_then(|s| s.get("engram")) else {
        return vec!["has no engram server — `engram-alpha setup --cli claude`".into()];
    };
    let mut problems = Vec::new();
    match entry["command"].as_str() {
        Some(cmd) if Path::new(cmd).is_absolute() && !Path::new(cmd).exists() => {
            problems.push(format!(
                "command points at a missing binary ({cmd}) — re-run `engram-alpha setup --cli claude`"
            ));
        }
        Some(cmd) if !Path::new(cmd).is_absolute() && !setup::on_path(cmd) => {
            problems.push(format!("command `{cmd}` is not on PATH"));
        }
        Some(_) => {}
        None => problems.push("engram entry has no command".into()),
    }
    let args: Vec<&str> = entry["args"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    if let Some(db_arg) = args
        .iter()
        .position(|a| *a == "--db")
        .and_then(|i| args.get(i + 1))
    {
        // Compare the stores the paths OPEN, not the strings: setup used to
        // write graph.db and every command resolves it to graph.tepin, so a
        // .db config on a migrated repo is healthy, not a wrong graph.
        let canon = |p: &Path| {
            let r = engram_core::resolve_db_path(p);
            std::fs::canonicalize(&r).unwrap_or(r)
        };
        let arg_path = Path::new(db_arg);
        if arg_path.is_absolute() && canon(arg_path) != canon(db_abs) {
            problems.push(format!("--db points at {db_arg}, not this repo's graph"));
        }
    }
    problems
}

fn check_codex(r: &mut Report) {
    let wired = setup::home_file(".codex/config.toml")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .filter(|raw| raw.contains("[mcp_servers.engram]"));
    let Some(raw) = wired else {
        return r.warn("codex: detected but not wired — `engram-alpha setup --cli codex`");
    };
    r.ok("codex: engram registered in ~/.codex/config.toml");
    if prerename_codex_toml(&raw) {
        r.warn(
            "codex: ~/.codex/config.toml launches the pre-rename `engram` binary — re-run `engram-alpha setup --cli codex` to re-point it",
        );
    }
    if codex_entry_cwd_dependent(&raw) {
        r.note(
            "codex: the entry resolves --db against the launch cwd — fine for the CLI started \
             in the repo root; the Codex/ChatGPT desktop app may launch elsewhere, so pin \
             `cwd = \"<repo>\"` or an absolute --db there if you use the app",
        );
    }
}

/// Does the `[mcp_servers.engram]` table rely on the launch cwd — no `cwd`
/// key and no pinned `--db`? (`setup` only ever writes absolute --db paths.)
fn codex_entry_cwd_dependent(toml: &str) -> bool {
    let Some(start) = toml.find("[mcp_servers.engram]") else {
        return false;
    };
    let section: Vec<&str> = toml[start..]
        .lines()
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .collect();
    !section
        .iter()
        .any(|l| l.trim_start().starts_with("cwd") || l.contains("--db"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn db() -> PathBuf {
        PathBuf::from("/repo/.engram/graph.db")
    }

    #[test]
    fn healthy_mcp_json_has_no_problems() {
        let raw = r#"{"mcpServers":{"engram":{"command":"/bin/sh","args":["mcp","--db","/repo/.engram/graph.db"]}}}"#;
        assert!(mcp_json_problems(raw, &db()).is_empty());
    }

    #[test]
    fn legacy_graph_db_config_matches_the_resolved_tepin_store() {
        // setup wrote graph.db for every 0.7.x repo; the store it opens is
        // graph.tepin — doctor must not flag its own generated config.
        let raw = r#"{"mcpServers":{"engram":{"command":"/bin/sh","args":["mcp","--db","/repo/.engram/graph.db"]}}}"#;
        let tepin = PathBuf::from("/repo/.engram/graph.tepin");
        assert!(mcp_json_problems(raw, &tepin).is_empty());
    }

    #[test]
    fn wrong_db_and_missing_binary_are_flagged() {
        let raw = r#"{"mcpServers":{"engram":{"command":"/nonexistent/engram-alpha","args":["mcp","--db","/other/.engram/graph.db"]}}}"#;
        let problems = mcp_json_problems(raw, &db());
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("missing binary"));
        assert!(problems[1].contains("/other/.engram/graph.db"));
    }

    #[test]
    fn missing_entry_and_bad_json_are_flagged() {
        assert_eq!(mcp_json_problems("{}", &db()).len(), 1);
        assert_eq!(mcp_json_problems("not json", &db()).len(), 1);
    }

    #[test]
    fn prerename_wiring_detection() {
        let json = r#"{"mcpServers":{"engram":{"command":"/x/engram","args":["mcp"]}}}"#;
        assert!(prerename_mcp_json(json));
        let json = r#"{"mcpServers":{"engram":{"command":"/x/engram-alpha","args":["mcp"]}}}"#;
        assert!(!prerename_mcp_json(json));

        let toml = "[mcp_servers.engram]\ncommand = \"/x/engram\"\nargs = [\"mcp\"]\n";
        assert!(prerename_codex_toml(toml));
        let toml = "[mcp_servers.other]\ncommand = \"/x/engram\"\n[mcp_servers.engram]\ncommand = \"/x/engram-alpha\"\n";
        assert!(!prerename_codex_toml(toml));
    }

    #[test]
    fn codex_cwd_dependence() {
        let bare = "[mcp_servers.engram]\ncommand = \"engram-alpha\"\nargs = [\"mcp\"]\n";
        assert!(codex_entry_cwd_dependent(bare));
        let pinned_cwd =
            "[mcp_servers.engram]\ncommand = \"engram-alpha\"\nargs = [\"mcp\"]\ncwd = \"/repo\"\n";
        assert!(!codex_entry_cwd_dependent(pinned_cwd));
        let pinned_db = "[mcp_servers.engram]\ncommand = \"engram-alpha\"\nargs = [\"mcp\", \"--db\", \"/repo/.engram/graph.db\"]\n";
        assert!(!codex_entry_cwd_dependent(pinned_db));
        let other_section =
            "[mcp_servers.engram]\nargs = [\"mcp\"]\n[mcp_servers.other]\ncwd = \"/x\"\n";
        assert!(codex_entry_cwd_dependent(other_section));
    }
}
