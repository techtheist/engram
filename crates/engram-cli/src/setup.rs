//! `engram-alpha setup` — wire the current repository for AI assistants: MCP
//! registration + capture instructions, all from assets embedded in the
//! binary (PLAN §8/§10 Phase 3). The shell installers only fetch the binary;
//! this module is the single source of setup truth.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;

// Machine/harness probes live in engram-core (shared with the daemon's
// /system endpoint); re-exported so main.rs/doctor keep their `setup::` paths.
pub use engram_core::harness::{
    AGENTS, detect_agents, home_file, is_prerename_bin, is_wired, on_path,
};

const MARK_BEGIN: &str = "<!-- engram:begin -->";
const MARK_END: &str = "<!-- engram:end -->";

// Claude Code skill variants (full SKILL.md files).
const CLAUDE_RELAXED: &str = include_str!("../../../skills/engram/relaxed/SKILL.md");
const CLAUDE_NORMAL: &str = include_str!("../../../skills/engram/normal/SKILL.md");
const CLAUDE_AGGRESSIVE: &str = include_str!("../../../skills/engram/aggressive/SKILL.md");
// The digest skill (PLAN §7B) has no variants: one explicit-invocation
// ingestion doc, installed alongside whichever capture variant was chosen.
const CLAUDE_DIGEST: &str = include_str!("../../../skills/engram/digest/SKILL.md");
// Harness-neutral variants for AGENTS.md / GEMINI.md / rules files.
const AGENT_RELAXED: &str = include_str!("../../../skills/engram/agents/relaxed.md");
const AGENT_NORMAL: &str = include_str!("../../../skills/engram/agents/normal.md");
const AGENT_AGGRESSIVE: &str = include_str!("../../../skills/engram/agents/aggressive.md");
// SessionStart hook: injects the brief so sessions start pre-briefed.
const SESSION_BRIEF_HOOK: &str = include_str!("../../../hooks/session-brief.sh");
const FILE_READ_MATCH_HOOK: &str = include_str!("../../../hooks/file-read-match.sh");

pub fn claude_skill(variant: &str) -> &'static str {
    match variant {
        "normal" => CLAUDE_NORMAL,
        "aggressive" => CLAUDE_AGGRESSIVE,
        _ => CLAUDE_RELAXED,
    }
}

pub fn agent_block(variant: &str) -> &'static str {
    match variant {
        "normal" => AGENT_NORMAL,
        "aggressive" => AGENT_AGGRESSIVE,
        _ => AGENT_RELAXED,
    }
}

fn say(msg: &str) {
    println!("==> {msg}");
}

/// Whether the path itself is a symlink (never follows it).
pub(crate) fn is_symlink(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

pub struct Setup {
    repo: PathBuf,
    bin: String,
    db: String,
    variant: String,
    mcp_only: bool,
}

impl Setup {
    pub fn new(variant: &str, mcp_only: bool) -> anyhow::Result<Self> {
        let repo = std::env::current_dir()?;
        let bin = std::env::current_exe()
            .context("locating the engram binary")?
            .display()
            .to_string();
        let db = repo.join(".engram/graph.db").display().to_string();
        Ok(Self {
            repo,
            bin,
            db,
            variant: variant.to_string(),
            mcp_only,
        })
    }

    /// Wire the given agents (deduplicated, order-stable). Always ensures the
    /// personal graph is git-ignored.
    pub fn run(&self, agents: &[&str]) -> anyhow::Result<()> {
        self.ensure_gitignore()?;
        let unique: BTreeSet<&str> = agents.iter().copied().collect();
        for agent in unique {
            match agent {
                "claude" => self.wire_claude()?,
                "codex" => self.wire_codex()?,
                "gemini" => self.wire_gemini()?,
                "opencode" => self.wire_mcp_array("opencode.json", "opencode")?,
                "kilo" => self.wire_mcp_array("kilo.json", "kilo")?,
                "antigravity" => self.wire_antigravity()?,
                other => anyhow::bail!("unknown agent: {other}"),
            }
        }
        say("done — restart your assistant sessions so they pick up the MCP server");
        Ok(())
    }

    fn ensure_gitignore(&self) -> anyhow::Result<()> {
        let path = self.repo.join(".gitignore");
        let current = fs::read_to_string(&path).unwrap_or_default();
        if !current.lines().any(|l| l.trim() == ".engram/") {
            let mut s = current;
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str("\n# Engram local graph (personal)\n.engram/\n");
            fs::write(&path, s)?;
        }
        Ok(())
    }

    /// Insert or refresh the marked instruction section in an AGENTS.md-style
    /// file. Re-running with a different --skill replaces the section.
    fn write_instructions(&self, file: &str) -> anyhow::Result<()> {
        if self.mcp_only {
            return Ok(());
        }
        let path = self.repo.join(file);
        let block = format!("{MARK_BEGIN}\n{}{MARK_END}\n", agent_block(&self.variant));
        let current = fs::read_to_string(&path).unwrap_or_default();
        let next = match (current.find(MARK_BEGIN), current.find(MARK_END)) {
            (Some(start), Some(end)) if end > start => {
                let after = end + MARK_END.len();
                let tail = current[after..]
                    .strip_prefix('\n')
                    .unwrap_or(&current[after..]);
                format!("{}{}{}", &current[..start], block, tail)
            }
            _ => {
                let mut s = current;
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                if !s.is_empty() {
                    s.push('\n');
                }
                s + &block
            }
        };
        fs::write(&path, next)?;
        say(&format!(
            "{file}: engram section in place ({})",
            self.variant
        ));
        Ok(())
    }

    /// v0.3.0 → v0.4.0 repair: if a JSON config's engram entry launches the
    /// pre-rename binary, re-point it at this one. Handles both shapes —
    /// `mcpServers.engram.command` (claude/gemini) and the opencode/kilo
    /// `mcp.engram.command` array. Returns the rewritten file when a repair
    /// applied; None means nothing to fix.
    fn repaired_json_config(&self, raw: &str) -> Option<String> {
        let mut v: serde_json::Value = serde_json::from_str(raw).ok()?;
        let mut repaired = false;
        if let Some(entry) = v.pointer_mut("/mcpServers/engram") {
            let cmd = entry.get("command").and_then(|c| c.as_str());
            if cmd.is_some_and(is_prerename_bin) {
                entry["command"] = serde_json::json!(self.bin);
                entry["args"] = serde_json::json!(["mcp", "--db", self.db]);
                repaired = true;
            }
        }
        if let Some(entry) = v.pointer_mut("/mcp/engram") {
            let first = entry
                .get("command")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|x| x.as_str());
            if first.is_some_and(is_prerename_bin) {
                entry["command"] = serde_json::json!([self.bin, "mcp", "--db", self.db]);
                repaired = true;
            }
        }
        repaired
            .then(|| serde_json::to_string_pretty(&v).ok())
            .flatten()
            .map(|s| s + "\n")
    }

    /// The same repair for codex's global TOML: rewrite the `command = "…"`
    /// line inside `[mcp_servers.engram]` when it names the pre-rename binary.
    fn repaired_codex_toml(&self, raw: &str) -> Option<String> {
        let mut in_engram = false;
        let mut repaired = false;
        let out: Vec<String> = raw
            .lines()
            .map(|line| {
                let t = line.trim();
                if t.starts_with('[') {
                    in_engram = t == "[mcp_servers.engram]";
                } else if in_engram
                    && t.starts_with("command")
                    && t.split('"').nth(1).is_some_and(is_prerename_bin)
                {
                    repaired = true;
                    return format!("command = \"{}\"", self.bin);
                }
                line.to_string()
            })
            .collect();
        repaired.then(|| out.join("\n") + "\n")
    }

    fn mcp_snippet(&self) -> String {
        format!(
            "\"engram\": {{ \"command\": \"{}\", \"args\": [\"mcp\", \"--db\", \"{}\"] }}",
            self.bin, self.db
        )
    }

    /// Write an `mcpServers`-shaped config, or print the snippet when a
    /// foreign config already exists (never rewrite user JSON blindly).
    fn write_mcp_servers(&self, rel: &str, label: &str) -> anyhow::Result<()> {
        let path = self.repo.join(rel);
        if path.exists() {
            let current = fs::read_to_string(&path)?;
            if current.contains("\"engram\"") {
                if let Some(next) = self.repaired_json_config(&current) {
                    fs::write(&path, next)?;
                    say(&format!(
                        "{label}: re-pointed {rel}'s engram entry at this binary (was the pre-rename `engram`)"
                    ));
                } else {
                    say(&format!("{label}: {rel} already has engram — leaving it"));
                }
            } else {
                say(&format!(
                    "{label}: {rel} exists — add this to its mcpServers manually:"
                ));
                println!("    {}", self.mcp_snippet());
            }
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &path,
            format!(
                "{{\n  \"mcpServers\": {{\n    \"engram\": {{\n      \"command\": \"{}\",\n      \"args\": [\"mcp\", \"--db\", \"{}\"]\n    }}\n  }}\n}}\n",
                self.bin, self.db
            ),
        )?;
        say(&format!("{label}: wrote {rel}"));
        Ok(())
    }

    fn wire_claude(&self) -> anyhow::Result<()> {
        self.write_mcp_servers(".mcp.json", "claude")?;
        if self.mcp_only {
            return Ok(());
        }
        let dir = self.repo.join(".claude/skills/engram");
        // A symlinked skill dir (this repo dogfoods that way) points into
        // someone's source tree — writing through it would clobber the
        // original. Leave symlinks strictly alone.
        if is_symlink(&dir) || is_symlink(&dir.join("SKILL.md")) {
            say("claude: .claude/skills/engram is a symlink — leaving it untouched");
            return Ok(());
        }
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("SKILL.md"), claude_skill(&self.variant))?;
        say(&format!(
            "claude: installed the '{}' skill to .claude/skills/engram",
            self.variant
        ));
        let digest_dir = self.repo.join(".claude/skills/engram-digest");
        if is_symlink(&digest_dir) || is_symlink(&digest_dir.join("SKILL.md")) {
            say("claude: .claude/skills/engram-digest is a symlink — leaving it untouched");
        } else {
            fs::create_dir_all(&digest_dir)?;
            fs::write(digest_dir.join("SKILL.md"), CLAUDE_DIGEST)?;
            say("claude: installed the digest skill to .claude/skills/engram-digest");
        }
        self.install_claude_brief_hook()
    }

    /// Install the SessionStart brief hook: the script under `.claude/hooks/`
    /// plus its registration in `.claude/settings.json`. A foreign settings
    /// file is never rewritten — the snippet is printed instead (same policy
    /// as `write_mcp_servers`).
    fn install_claude_brief_hook(&self) -> anyhow::Result<()> {
        let hooks_dir = self.repo.join(".claude/hooks");
        fs::create_dir_all(&hooks_dir)?;
        for (name, body) in [
            ("engram-brief.sh", SESSION_BRIEF_HOOK),
            ("engram-refs.sh", FILE_READ_MATCH_HOOK),
        ] {
            let script = hooks_dir.join(name);
            fs::write(&script, body)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
            }
        }

        let registration = r#"{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|clear|compact",
        "hooks": [
          {
            "type": "command",
            "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/engram-brief.sh"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Read|Edit|Write|MultiEdit",
        "hooks": [
          {
            "type": "command",
            "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/engram-refs.sh"
          }
        ]
      }
    ]
  }
}
"#;
        let settings = self.repo.join(".claude/settings.json");
        if settings.exists() {
            let current = fs::read_to_string(&settings)?;
            let has_brief = current.contains("engram-brief") || current.contains("session-brief");
            let has_refs = current.contains("engram-refs") || current.contains("file-read-match");
            if has_brief && has_refs {
                say("claude: .claude/settings.json already runs both hooks — leaving it");
            } else {
                say("claude: .claude/settings.json exists — merge the missing hook(s) from:");
                println!("{registration}");
            }
            return Ok(());
        }
        fs::write(&settings, registration)?;
        say("claude: brief + file-read-match hooks installed (.claude/hooks + settings.json)");
        Ok(())
    }

    /// Codex's MCP config is global (`~/.codex/config.toml`) and shared by the
    /// CLI, the IDE extension, and the Codex/ChatGPT desktop app — the app
    /// ignores project-local config entirely (openai/codex#13025). No --db, so
    /// the graph resolves against the cwd: one entry serves every repo when
    /// codex is launched from the repo root.
    fn wire_codex(&self) -> anyhow::Result<()> {
        let path = home_file(".codex/config.toml").context("no home directory")?;
        let current = fs::read_to_string(&path).unwrap_or_default();
        if current.contains("[mcp_servers.engram]") {
            if let Some(next) = self.repaired_codex_toml(&current) {
                fs::write(&path, next)?;
                say(
                    "codex: re-pointed ~/.codex/config.toml's engram entry at this binary (was the pre-rename `engram`)",
                );
            } else {
                say("codex: ~/.codex/config.toml already has engram — leaving it");
            }
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut s = current;
            s.push_str(&format!(
                "\n# Engram — durable project memory (db resolves per-repo against the cwd)\n[mcp_servers.engram]\ncommand = \"{}\"\nargs = [\"mcp\"]\n",
                self.bin
            ));
            fs::write(&path, s)?;
            say(
                "codex: registered engram in ~/.codex/config.toml (launch codex from the repo root)",
            );
            say(
                "codex: the desktop app may launch MCP servers from another cwd — if you use it, pin this repo there: add `cwd = \"<repo>\"` or `args = [\"mcp\", \"--db\", \"<repo>/.engram/graph.db\"]` to that entry (`engram-alpha doctor` checks this)",
            );
        }
        self.write_instructions("AGENTS.md")
    }

    fn wire_gemini(&self) -> anyhow::Result<()> {
        self.write_mcp_servers(".gemini/settings.json", "gemini")?;
        self.write_instructions("GEMINI.md")
    }

    fn wire_antigravity(&self) -> anyhow::Result<()> {
        self.write_mcp_servers(".agents/mcp_config.json", "antigravity")?;
        self.write_instructions("AGENTS.md")
    }

    /// opencode.json / kilo.json share the {"mcp": {..., "type": "local"}} shape.
    fn wire_mcp_array(&self, rel: &str, label: &str) -> anyhow::Result<()> {
        let path = self.repo.join(rel);
        if path.exists() {
            let current = fs::read_to_string(&path)?;
            if current.contains("\"engram\"") {
                if let Some(next) = self.repaired_json_config(&current) {
                    fs::write(&path, next)?;
                    say(&format!(
                        "{label}: re-pointed {rel}'s engram entry at this binary (was the pre-rename `engram`)"
                    ));
                } else {
                    say(&format!("{label}: {rel} already has engram — leaving it"));
                }
            } else {
                say(&format!(
                    "{label}: {rel} exists — add this to its \"mcp\" block manually:"
                ));
                println!(
                    "    \"engram\": {{ \"type\": \"local\", \"command\": [\"{}\", \"mcp\", \"--db\", \"{}\"], \"enabled\": true }}",
                    self.bin, self.db
                );
            }
        } else {
            fs::write(
                &path,
                format!(
                    "{{\n  \"mcp\": {{\n    \"engram\": {{\n      \"type\": \"local\",\n      \"command\": [\"{}\", \"mcp\", \"--db\", \"{}\"],\n      \"enabled\": true\n    }}\n  }}\n}}\n",
                    self.bin, self.db
                ),
            )?;
            say(&format!("{label}: wrote {rel}"));
        }
        self.write_instructions("AGENTS.md")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Claude Code plugin (claude-plugin/) ships verbatim copies of assets
    // whose canonical home is elsewhere in the repo — a symlink would break on
    // Windows checkouts, so these tests are the sync mechanism instead.

    #[test]
    fn prerename_detection() {
        assert!(is_prerename_bin("/usr/local/bin/engram"));
        assert!(is_prerename_bin("engram.exe"));
        assert!(!is_prerename_bin("/usr/local/bin/engram-alpha"));
        assert!(!is_prerename_bin("someones-engram"));
    }

    #[test]
    fn prerename_wiring_is_repaired_in_every_config_shape() {
        let s = Setup {
            repo: PathBuf::from("/repo"),
            bin: "/new/engram-alpha".into(),
            db: "/repo/.engram/graph.db".into(),
            variant: "relaxed".into(),
            mcp_only: true,
        };

        // claude / gemini shape: mcpServers.engram.command
        let raw = r#"{"mcpServers":{"engram":{"command":"/old/engram","args":["mcp","--db","/repo/.engram/graph.db"]},"other":{"command":"keep-me"}}}"#;
        let fixed = s.repaired_json_config(raw).expect("repairs old command");
        assert!(fixed.contains("/new/engram-alpha"));
        assert!(fixed.contains("keep-me"), "unrelated servers survive");
        assert!(s.repaired_json_config(&fixed).is_none(), "idempotent");

        // opencode / kilo shape: mcp.engram.command array
        let raw = r#"{"mcp":{"engram":{"type":"local","command":["/old/engram.exe","mcp"],"enabled":true}}}"#;
        let fixed = s.repaired_json_config(raw).expect("repairs command array");
        assert!(fixed.contains("/new/engram-alpha"));

        // codex global TOML: only the engram section's command line changes
        let raw = "[mcp_servers.other]\ncommand = \"/old/engram\"\n[mcp_servers.engram]\ncommand = \"/old/engram\"\nargs = [\"mcp\"]\n";
        let fixed = s.repaired_codex_toml(raw).expect("repairs codex command");
        assert!(fixed.contains("command = \"/new/engram-alpha\""));
        assert_eq!(
            fixed.matches("/new/engram-alpha").count(),
            1,
            "other sections untouched"
        );
        assert!(s.repaired_codex_toml(&fixed).is_none(), "idempotent");
    }

    #[test]
    fn plugin_skill_matches_relaxed_variant() {
        let plugin = include_str!("../../../claude-plugin/skills/engram/SKILL.md");
        assert_eq!(
            plugin, CLAUDE_RELAXED,
            "claude-plugin/skills/engram/SKILL.md drifted from skills/engram/relaxed/SKILL.md — re-copy it"
        );
    }

    #[test]
    fn plugin_digest_skill_matches_canonical() {
        let plugin = include_str!("../../../claude-plugin/skills/engram-digest/SKILL.md");
        assert_eq!(
            plugin, CLAUDE_DIGEST,
            "claude-plugin/skills/engram-digest/SKILL.md drifted from skills/engram/digest/SKILL.md — re-copy it"
        );
    }

    #[test]
    fn plugin_hook_matches_canonical_script() {
        let plugin = include_str!("../../../claude-plugin/hooks/session-brief.sh");
        assert_eq!(
            plugin, SESSION_BRIEF_HOOK,
            "claude-plugin/hooks/session-brief.sh drifted from hooks/session-brief.sh — re-copy it"
        );
        let refs = include_str!("../../../claude-plugin/hooks/file-read-match.sh");
        assert_eq!(
            refs, FILE_READ_MATCH_HOOK,
            "claude-plugin/hooks/file-read-match.sh drifted from hooks/file-read-match.sh — re-copy it"
        );
    }

    #[test]
    fn plugin_manifests_parse() {
        for raw in [
            include_str!("../../../claude-plugin/.claude-plugin/plugin.json"),
            include_str!("../../../.claude-plugin/marketplace.json"),
            include_str!("../../../claude-plugin/hooks/hooks.json"),
        ] {
            serde_json::from_str::<serde_json::Value>(raw).expect("plugin manifest is valid JSON");
        }
    }

    // The plugin installs from the repo (not from release artifacts), so its
    // checked-in version must move with the workspace version by hand — this
    // makes a release-prep bump of Cargo.toml fail until plugin.json follows.
    #[test]
    fn plugin_version_matches_workspace() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../../../claude-plugin/.claude-plugin/plugin.json"
        ))
        .unwrap();
        assert_eq!(
            manifest["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "claude-plugin/.claude-plugin/plugin.json version drifted from [workspace.package] version"
        );
    }
}
