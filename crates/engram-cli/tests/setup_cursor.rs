//! `engram-alpha setup --cli cursor` wires the repository for
//! Cursor: the project MCP config `.cursor/mcp.json` (explicit --db,
//! git-ignored when setup creates it), skills under `.cursor/skills` unless
//! `.claude/skills/engram` already serves Cursor, and a `sessionStart` hook
//! in `.cursor/hooks.json` whose flat `additional_context` output carries the
//! brief. Sandboxed HOME/ENGRAM_HOME; no core is started.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_engram-alpha");

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("engram-cursor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo")).unwrap();
        Self { root }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn run_setup(&self, cli: &str, extra: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .args(["setup", "--cli", cli])
            .args(extra)
            .current_dir(self.repo())
            .env("HOME", self.root.join("home"))
            .env("ENGRAM_HOME", self.root.join("home/.engram"))
            .env("ENGRAM_UPDATE_CHECK", "0")
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .expect("running setup")
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.repo().join(rel)).unwrap_or_else(|_| panic!("{rel} exists"))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cursor_wires_mcp_config_skills_and_session_start_hook() {
    let sb = Sandbox::new("full");
    let out = sb.run_setup("cursor", &["--skill", "aggressive"]);
    assert!(out.status.success(), "setup failed: {out:?}");

    let v: serde_json::Value = serde_json::from_str(&sb.read(".cursor/mcp.json")).unwrap();
    assert_eq!(v["mcpServers"]["engram"]["command"], serde_json::json!(BIN));
    let args = v["mcpServers"]["engram"]["args"].as_array().unwrap();
    assert_eq!(args[0], "mcp");
    assert_eq!(args[1], "--db", "the project entry pins the db: {v}");
    assert!(
        sb.read(".gitignore")
            .lines()
            .any(|l| l.trim() == ".cursor/mcp.json"),
        "a config setup created carries personal paths and is git-ignored"
    );

    assert!(
        sb.read(".cursor/skills/engram/SKILL.md")
            .contains("Aggressive variant"),
        "--skill aggressive installs the aggressive variant"
    );
    assert!(
        sb.repo()
            .join(".cursor/skills/engram-digest/SKILL.md")
            .exists()
    );

    let hooks: serde_json::Value = serde_json::from_str(&sb.read(".cursor/hooks.json")).unwrap();
    assert_eq!(hooks["version"], 1);
    assert_eq!(
        hooks["hooks"]["sessionStart"][0]["command"],
        serde_json::json!("./.cursor/hooks/engram-brief.sh cursor")
    );
    for script in ["engram-brief.sh", "engram-brief-text.sh"] {
        assert!(
            sb.repo().join(".cursor/hooks").join(script).exists(),
            "{script} installed"
        );
    }
    assert!(sb.read("AGENTS.md").contains("<!-- engram:begin v"));

    // Re-running is idempotent: one hook entry, not two.
    assert!(sb.run_setup("cursor", &[]).status.success());
    let hooks: serde_json::Value = serde_json::from_str(&sb.read(".cursor/hooks.json")).unwrap();
    assert_eq!(hooks["hooks"]["sessionStart"].as_array().unwrap().len(), 1);
}

#[test]
fn cursor_merges_into_existing_files_and_reuses_the_claude_skill() {
    let sb = Sandbox::new("merge");
    std::fs::create_dir_all(sb.repo().join(".cursor")).unwrap();
    std::fs::create_dir_all(sb.repo().join(".claude/skills/engram")).unwrap();
    std::fs::write(sb.repo().join(".claude/skills/engram/SKILL.md"), "existing").unwrap();
    std::fs::write(
        sb.repo().join(".cursor/hooks.json"),
        r#"{"version":1,"hooks":{"afterFileEdit":[{"command":"./fmt.sh"}],"sessionStart":[{"command":"./mine.sh"}]}}"#,
    )
    .unwrap();
    std::fs::write(
        sb.repo().join(".cursor/mcp.json"),
        r#"{"mcpServers":{"team":{"command":"team-server"}}}"#,
    )
    .unwrap();

    let out = sb.run_setup("cursor", &[]);
    assert!(out.status.success(), "setup failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The user's hooks survive; ours is appended.
    let hooks: serde_json::Value = serde_json::from_str(&sb.read(".cursor/hooks.json")).unwrap();
    assert_eq!(hooks["hooks"]["afterFileEdit"][0]["command"], "./fmt.sh");
    let start = hooks["hooks"]["sessionStart"].as_array().unwrap();
    assert_eq!(start.len(), 2);
    assert_eq!(start[0]["command"], "./mine.sh");

    // A team's existing mcp.json is never rewritten or git-ignored — the
    // snippet is printed instead.
    assert!(!sb.read(".cursor/mcp.json").contains("engram"));
    assert!(
        stdout.contains("add this to its mcpServers manually"),
        "{stdout}"
    );
    let ignored = std::fs::read_to_string(sb.repo().join(".gitignore")).unwrap_or_default();
    assert!(!ignored.lines().any(|l| l.trim() == ".cursor/mcp.json"));

    // Cursor reads .claude/skills: no duplicate copy under .cursor/skills.
    assert!(!sb.repo().join(".cursor/skills/engram").exists());
    assert_eq!(sb.read(".claude/skills/engram/SKILL.md"), "existing");
}
