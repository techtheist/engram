//! Per-assistant wiring: MCP entry, instructions, skills and rules.

use super::*;

impl Setup {
    /// Claude Code launches .mcp.json servers with the project as cwd (and
    /// answers MCP roots), so the entry is db-less — portable across
    /// checkouts, and the bridge binds the right graph either way.
    pub(super) fn wire_claude(&self) -> anyhow::Result<()> {
        if claude_plugin_installed() {
            // The plugin serves MCP globally (`mcp --wired-only`, 0.9.9) —
            // a project `.mcp.json` entry would load every engram tool a
            // second time under another name.
            say(
                "claude: the Engram Claude Code plugin is installed and serves MCP in every \
                 wired repo — no project .mcp.json entry written",
            );
            if mcp_json_has_engram(&self.repo) {
                say(
                    "claude: .mcp.json still registers an `engram` server — remove that entry, \
                     or Claude Code loads the engram tools twice",
                );
            }
        } else {
            self.write_mcp_servers(".mcp.json", "claude", false)?;
        }
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

    /// Codex's MCP config is global (`~/.codex/config.toml`) and shared by the
    /// CLI, the IDE extension, and the Codex/ChatGPT desktop app — the app
    /// ignores project-local config entirely (openai/codex#13025). No --db, so
    /// the graph resolves against the cwd: one entry serves every repo when
    /// codex is launched from the repo root.
    pub(super) fn wire_codex(&self) -> anyhow::Result<()> {
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

    /// Gemini keeps --db: the CLI is often launched from a subdirectory (its
    /// project config resolves upward), which would make a cwd-bound bridge
    /// bind the subdirectory as a project.
    pub(super) fn wire_gemini(&self) -> anyhow::Result<()> {
        self.write_mcp_servers(".gemini/settings.json", "gemini", true)?;
        self.write_instructions("GEMINI.md")
    }

    /// Antigravity keeps --db: an IDE's MCP launch cwd is not verified to be
    /// the project root.
    pub(super) fn wire_antigravity(&self) -> anyhow::Result<()> {
        self.write_mcp_servers(".agents/mcp_config.json", "antigravity", true)?;
        self.write_instructions("AGENTS.md")
    }

    /// Windsurf reads one global MCP config, so it gets the db-less bridge, which
    /// follows the workspace via MCP roots (issue #4). The JetBrains plugin reads
    /// `${XDG_CONFIG_HOME:-~/.config}/devin/mcp_config.json`, the older generation
    /// `~/.devin/mcp_config.json`; both are written with the same merge.
    pub(super) fn wire_windsurf(&self) -> anyhow::Result<()> {
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

    /// The engram-owned `.windsurf/rules/engram.md` always-on rule: Windsurf
    /// answers no MCP roots, so its first line has the agent rebind via a scoped
    /// `brief`. Rewritten on every run (symlinks excepted).
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
             this project's memory brief — read it before planning.\n\nVERIFY every \
             `brief` reply: its first line names the graph. If it says the home graph, \
             a fallback, or a project that is not this workspace, rebind with the same \
             call before doing anything else. This applies even when you believe you \
             already bound the session: a resumed conversation, an IDE restart or an MCP \
             reload starts a NEW engram session on the fallback binding. An unbound \
             session refuses writes with \"bound to the home graph by fallback\" — that \
             error means: rebind, then retry the write. Every write verdict names the \
             `project` it landed in; read it.\n\n{}",
            agent_block(&self.variant)
        );
        fs::write(&path, rule)?;
        say(&format!(
            "windsurf: wrote the always_on rule to .windsurf/rules/engram.md ({})",
            self.variant
        ));
        Ok(())
    }

    /// Devin CLI (issue #8): `.devin/mcp_config.local.json` with an explicit --db
    /// (personal, git-ignored), skills under `.devin/skills`, and a SessionStart
    /// hook in `.devin/hooks.v1.json` wrapping the brief in Devin's JSON envelope.
    pub(super) fn wire_devin(&self) -> anyhow::Result<()> {
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

    /// Cursor: `.cursor/mcp.json` with an explicit --db (roots aren't promised;
    /// git-ignored when created), skills under `.cursor/skills` unless
    /// `.claude/skills/engram` exists (Cursor reads it too), and a `sessionStart`
    /// hook in `.cursor/hooks.json`. Capture discipline travels in AGENTS.md.
    pub(super) fn wire_cursor(&self) -> anyhow::Result<()> {
        let mcp = self.repo.join(".cursor/mcp.json");
        let created = !mcp.exists();
        self.write_mcp_servers(".cursor/mcp.json", "cursor", true)?;
        if created {
            self.ensure_gitignore_line(
                ".cursor/mcp.json",
                "Cursor project MCP config (personal, absolute paths)",
            )?;
        }
        if !self.mcp_only {
            if self.repo.join(".claude/skills/engram").exists() {
                say("cursor: Cursor reads .claude/skills — the engram skill there serves it");
            } else {
                self.install_skills(".cursor/skills", "cursor")?;
            }
            self.install_cursor_brief_hook()?;
        }
        self.write_instructions("AGENTS.md")
    }

    /// IBM Bob IDE and BobShell: writes the project-level `.bob/mcp.json`, which
    /// wins over the globals (`~/.bob/mcp.json`, `~/.bob/mcp_settings.json`; wire
    /// those by hand). Bob 2.0.2+ runs a SessionStart hook from `.bob/settings.json`
    /// whose stdout becomes context, so the brief script runs directly; AGENTS.md
    /// covers older installs.
    pub(super) fn wire_bob(&self) -> anyhow::Result<()> {
        // Bob keeps --db: the IDE's MCP launch cwd is not verified to be the
        // project root (and BobShell is unverified live altogether).
        self.write_mcp_servers(".bob/mcp.json", "bob", true)?;
        if !self.mcp_only {
            self.install_bob_brief_hook()?;
        }
        self.write_instructions("AGENTS.md")
    }
}
