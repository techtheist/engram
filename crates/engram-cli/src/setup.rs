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
    AGENTS, detect_agents, home_file, is_prerename_bin, is_wired, on_path, windsurf_xdg_config,
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
// Envelope wrapper: Devin CLI and Codex CLI inject SessionStart context only
// via the hookSpecificOutput JSON envelope, so both get this wrapper around
// the portable script above.
const ENVELOPE_BRIEF_HOOK: &str = include_str!("../../../hooks/envelope-session-brief.sh");

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

/// A JSON string literal, quotes included. Every hand-rolled JSON writer and
/// printed snippet goes through this: a Windows binary path (`C:\Users\…`)
/// interpolated raw into `"…"` is invalid JSON (`\U` is an escape).
fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// A TOML basic-string literal, quotes included — same Windows-path hazard
/// as [`json_str`]: raw `C:\Users\…` inside `"…"` is a TOML unicode-escape
/// error (the issue a Codex field test hit).
fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
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
        // Wire the store that will actually open: on a tepin repo (the 0.7+
        // default, including fresh ones) that's graph.tepin — a graph.db here
        // would trip doctor's path check against the resolved store.
        let db = engram_core::resolve_db_path(&repo.join(".engram/graph.db"))
            .display()
            .to_string();
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
                "bob" => self.wire_bob()?,
                "windsurf" => self.wire_windsurf()?,
                "devin" => self.wire_devin()?,
                other => anyhow::bail!("unknown agent: {other}"),
            }
        }
        say("done — restart your assistant sessions so they pick up the MCP server");
        Ok(())
    }

    fn ensure_gitignore(&self) -> anyhow::Result<()> {
        self.ensure_gitignore_line(".engram/", "Engram local graph (personal)")
    }

    /// Append one line to .gitignore unless it is already there.
    fn ensure_gitignore_line(&self, line: &str, comment: &str) -> anyhow::Result<()> {
        let path = self.repo.join(".gitignore");
        let current = fs::read_to_string(&path).unwrap_or_default();
        if !current.lines().any(|l| l.trim() == line) {
            let mut s = current;
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&format!("\n# {comment}\n{line}\n"));
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
                    return format!("command = {}", toml_str(&self.bin));
                }
                line.to_string()
            })
            .collect();
        repaired.then(|| out.join("\n") + "\n")
    }

    /// The launch arguments an adapter writes. db-less (`["mcp"]`) is the
    /// 0.8.8 default wherever the client launches the server with the
    /// project as its cwd — the bridge binds by MCP roots, then cwd, so the
    /// entry is portable and one shape serves every repo. Adapters whose
    /// launch cwd is unverified (IDEs mostly) keep the explicit --db.
    fn mcp_args_json(&self, with_db: bool) -> String {
        if with_db {
            format!("[\"mcp\", \"--db\", {}]", json_str(&self.db))
        } else {
            "[\"mcp\"]".to_string()
        }
    }

    fn mcp_snippet(&self, with_db: bool) -> String {
        format!(
            "\"engram\": {{ \"command\": {}, \"args\": {} }}",
            json_str(&self.bin),
            self.mcp_args_json(with_db)
        )
    }

    /// Write an `mcpServers`-shaped config, or print the snippet when a
    /// foreign config already exists (never rewrite user JSON blindly).
    fn write_mcp_servers(&self, rel: &str, label: &str, with_db: bool) -> anyhow::Result<()> {
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
                println!("    {}", self.mcp_snippet(with_db));
            }
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &path,
            format!(
                "{{\n  \"mcpServers\": {{\n    \"engram\": {{\n      \"command\": {},\n      \"args\": {}\n    }}\n  }}\n}}\n",
                json_str(&self.bin),
                self.mcp_args_json(with_db)
            ),
        )?;
        say(&format!("{label}: wrote {rel}"));
        Ok(())
    }

    /// Claude Code launches .mcp.json servers with the project as cwd (and
    /// answers MCP roots), so the entry is db-less — portable across
    /// checkouts, and the bridge binds the right graph either way.
    fn wire_claude(&self) -> anyhow::Result<()> {
        self.write_mcp_servers(".mcp.json", "claude", false)?;
        if self.mcp_only {
            return Ok(());
        }
        // The dogfood layout symlinks .claude/skills/engram into the source
        // tree — treat that as "this checkout manages its own wiring" and
        // skip skills and hooks entirely, as setup always has.
        let dir = self.repo.join(".claude/skills/engram");
        if is_symlink(&dir) || is_symlink(&dir.join("SKILL.md")) {
            say("claude: .claude/skills/engram is a symlink — leaving it untouched");
            return Ok(());
        }
        self.install_skills(".claude/skills", "claude")?;
        self.install_claude_brief_hook()
    }

    /// Install the capture-variant and digest skills under `base` (SKILL.md
    /// format shared by Claude Code, Windsurf, and Devin CLI). A symlinked
    /// skill dir (this repo dogfoods that way) points into someone's source
    /// tree — writing through it would clobber the original; symlinks are
    /// left strictly alone.
    fn install_skills(&self, base: &str, label: &str) -> anyhow::Result<()> {
        for (name, body) in [
            ("engram", claude_skill(&self.variant)),
            ("engram-digest", CLAUDE_DIGEST),
        ] {
            let dir = self.repo.join(base).join(name);
            if is_symlink(&dir) || is_symlink(&dir.join("SKILL.md")) {
                say(&format!(
                    "{label}: {base}/{name} is a symlink — leaving it untouched"
                ));
                continue;
            }
            fs::create_dir_all(&dir)?;
            fs::write(dir.join("SKILL.md"), body)?;
        }
        say(&format!(
            "{label}: installed the '{}' + digest skills to {base}",
            self.variant
        ));
        Ok(())
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
                "\n# Engram — durable project memory (db resolves per-repo against the cwd)\n[mcp_servers.engram]\ncommand = {}\nargs = [\"mcp\"]\n",
                toml_str(&self.bin)
            ));
            fs::write(&path, s)?;
            say(
                "codex: registered engram in ~/.codex/config.toml (launch codex from the repo root)",
            );
            say(
                "codex: the desktop app may launch MCP servers from another cwd — if you use it, pin this repo there: add `cwd = \"<repo>\"` or `args = [\"mcp\", \"--db\", \"<repo>/.engram/graph.tepin\"]` to that entry (`engram-alpha doctor` checks this)",
            );
        }
        if !self.mcp_only {
            // Project-scope skills (.codex/skills, SKILL.md format) and the
            // SessionStart brief hook — Codex injects hook context only via
            // the hookSpecificOutput JSON envelope, same contract as Devin.
            self.install_skills(".codex/skills", "codex")?;
            self.install_codex_brief_hook()?;
        }
        self.write_instructions("AGENTS.md")
    }

    /// The Codex SessionStart hook: the envelope scripts under
    /// `.codex/hooks/` and their registration in `.codex/hooks.json`
    /// (project layer). Codex trusts hooks per definition hash, so the
    /// install ends with a one-time `/hooks` review on the user's side; a
    /// foreign hooks file is never rewritten — the snippet is printed
    /// instead. The command resolves the repo root itself because codex
    /// sessions can start in a subdirectory.
    fn install_codex_brief_hook(&self) -> anyhow::Result<()> {
        self.write_envelope_brief_scripts(".codex/hooks")?;

        let registration = r#"{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          {
            "type": "command",
            "command": "\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"/.codex/hooks/engram-brief.sh",
            "statusMessage": "Loading Engram brief"
          }
        ]
      }
    ]
  }
}
"#;
        let config = self.repo.join(".codex/hooks.json");
        if config.exists() {
            let current = fs::read_to_string(&config)?;
            if current.contains("engram-brief") {
                say("codex: .codex/hooks.json already runs the brief hook — leaving it");
            } else {
                say("codex: .codex/hooks.json exists — merge the SessionStart hook from:");
                println!("{registration}");
            }
        } else {
            fs::write(&config, registration)?;
            say("codex: brief hook installed (.codex/hooks + hooks.json)");
        }
        say(
            "codex: project hooks are trust-gated — run `/hooks` inside codex once to review and trust the engram hook (older codex builds also need `codex --enable skills` for .codex/skills)",
        );
        Ok(())
    }

    /// Gemini keeps --db: the CLI is often launched from a subdirectory (its
    /// project config resolves upward), which would make a cwd-bound bridge
    /// bind the subdirectory as a project.
    fn wire_gemini(&self) -> anyhow::Result<()> {
        self.write_mcp_servers(".gemini/settings.json", "gemini", true)?;
        self.write_instructions("GEMINI.md")
    }

    /// Antigravity keeps --db: an IDE's MCP launch cwd is not verified to be
    /// the project root.
    fn wire_antigravity(&self) -> anyhow::Result<()> {
        self.write_mcp_servers(".agents/mcp_config.json", "antigravity", true)?;
        self.write_instructions("AGENTS.md")
    }

    /// Windsurf's Cascade agent reads ONE global MCP config with no
    /// per-project entries, which is exactly why the db-less bridge exists
    /// (issue #4): one global entry, and the bridge follows the editor's
    /// workspace via MCP roots (cwd as the fallback). But WHICH global file
    /// depends on the plugin generation: the JetBrains plugin (mcp-go) reads
    /// `${XDG_CONFIG_HOME:-~/.config}/devin/mcp_config.json` — its own
    /// "edit config" opens that file — while the older generation reads
    /// `~/.devin/mcp_config.json`. Write BOTH with the same merge semantics
    /// (the 0.8.6 dual-artifact lesson in config form) so whichever
    /// generation is installed finds the entry.
    fn wire_windsurf(&self) -> anyhow::Result<()> {
        let xdg_shown = if std::env::var("XDG_CONFIG_HOME").is_ok_and(|v| !v.is_empty()) {
            "$XDG_CONFIG_HOME/devin/mcp_config.json"
        } else {
            "~/.config/devin/mcp_config.json"
        };
        if let Some(path) = windsurf_xdg_config() {
            self.wire_windsurf_file(&path, xdg_shown)?;
        }
        let legacy = home_file(".devin/mcp_config.json").context("no home directory")?;
        self.wire_windsurf_file(&legacy, "~/.devin/mcp_config.json")?;
        say("windsurf: reload Windsurf so Cascade picks up the server");
        if !self.mcp_only {
            // Windsurf has no session hooks, so an always_on rule carries
            // what the Claude hook automates: bind first, then recall
            // (issue #7). Cascade reads .windsurf/skills natively.
            self.write_windsurf_rule()?;
            self.install_skills(".windsurf/skills", "windsurf")?;
        }
        // Cascade reads AGENTS.md, so the capture discipline travels the
        // same shared file the other harnesses use.
        self.write_instructions("AGENTS.md")
    }

    /// The `.windsurf/rules/engram.md` always_on rule: Windsurf's client
    /// advertises MCP roots but answers none and the IntelliJ plugin has no
    /// usable cwd, so the session starts on a fallback binding — the rule's
    /// critical first line makes the agent rebind itself via a scoped
    /// `brief`. The file is engram-namespaced and fully owned: rewritten on
    /// every setup run (symlinks excepted).
    fn write_windsurf_rule(&self) -> anyhow::Result<()> {
        let path = self.repo.join(".windsurf/rules/engram.md");
        if is_symlink(&path) {
            say("windsurf: .windsurf/rules/engram.md is a symlink — leaving it untouched");
            return Ok(());
        }
        fs::create_dir_all(path.parent().expect("rules path has a parent"))?;
        let rule = format!(
            "---\ntrigger: always_on\ndescription: Engram durable project memory — bind \
             the session and recall at session start\n---\n\nCRITICAL — first tool call \
             of every new session: call engram's `brief` with `project` set to this \
             workspace's ABSOLUTE path. Windsurf answers no MCP roots, so the session \
             starts bound to a fallback graph until you rebind it; the call returns \
             this project's memory brief — read it before planning.\n\n{}",
            agent_block(&self.variant)
        );
        fs::write(&path, rule)?;
        say(&format!(
            "windsurf: wrote the always_on rule to .windsurf/rules/engram.md ({})",
            self.variant
        ));
        Ok(())
    }

    /// Devin CLI (issue #8). Three moves: (1) the project-local MCP config
    /// `.devin/mcp_config.local.json` — Devin's highest-precedence tier,
    /// personal by contract (absolute paths), so it carries an explicit
    /// --db and is git-ignored; (2) skills under `.devin/skills` (same
    /// SKILL.md format); (3) a SessionStart hook in `.devin/hooks.v1.json`
    /// injecting the brief — Devin only accepts context through the
    /// hookSpecificOutput JSON envelope, so the hook is a wrapper around
    /// the portable brief script installed next to it.
    fn wire_devin(&self) -> anyhow::Result<()> {
        self.write_mcp_servers(".devin/mcp_config.local.json", "devin", true)?;
        self.ensure_gitignore_line(
            ".devin/mcp_config.local.json",
            "Devin CLI local MCP config (personal, absolute paths)",
        )?;
        if !self.mcp_only {
            self.install_skills(".devin/skills", "devin")?;
            self.install_devin_brief_hook()?;
        }
        self.write_instructions("AGENTS.md")
    }

    /// The two brief-hook scripts for a JSON-envelope harness (Devin, Codex):
    /// the shared portable brief script plus the envelope wrapper, installed
    /// under `<dir>/` as engram-brief-text.sh / engram-brief.sh.
    fn write_envelope_brief_scripts(&self, hooks_dir_rel: &str) -> anyhow::Result<()> {
        let hooks_dir = self.repo.join(hooks_dir_rel);
        fs::create_dir_all(&hooks_dir)?;
        for (name, body) in [
            ("engram-brief-text.sh", SESSION_BRIEF_HOOK),
            ("engram-brief.sh", ENVELOPE_BRIEF_HOOK),
        ] {
            let script = hooks_dir.join(name);
            fs::write(&script, body)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
            }
        }
        Ok(())
    }

    /// The Devin SessionStart hook: the envelope scripts under
    /// `.devin/hooks/` and their registration in `.devin/hooks.v1.json`. A
    /// foreign hooks file is never rewritten — the snippet is printed
    /// instead (same policy as Claude's settings.json).
    fn install_devin_brief_hook(&self) -> anyhow::Result<()> {
        self.write_envelope_brief_scripts(".devin/hooks")?;

        let registration = r#"{
  "SessionStart": [
    {
      "hooks": [
        {
          "type": "command",
          "command": "./.devin/hooks/engram-brief.sh"
        }
      ]
    }
  ]
}
"#;
        let config = self.repo.join(".devin/hooks.v1.json");
        if config.exists() {
            let current = fs::read_to_string(&config)?;
            if current.contains("engram-brief") {
                say("devin: .devin/hooks.v1.json already runs the brief hook — leaving it");
            } else {
                say("devin: .devin/hooks.v1.json exists — merge the SessionStart hook from:");
                println!("{registration}");
            }
            return Ok(());
        }
        fs::write(&config, registration)?;
        say("devin: brief hook installed (.devin/hooks + hooks.v1.json)");
        Ok(())
    }

    /// One Windsurf global config file: merge the db-less engram entry in.
    /// Merging into an existing parseable config is the useful gesture (a
    /// global file usually already lists other servers — same treatment
    /// codex's global TOML gets); other keys survive the serde round-trip
    /// untouched. Unparseable JSON is never rewritten — the snippet is
    /// printed instead.
    fn wire_windsurf_file(&self, path: &Path, shown: &str) -> anyhow::Result<()> {
        let entry = serde_json::json!({ "command": self.bin, "args": ["mcp"] });
        match fs::read_to_string(path) {
            Ok(current) => match serde_json::from_str::<serde_json::Value>(&current) {
                Ok(mut v) if v.is_object() => {
                    if let Some(engram) = v.pointer_mut("/mcpServers/engram") {
                        let cmd = engram.get("command").and_then(|c| c.as_str());
                        if cmd.is_some_and(is_prerename_bin) {
                            // The global repair is db-less on purpose — a
                            // --db here would pin every project to one repo.
                            engram["command"] = serde_json::json!(self.bin);
                            engram["args"] = serde_json::json!(["mcp"]);
                            fs::write(path, serde_json::to_string_pretty(&v)? + "\n")?;
                            say(&format!(
                                "windsurf: re-pointed {shown}'s engram entry at this binary (was the pre-rename `engram`)"
                            ));
                        } else {
                            say(&format!(
                                "windsurf: {shown} already has engram — leaving it"
                            ));
                        }
                    } else {
                        v.as_object_mut()
                            .expect("checked is_object")
                            .entry("mcpServers")
                            .or_insert_with(|| serde_json::json!({}));
                        match v["mcpServers"].as_object_mut() {
                            Some(servers) => {
                                servers.insert("engram".into(), entry);
                                fs::write(path, serde_json::to_string_pretty(&v)? + "\n")?;
                                say(&format!("windsurf: added engram to {shown}"));
                            }
                            None => {
                                say(&format!(
                                    "windsurf: {shown} has a non-object mcpServers — add this to it manually:"
                                ));
                                println!("    {}", self.mcp_snippet(false));
                            }
                        }
                    }
                }
                _ => {
                    say(&format!(
                        "windsurf: {shown} isn't valid JSON — add this to its mcpServers manually:"
                    ));
                    println!("    {}", self.mcp_snippet(false));
                }
            },
            Err(_) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(
                    path,
                    serde_json::to_string_pretty(&serde_json::json!({
                        "mcpServers": { "engram": entry }
                    }))? + "\n",
                )?;
                say(&format!(
                    "windsurf: wrote {shown} (global — one db-less entry serves every project via MCP roots)"
                ));
            }
        }
        Ok(())
    }

    /// IBM Bob IDE and BobShell share the `~/.bob/` home directory.
    /// - Bob IDE global: `~/.bob/mcp.json`  (mcpServers shape)
    /// - BobShell global: `~/.bob/mcp_settings.json`  (same mcpServers shape)
    /// - Both: project-level `.bob/mcp.json` takes precedence when names collide.
    ///
    /// This writes the project-level file only (the safe default for a per-repo
    /// setup). To wire globally: add the entry to `~/.bob/mcp.json` (IDE) or
    /// `~/.bob/mcp_settings.json` (BobShell) by hand.
    /// Bob has no agent-harness hooks, so the AGENTS.md instruction block
    /// (which Bob's /init flow reads) carries the recall discipline.
    fn wire_bob(&self) -> anyhow::Result<()> {
        // Bob keeps --db: the IDE's MCP launch cwd is not verified to be the
        // project root (and BobShell is unverified live altogether).
        self.write_mcp_servers(".bob/mcp.json", "bob", true)?;
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
                    "    \"engram\": {{ \"type\": \"local\", \"command\": [{}, \"mcp\", \"--db\", {}], \"enabled\": true }}",
                    json_str(&self.bin),
                    json_str(&self.db)
                );
            }
        } else {
            fs::write(
                &path,
                format!(
                    "{{\n  \"mcp\": {{\n    \"engram\": {{\n      \"type\": \"local\",\n      \"command\": [{}, \"mcp\", \"--db\", {}],\n      \"enabled\": true\n    }}\n  }}\n}}\n",
                    json_str(&self.bin),
                    json_str(&self.db)
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

    /// The Windows field bug (2026-08-30): a raw `C:\Users\…` interpolated
    /// into a double-quoted TOML or JSON string is an invalid escape (`\U`),
    /// and Codex refused its whole config. Every writer and printed snippet
    /// must emit paths through an escaping encoder, and the emitted document
    /// must parse back to the exact path.
    #[test]
    fn windows_paths_survive_every_config_encoding() {
        let bin = r"C:\Users\apl20\AppData\Local\Engram\bin\engram-alpha.exe";
        let db = r"C:\Users\apl20\proj\.engram\graph.tepin";
        let tmp = std::env::temp_dir().join(format!("engram-setup-win-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let s = Setup {
            repo: tmp.clone(),
            bin: bin.into(),
            db: db.into(),
            variant: "relaxed".into(),
            mcp_only: true,
        };

        // The string encoders round-trip through real parsers.
        assert_eq!(
            serde_json::from_str::<String>(&json_str(bin)).unwrap(),
            bin,
            "json_str must produce a parseable JSON literal"
        );
        assert_eq!(
            toml_str(bin),
            r#""C:\\Users\\apl20\\AppData\\Local\\Engram\\bin\\engram-alpha.exe""#,
            "toml_str escapes every backslash"
        );

        // mcpServers writers (claude/gemini/devin/bob shape): the written
        // file is valid JSON and the paths come back byte-identical.
        s.write_mcp_servers("mcp.json", "test", true).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(tmp.join("mcp.json")).unwrap())
                .expect("written mcpServers config parses");
        assert_eq!(v["mcpServers"]["engram"]["command"], bin);
        assert_eq!(v["mcpServers"]["engram"]["args"][2], db);

        // opencode/kilo shape.
        s.wire_mcp_array("opencode.json", "test").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(tmp.join("opencode.json")).unwrap())
                .expect("written mcp-array config parses");
        assert_eq!(v["mcp"]["engram"]["command"][0], bin);
        assert_eq!(v["mcp"]["engram"]["command"][3], db);

        // The printed manual-add snippet is itself valid JSON.
        let snippet = format!("{{{}}}", s.mcp_snippet(true));
        let v: serde_json::Value = serde_json::from_str(&snippet).expect("snippet parses as JSON");
        assert_eq!(v["engram"]["command"], bin);

        // The codex TOML repair writes an escaped command line.
        let raw = "[mcp_servers.engram]\ncommand = \"/old/engram\"\n";
        let fixed = s.repaired_codex_toml(raw).unwrap();
        assert!(
            fixed.contains(&format!("command = {}", toml_str(bin))),
            "repair must escape the Windows path: {fixed}"
        );

        let _ = fs::remove_dir_all(&tmp);
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
