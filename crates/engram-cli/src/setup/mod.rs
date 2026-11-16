//! `engram-alpha setup` — wire the current repository for AI assistants: MCP
//! registration + capture instructions, all from assets embedded in the
//! binary. The shell installers only fetch the binary;
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

mod agents;
mod hooks;
mod mcp_config;

pub use mcp_config::*;

// Claude Code skill variants (full SKILL.md files).
const CLAUDE_RELAXED: &str = include_str!("../../../../skills/engram/relaxed/SKILL.md");
const CLAUDE_NORMAL: &str = include_str!("../../../../skills/engram/normal/SKILL.md");
const CLAUDE_AGGRESSIVE: &str = include_str!("../../../../skills/engram/aggressive/SKILL.md");
// The digest skill has no variants: one explicit-invocation
// ingestion doc, installed alongside whichever capture variant was chosen.
pub const CLAUDE_DIGEST: &str = include_str!("../../../../skills/engram/digest/SKILL.md");
// The sleep skill: on-demand consolidation of a crowded graph, no
// variants either.
pub const CLAUDE_SLEEP: &str = include_str!("../../../../skills/engram/sleep/SKILL.md");
// Harness-neutral variants for AGENTS.md / GEMINI.md / rules files.
const AGENT_RELAXED: &str = include_str!("../../../../skills/engram/agents/relaxed.md");
const AGENT_NORMAL: &str = include_str!("../../../../skills/engram/agents/normal.md");
const AGENT_AGGRESSIVE: &str = include_str!("../../../../skills/engram/agents/aggressive.md");
// SessionStart hook: injects the brief so sessions start pre-briefed.
const SESSION_BRIEF_HOOK: &str = include_str!("../../../../hooks/session-brief.sh");
const FILE_READ_MATCH_HOOK: &str = include_str!("../../../../hooks/file-read-match.sh");
// Envelope wrapper: Devin CLI and Codex CLI inject SessionStart context only
// via the hookSpecificOutput JSON envelope, so both get this wrapper around
// the portable script above.
const ENVELOPE_BRIEF_HOOK: &str = include_str!("../../../../hooks/envelope-session-brief.sh");

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
        self.ensure_graph_dir()?;
        let unique: BTreeSet<&str> = agents.iter().copied().collect();
        for &agent in &unique {
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
                "cursor" => self.wire_cursor()?,
                other => anyhow::bail!("unknown agent: {other}"),
            }
        }
        // Copies this run didn't touch (other harnesses wired earlier) follow
        // the binary too; edited ones are reported and kept.
        let found = crate::skillsync::refresh(&self.repo, crate::skillsync::Mode::All);
        for line in crate::skillsync::report(&self.repo, &found) {
            say(&line);
        }
        // What was wired, by which binary, in which variant (issues #18/#19).
        let variant = (!self.mcp_only).then_some(self.variant.as_str());
        let wired: Vec<&str> = unique.iter().copied().collect();
        crate::upkeep::record_setup(&self.repo, &wired, variant, &found);
        say("done — restart your assistant sessions so they pick up the MCP server");
        Ok(())
    }

    /// A wired repo holds `.engram/` from the moment it is wired:
    /// that directory is what a `--wired-only` bridge — the Claude Code
    /// plugin's global server — reads as "this folder is an Engram project".
    /// The store inside is the core's to create on first open. Never in `/`
    /// or the home directory, which can't host a project.
    fn ensure_graph_dir(&self) -> anyhow::Result<()> {
        let canon = self
            .repo
            .canonicalize()
            .unwrap_or_else(|_| self.repo.clone());
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .map(|h| h.canonicalize().unwrap_or(h));
        if canon.parent().is_none() || home.as_ref() == Some(&canon) {
            return Ok(());
        }
        fs::create_dir_all(self.repo.join(".engram"))?;
        Ok(())
    }

    fn ensure_gitignore(&self) -> anyhow::Result<()> {
        self.ensure_gitignore_line(".engram/", "Engram local graph (personal)")
    }

    /// Append one line to .gitignore unless the path is already ignored.
    fn ensure_gitignore_line(&self, line: &str, comment: &str) -> anyhow::Result<()> {
        if crate::gitignore::is_ignored(&self.repo, line) {
            return Ok(());
        }
        let path = self.repo.join(".gitignore");
        let mut s = fs::read_to_string(&path).unwrap_or_default();
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&format!("\n# {comment}\n{line}\n"));
        fs::write(&path, s)?;
        Ok(())
    }

    /// Insert or refresh the marked instruction section in an AGENTS.md-style
    /// file. Re-running with a different --skill replaces the section.
    fn write_instructions(&self, file: &str) -> anyhow::Result<()> {
        if self.mcp_only {
            return Ok(());
        }
        let path = self.repo.join(file);
        let block = crate::skillsync::stamped_block(&self.variant);
        let current = fs::read_to_string(&path).unwrap_or_default();
        let next = match crate::skillsync::replace_block(&current, &block) {
            Ok(Some(next)) => next,
            Ok(None) => {
                let mut s = current;
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                if !s.is_empty() {
                    s.push('\n');
                }
                s + &block
            }
            Err(()) => {
                say(&format!(
                    "{file}: the engram section was edited — kept as it is (remove it to get the current one)"
                ));
                return Ok(());
            }
        };
        fs::write(&path, next)?;
        say(&format!(
            "{file}: engram section in place ({})",
            self.variant
        ));
        Ok(())
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
            ("engram-sleep", CLAUDE_SLEEP),
        ] {
            let dir = self.repo.join(base).join(name);
            if is_symlink(&dir) || is_symlink(&dir.join("SKILL.md")) {
                say(&format!(
                    "{label}: {base}/{name} is a symlink — leaving it untouched"
                ));
                continue;
            }
            let path = dir.join("SKILL.md");
            let kind = match name {
                "engram" => "engram",
                "engram-sleep" => "sleep",
                _ => "digest",
            };
            if crate::skillsync::is_edited(&path, kind) {
                say(&format!(
                    "{label}: {base}/{name} was edited — kept as it is (delete it to get the current text)"
                ));
                continue;
            }
            fs::create_dir_all(&dir)?;
            let variant = if kind == "engram" {
                self.variant.as_str()
            } else {
                kind
            };
            fs::write(&path, crate::skillsync::stamp_skill(body, kind, variant))?;
        }
        say(&format!(
            "{label}: installed the '{}' + digest + sleep skills to {base}",
            self.variant
        ));
        Ok(())
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

    /// The Bob brief hook installer's three branches: a fresh repo gets the
    /// script (executable) plus a parseable Claude-Code-shaped registration
    /// with the `sh`-prefixed workspace-relative command and an explicit
    /// timeout; a re-run leaves an already-wired file byte-identical; a
    /// foreign `.bob/settings.json` is never rewritten (the snippet is
    /// printed instead) while the script is still installed.
    #[test]
    fn bob_brief_hook_installs_once_and_never_clobbers_foreign_settings() {
        let tmp = std::env::temp_dir().join(format!("engram-setup-bob-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let s = Setup {
            repo: tmp.clone(),
            bin: "/new/engram-alpha".into(),
            db: tmp
                .join(".engram/graph.tepin")
                .to_string_lossy()
                .into_owned(),
            variant: "relaxed".into(),
            mcp_only: false,
        };

        s.install_bob_brief_hook().unwrap();
        let script = tmp.join(".bob/hooks/engram-brief.sh");
        assert_eq!(
            fs::read_to_string(&script).unwrap(),
            SESSION_BRIEF_HOOK,
            "bob runs the portable script verbatim — no envelope wrapper"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(&script).unwrap().permissions().mode() & 0o111,
                0,
                "script is executable"
            );
        }
        let settings = tmp.join(".bob/settings.json");
        let first = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&first).expect("registration parses");
        let hook = &v["hooks"]["SessionStart"][0]["hooks"][0];
        assert_eq!(hook["type"], "command");
        assert_eq!(hook["command"], "sh .bob/hooks/engram-brief.sh");
        assert!(
            hook["timeout"].as_u64().is_some_and(|t| t > 10),
            "timeout must be explicit and above Bob's 10s default"
        );

        // Re-run: already wired, untouched.
        s.install_bob_brief_hook().unwrap();
        assert_eq!(fs::read_to_string(&settings).unwrap(), first, "idempotent");

        // Foreign settings: preserved byte-for-byte, script still installed.
        let foreign = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo bye"}]}]}}"#;
        fs::write(&settings, foreign).unwrap();
        fs::remove_file(&script).unwrap();
        s.install_bob_brief_hook().unwrap();
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            foreign,
            "foreign file untouched"
        );
        assert!(script.exists(), "script installed regardless");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn plugin_skill_matches_relaxed_variant() {
        let plugin = include_str!("../../../../claude-plugin/skills/engram/SKILL.md");
        assert_eq!(
            plugin, CLAUDE_RELAXED,
            "claude-plugin/skills/engram/SKILL.md drifted from skills/engram/relaxed/SKILL.md — re-copy it"
        );
    }

    #[test]
    fn plugin_digest_skill_matches_canonical() {
        let plugin = include_str!("../../../../claude-plugin/skills/engram-digest/SKILL.md");
        assert_eq!(
            plugin, CLAUDE_DIGEST,
            "claude-plugin/skills/engram-digest/SKILL.md drifted from skills/engram/digest/SKILL.md — re-copy it"
        );
    }

    #[test]
    fn plugin_sleep_skill_matches_canonical() {
        let plugin = include_str!("../../../../claude-plugin/skills/engram-sleep/SKILL.md");
        assert_eq!(
            plugin, CLAUDE_SLEEP,
            "claude-plugin/skills/engram-sleep/SKILL.md drifted from skills/engram/sleep/SKILL.md — re-copy it"
        );
    }

    #[test]
    fn plugin_hook_matches_canonical_script() {
        let plugin = include_str!("../../../../claude-plugin/hooks/session-brief.sh");
        assert_eq!(
            plugin, SESSION_BRIEF_HOOK,
            "claude-plugin/hooks/session-brief.sh drifted from hooks/session-brief.sh — re-copy it"
        );
        let refs = include_str!("../../../../claude-plugin/hooks/file-read-match.sh");
        assert_eq!(
            refs, FILE_READ_MATCH_HOOK,
            "claude-plugin/hooks/file-read-match.sh drifted from hooks/file-read-match.sh — re-copy it"
        );
    }

    #[test]
    fn plugin_manifests_parse() {
        for raw in [
            include_str!("../../../../claude-plugin/.claude-plugin/plugin.json"),
            include_str!("../../../../.claude-plugin/marketplace.json"),
            include_str!("../../../../claude-plugin/hooks/hooks.json"),
            include_str!("../../../../claude-plugin/.mcp.json"),
        ] {
            serde_json::from_str::<serde_json::Value>(raw).expect("plugin manifest is valid JSON");
        }
    }

    // The plugin's global MCP server must launch the wired-only
    // bridge through a script that exists and is executable — a plugin that
    // runs a plain `mcp` would turn every folder Claude Code opens into a
    // project.
    #[test]
    fn plugin_mcp_server_is_the_wired_only_bridge() {
        let mcp: serde_json::Value =
            serde_json::from_str(include_str!("../../../../claude-plugin/.mcp.json")).unwrap();
        let command = mcp["mcpServers"]["engram"]["command"].as_str().unwrap();
        let rel = command
            .strip_prefix("${CLAUDE_PLUGIN_ROOT}/")
            .expect("the command lives inside the plugin");
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../claude-plugin")
            .join(rel);
        let body = std::fs::read_to_string(&script).expect("launcher exists");
        assert!(
            body.contains("mcp --wired-only"),
            "launcher runs the wired-only bridge"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&script).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "{} must be executable", script.display());
        }
    }

    // The plugin installs from the repo (not from release artifacts), so its
    // checked-in version must move with the workspace version by hand — this
    // makes a release-prep bump of Cargo.toml fail until plugin.json follows.
    #[test]
    fn plugin_version_matches_workspace() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../claude-plugin/.claude-plugin/plugin.json"
        ))
        .unwrap();
        assert_eq!(
            manifest["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "claude-plugin/.claude-plugin/plugin.json version drifted from [workspace.package] version"
        );
    }
}
