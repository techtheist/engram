//! `engram-alpha setup --cli devin` (issue #8) wires the repository for
//! Devin CLI: the project-local MCP config (highest-precedence tier, explicit
//! --db, git-ignored), skills under `.devin/skills`, and a SessionStart hook
//! whose output is the JSON envelope Devin requires. The full (non
//! `--mcp-only`) Windsurf run (issue #7) is locked here too: the always_on
//! rule plus `.windsurf/skills`. Sandboxed HOME/ENGRAM_HOME; no core is
//! started (setup never talks to one).

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_engram-alpha");

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("engram-devin-{tag}-{}", std::process::id()));
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
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn devin_wires_local_config_skills_and_hook() {
    let sb = Sandbox::new("full");
    let out = sb.run_setup("devin", &["--skill", "aggressive"]);
    assert!(out.status.success(), "setup failed: {out:?}");

    // Project-local MCP config: Devin's highest-precedence tier, explicit
    // --db (the file is per-repo and personal, so the pin costs nothing).
    let raw = std::fs::read_to_string(sb.repo().join(".devin/mcp_config.local.json"))
        .expect("local MCP config written");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["mcpServers"]["engram"]["command"], serde_json::json!(BIN));
    let args = v["mcpServers"]["engram"]["args"].as_array().unwrap();
    assert_eq!(args[0], "mcp");
    assert_eq!(args[1], "--db", "the project-local entry pins the db: {v}");

    // …and it is git-ignored: it carries personal absolute paths.
    let gitignore = std::fs::read_to_string(sb.repo().join(".gitignore")).unwrap();
    assert!(
        gitignore
            .lines()
            .any(|l| l.trim() == ".devin/mcp_config.local.json"),
        "local config is git-ignored:\n{gitignore}"
    );

    // Skills: same SKILL.md format Claude uses, read natively by Devin.
    let skill = std::fs::read_to_string(sb.repo().join(".devin/skills/engram/SKILL.md"))
        .expect("capture skill installed");
    assert!(
        skill.contains("Aggressive variant"),
        "--skill aggressive installs the aggressive variant"
    );
    assert!(
        sb.repo()
            .join(".devin/skills/engram-digest/SKILL.md")
            .exists(),
        "digest skill installed alongside"
    );

    // SessionStart hook: registration + both scripts (the shared brief
    // script and the JSON-envelope wrapper).
    let hooks: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(sb.repo().join(".devin/hooks.v1.json"))
            .expect("hooks.v1.json written"),
    )
    .unwrap();
    assert_eq!(
        hooks["SessionStart"][0]["hooks"][0]["command"],
        serde_json::json!("./.devin/hooks/engram-brief.sh")
    );
    for script in ["engram-brief.sh", "engram-brief-text.sh"] {
        let p = sb.repo().join(".devin/hooks").join(script);
        assert!(p.exists(), "{script} installed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o111,
                0,
                "{script} is executable"
            );
        }
    }

    // The capture discipline travels AGENTS.md like the other harnesses.
    let agents = std::fs::read_to_string(sb.repo().join("AGENTS.md")).unwrap();
    assert!(agents.contains("<!-- engram:begin -->"));

    // A foreign hooks file is never rewritten — the snippet is printed.
    std::fs::write(
        sb.repo().join(".devin/hooks.v1.json"),
        r#"{"PreToolUse":[{"hooks":[{"type":"command","command":"./check.sh"}]}]}"#,
    )
    .unwrap();
    let out = sb.run_setup("devin", &[]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("merge the SessionStart hook"),
        "foreign hooks file gets the snippet, not a rewrite:\n{stdout}"
    );
    assert!(
        std::fs::read_to_string(sb.repo().join(".devin/hooks.v1.json"))
            .unwrap()
            .contains("PreToolUse"),
        "the foreign hooks file is untouched"
    );
}

/// The wrapper's awk escaper must produce valid JSON for hostile markdown —
/// quotes, backslashes, tabs, and newlines all appear in real briefs.
#[cfg(unix)]
#[test]
fn devin_hook_wraps_the_brief_in_a_valid_json_envelope() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new("envelope");
    let out = sb.run_setup("devin", &[]);
    assert!(out.status.success(), "setup failed: {out:?}");

    // Stub the shared brief script with output exercising every escape.
    let text_script = sb.repo().join(".devin/hooks/engram-brief-text.sh");
    std::fs::write(
        &text_script,
        "#!/bin/sh\nprintf '%s\\n' '# Brief' 'a \"quoted\" thing' 'back\\slash' 'tab\there'\n",
    )
    .unwrap();
    std::fs::set_permissions(&text_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = Command::new(sb.repo().join(".devin/hooks/engram-brief.sh"))
        .current_dir(sb.repo())
        .output()
        .expect("running the wrapper");
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("wrapper output is valid JSON");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert_eq!(ctx, "# Brief\na \"quoted\" thing\nback\\slash\ntab\there");
}

#[test]
fn windsurf_full_setup_writes_rule_and_skills() {
    let sb = Sandbox::new("windsurf");
    let out = sb.run_setup("windsurf", &["--skill", "normal"]);
    assert!(out.status.success(), "setup failed: {out:?}");

    let rule = std::fs::read_to_string(sb.repo().join(".windsurf/rules/engram.md"))
        .expect("always_on rule written");
    assert!(
        rule.starts_with("---\ntrigger: always_on\n"),
        "the rule is always_on:\n{rule}"
    );
    assert!(
        rule.contains("`brief` with `project`"),
        "the critical first instruction is the brief-rebind:\n{rule}"
    );
    assert!(
        rule.contains("**Recall.**"),
        "the capture discipline rides in the rule body:\n{rule}"
    );
    assert!(
        sb.repo().join(".windsurf/skills/engram/SKILL.md").exists()
            && sb
                .repo()
                .join(".windsurf/skills/engram-digest/SKILL.md")
                .exists(),
        "skills installed under .windsurf/skills"
    );

    // `--mcp-only` skips rule and skills (the plugin-style setup).
    let sb2 = Sandbox::new("windsurf-mcponly");
    let out = sb2.run_setup("windsurf", &["--mcp-only"]);
    assert!(out.status.success());
    assert!(
        !sb2.repo().join(".windsurf").exists(),
        "--mcp-only writes no .windsurf directory"
    );
}
