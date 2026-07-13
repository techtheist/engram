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

fn check_store(r: &mut Report, db: &Path) {
    let store = match engram_core::Store::open(db) {
        Ok(s) => s,
        Err(e) => return r.fail(&format!("cannot open {}: {e}", db.display())),
    };
    let conn = store.conn();
    let text = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, String>(0));
    match text("PRAGMA journal_mode") {
        Ok(m) if m.eq_ignore_ascii_case("wal") => r.ok("journal_mode = wal"),
        Ok(m) => r.fail(&format!("journal_mode is {m}, expected wal")),
        Err(e) => r.fail(&format!("PRAGMA journal_mode: {e}")),
    }
    match text("PRAGMA quick_check") {
        Ok(v) if v == "ok" => r.ok("integrity quick_check passed"),
        Ok(v) => r.fail(&format!("quick_check: {v}")),
        Err(e) => r.fail(&format!("quick_check: {e}")),
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
    let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
    match (
        count("SELECT count(*) FROM nodes"),
        count("SELECT count(*) FROM vec_nodes"),
        count("SELECT count(*) FROM nodes_fts"),
    ) {
        (Ok(n), Ok(v), Ok(f)) => {
            r.ok(&format!("{n} nodes ({v} embedded, {f} in the FTS index)"));
            if v < n {
                r.warn(&format!(
                    "{} node(s) lack embeddings — semantic search misses them",
                    n - v
                ));
            }
            if f != n {
                r.warn(&format!("FTS index has {f} rows for {n} nodes"));
            }
        }
        (n, v, f) => {
            for e in [n.err(), v.err(), f.err()].into_iter().flatten() {
                r.fail(&format!("table count failed: {e}"));
            }
        }
    }
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

fn check_daemon(r: &mut Report, db: &Path) {
    let Some(dir) = db.parent() else { return };
    let file = dir.join("daemon.json");
    let Ok(raw) = std::fs::read_to_string(&file) else {
        r.note("daemon not running (no daemon.json) — `engram-alpha serve` starts the pane");
        return;
    };
    let port = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v["port"].as_u64());
    let Some(port) = port else {
        return r.fail("daemon.json is unreadable — remove it and restart `engram-alpha serve`");
    };
    match http_get(port as u16, "/health") {
        Some(body) if body.contains(&db.display().to_string()) => {
            r.ok(&format!(
                "daemon healthy on port {port}, serving this repo's DB"
            ));
        }
        Some(body) => {
            let served = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["db"].as_str().map(str::to_string))
                .unwrap_or_else(|| body.trim().to_string());
            r.fail(&format!(
                "daemon on port {port} serves a DIFFERENT db ({served}) — restart `engram-alpha serve` from this repo's root"
            ));
        }
        None => r.warn(&format!(
            "stale daemon.json (nothing healthy on port {port}) — restart `engram-alpha serve`"
        )),
    }
}

/// Minimal localhost GET. HTTP/1.0 keeps the reply un-chunked, so everything
/// after the blank line is the body.
fn http_get(port: u16, path: &str) -> Option<String> {
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

fn check_wiring(r: &mut Report, repo: &Path, db_abs: &Path) {
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
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
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
