//! `engram setup` — wire the current repository for AI assistants: MCP
//! registration + capture instructions, all from assets embedded in the
//! binary (PLAN §8/§10 Phase 3). The shell installers only fetch the binary;
//! this module is the single source of setup truth.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;

pub const AGENTS: [&str; 6] = [
    "claude",
    "codex",
    "gemini",
    "opencode",
    "kilo",
    "antigravity",
];

const MARK_BEGIN: &str = "<!-- engram:begin -->";
const MARK_END: &str = "<!-- engram:end -->";

// Claude Code skill variants (full SKILL.md files).
const CLAUDE_RELAXED: &str = include_str!("../../../skills/engram/relaxed/SKILL.md");
const CLAUDE_NORMAL: &str = include_str!("../../../skills/engram/normal/SKILL.md");
const CLAUDE_AGGRESSIVE: &str = include_str!("../../../skills/engram/aggressive/SKILL.md");
// Harness-neutral variants for AGENTS.md / GEMINI.md / rules files.
const AGENT_RELAXED: &str = include_str!("../../../skills/engram/agents/relaxed.md");
const AGENT_NORMAL: &str = include_str!("../../../skills/engram/agents/normal.md");
const AGENT_AGGRESSIVE: &str = include_str!("../../../skills/engram/agents/aggressive.md");
// SessionStart hook: injects the brief so sessions start pre-briefed.
const SESSION_BRIEF_HOOK: &str = include_str!("../../../hooks/session-brief.sh");

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

/// Which assistants look installed on this machine (binary on PATH or a
/// well-known config directory).
pub fn detect_agents() -> Vec<&'static str> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .ok();
    let dir = |rel: &str| home.as_ref().is_some_and(|h| h.join(rel).exists());
    let mut found = Vec::new();
    if on_path("claude") || dir(".claude") {
        found.push("claude");
    }
    if on_path("codex") || dir(".codex") {
        found.push("codex");
    }
    if on_path("gemini") || dir(".gemini") {
        found.push("gemini");
    }
    if on_path("opencode") || dir(".config/opencode") {
        found.push("opencode");
    }
    if on_path("kilo") {
        found.push("kilo");
    }
    if on_path("antigravity") || on_path("agy") || dir(".antigravity") {
        found.push("antigravity");
    }
    found
}

fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let base = dir.join(bin);
        base.exists() || base.with_extension("exe").exists() || base.with_extension("cmd").exists()
    })
}

/// Is this repo already wired for the given agent? (Used by `serve` to decide
/// whether to suggest `engram setup` — cheap file probes only.)
pub fn is_wired(repo: &Path, agent: &str) -> bool {
    let has_engram = |p: PathBuf| fs::read_to_string(p).is_ok_and(|s| s.contains("engram"));
    match agent {
        "claude" => has_engram(repo.join(".mcp.json")),
        "codex" => home_file(".codex/config.toml").is_some_and(|p| {
            fs::read_to_string(p).is_ok_and(|s| s.contains("[mcp_servers.engram]"))
        }),
        "gemini" => has_engram(repo.join(".gemini/settings.json")),
        "opencode" => has_engram(repo.join("opencode.json")),
        "kilo" => has_engram(repo.join("kilo.json")),
        "antigravity" => has_engram(repo.join(".agents/mcp_config.json")),
        _ => false,
    }
}

fn home_file(rel: &str) -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| PathBuf::from(h).join(rel))
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
                say(&format!("{label}: {rel} already has engram — leaving it"));
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
        let is_symlink =
            |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
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
        self.install_claude_brief_hook()
    }

    /// Install the SessionStart brief hook: the script under `.claude/hooks/`
    /// plus its registration in `.claude/settings.json`. A foreign settings
    /// file is never rewritten — the snippet is printed instead (same policy
    /// as `write_mcp_servers`).
    fn install_claude_brief_hook(&self) -> anyhow::Result<()> {
        let hooks_dir = self.repo.join(".claude/hooks");
        fs::create_dir_all(&hooks_dir)?;
        let script = hooks_dir.join("engram-brief.sh");
        fs::write(&script, SESSION_BRIEF_HOOK)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        }

        let registration = "{\n  \"hooks\": {\n    \"SessionStart\": [\n      {\n        \"matcher\": \"startup|clear|compact\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"\\\"$CLAUDE_PROJECT_DIR\\\"/.claude/hooks/engram-brief.sh\"\n          }\n        ]\n      }\n    ]\n  }\n}\n";
        let settings = self.repo.join(".claude/settings.json");
        if settings.exists() {
            let current = fs::read_to_string(&settings)?;
            if current.contains("engram-brief") || current.contains("session-brief") {
                say("claude: .claude/settings.json already runs the brief hook — leaving it");
            } else {
                say("claude: .claude/settings.json exists — add this SessionStart hook manually:");
                println!("{registration}");
            }
            return Ok(());
        }
        fs::write(&settings, registration)?;
        say("claude: session-start brief hook installed (.claude/hooks + settings.json)");
        Ok(())
    }

    fn wire_codex(&self) -> anyhow::Result<()> {
        // Codex's MCP config is global; no --db so the graph resolves against
        // the cwd — one entry serves every repo, launch codex from the root.
        let path = home_file(".codex/config.toml").context("no home directory")?;
        let current = fs::read_to_string(&path).unwrap_or_default();
        if current.contains("[mcp_servers.engram]") {
            say("codex: ~/.codex/config.toml already has engram — leaving it");
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
                say(&format!("{label}: {rel} already has engram — leaving it"));
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
    fn plugin_skill_matches_relaxed_variant() {
        let plugin = include_str!("../../../claude-plugin/skills/engram/SKILL.md");
        assert_eq!(
            plugin, CLAUDE_RELAXED,
            "claude-plugin/skills/engram/SKILL.md drifted from skills/engram/relaxed/SKILL.md — re-copy it"
        );
    }

    #[test]
    fn plugin_hook_matches_canonical_script() {
        let plugin = include_str!("../../../claude-plugin/hooks/session-brief.sh");
        assert_eq!(
            plugin, SESSION_BRIEF_HOOK,
            "claude-plugin/hooks/session-brief.sh drifted from hooks/session-brief.sh — re-copy it"
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
