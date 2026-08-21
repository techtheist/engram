//! `engram-alpha setup --cli windsurf` writes BOTH global Windsurf configs
//! — `${XDG_CONFIG_HOME:-~/.config}/devin/mcp_config.json` (the JetBrains
//! plugin generation reads this one) and `~/.devin/mcp_config.json` (the
//! older generation) — each with the same db-less merge semantics: the
//! bridge binds by MCP roots, so one entry serves every project (issue #4).
//! Sandboxed HOME/ENGRAM_HOME; no core is started (setup never talks to
//! one).

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_engram-alpha");

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("engram-windsurf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo")).unwrap();
        Self { root }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    /// The XDG-generation config (what the JetBrains plugin reads).
    fn xdg_path(&self) -> PathBuf {
        self.root.join("home/.config/devin/mcp_config.json")
    }

    /// The legacy-generation config.
    fn legacy_path(&self) -> PathBuf {
        self.root.join("home/.devin/mcp_config.json")
    }

    fn run_setup(&self) -> std::process::Output {
        Command::new(BIN)
            .args(["setup", "--cli", "windsurf", "--mcp-only"])
            .current_dir(self.repo())
            .env("HOME", self.root.join("home"))
            .env("ENGRAM_HOME", self.root.join("home/.engram"))
            .env("ENGRAM_UPDATE_CHECK", "0")
            // The adapter falls back to ~/.config when XDG_CONFIG_HOME is
            // unset — pin the fallback so the host environment can't leak in.
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .expect("running setup")
    }

    fn json_at(&self, path: &PathBuf) -> serde_json::Value {
        let raw = std::fs::read_to_string(path).expect("global config exists");
        serde_json::from_str(&raw).expect("global config is valid JSON")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn assert_dbless_engram_entry(v: &serde_json::Value) {
    let entry = &v["mcpServers"]["engram"];
    assert_eq!(
        entry["command"],
        serde_json::json!(BIN),
        "command is this binary: {v}"
    );
    assert_eq!(
        entry["args"],
        serde_json::json!(["mcp"]),
        "the global entry is db-less — roots/cwd pick the project: {v}"
    );
}

#[test]
fn writes_both_global_configs() {
    let sb = Sandbox::new("fresh");
    let out = sb.run_setup();
    assert!(out.status.success(), "setup failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("wrote ~/.config/devin/mcp_config.json")
            && stdout.contains("wrote ~/.devin/mcp_config.json"),
        "reports both writes (two plugin generations, two config homes):\n{stdout}"
    );
    assert_dbless_engram_entry(&sb.json_at(&sb.xdg_path()));
    assert_dbless_engram_entry(&sb.json_at(&sb.legacy_path()));
}

#[test]
fn merges_into_existing_config_preserving_other_servers() {
    let sb = Sandbox::new("merge");
    // Seed only the legacy file — the XDG one starts absent, proving each
    // file gets its own independent merge.
    std::fs::create_dir_all(sb.legacy_path().parent().unwrap()).unwrap();
    std::fs::write(
        sb.legacy_path(),
        r#"{"mcpServers":{"other":{"command":"/usr/bin/other","args":["--x"]}},"theme":"dark"}"#,
    )
    .unwrap();

    let out = sb.run_setup();
    assert!(out.status.success(), "setup failed: {out:?}");
    let v = sb.json_at(&sb.legacy_path());
    assert_dbless_engram_entry(&v);
    assert_eq!(
        v["mcpServers"]["other"]["command"],
        serde_json::json!("/usr/bin/other"),
        "existing servers survive the merge: {v}"
    );
    assert_eq!(
        v["theme"],
        serde_json::json!("dark"),
        "unrelated keys survive the merge: {v}"
    );
    assert_dbless_engram_entry(&sb.json_at(&sb.xdg_path()));

    // Idempotent: a second run leaves both existing entries alone.
    let legacy_before = std::fs::read_to_string(sb.legacy_path()).unwrap();
    let xdg_before = std::fs::read_to_string(sb.xdg_path()).unwrap();
    let out = sb.run_setup();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("already has engram"),
        "second run reports the existing entries: {out:?}"
    );
    assert_eq!(
        legacy_before,
        std::fs::read_to_string(sb.legacy_path()).unwrap(),
        "second run changes the legacy file in no way"
    );
    assert_eq!(
        xdg_before,
        std::fs::read_to_string(sb.xdg_path()).unwrap(),
        "second run changes the XDG file in no way"
    );
}

#[test]
fn never_rewrites_unparseable_config() {
    let sb = Sandbox::new("badjson");
    std::fs::create_dir_all(sb.xdg_path().parent().unwrap()).unwrap();
    std::fs::write(sb.xdg_path(), "{not json").unwrap();

    let out = sb.run_setup();
    assert!(out.status.success(), "setup still succeeds: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("isn't valid JSON") && stdout.contains("\"engram\""),
        "prints the manual snippet instead of clobbering:\n{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(sb.xdg_path()).unwrap(),
        "{not json",
        "the unparseable file is untouched"
    );
    // The sibling generation's file is independent — still written cleanly.
    assert_dbless_engram_entry(&sb.json_at(&sb.legacy_path()));
}

/// The claude adapter's project entry went db-less too (its launch cwd IS
/// the project, and Claude Code answers roots) — locked here so a future
/// adapter edit doesn't silently reintroduce the per-repo pin.
#[test]
fn claude_project_entry_is_dbless() {
    let sb = Sandbox::new("claude");
    let out = Command::new(BIN)
        .args(["setup", "--cli", "claude", "--mcp-only"])
        .current_dir(sb.repo())
        .env("HOME", sb.root.join("home"))
        .env("ENGRAM_HOME", sb.root.join("home/.engram"))
        .env("ENGRAM_UPDATE_CHECK", "0")
        .output()
        .expect("running setup");
    assert!(out.status.success(), "setup failed: {out:?}");
    let raw = std::fs::read_to_string(sb.repo().join(".mcp.json")).expect(".mcp.json written");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        v["mcpServers"]["engram"]["args"],
        serde_json::json!(["mcp"])
    );
    assert_eq!(v["mcpServers"]["engram"]["command"], serde_json::json!(BIN));
}
