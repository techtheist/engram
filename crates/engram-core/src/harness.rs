//! Assistant-harness probes: which AI CLIs/apps are installed on this
//! machine, and whether a repo is wired to them. Shared by `engram-alpha
//! setup`/`doctor` (terminal) and the daemon's `/system` endpoint (pane) —
//! one implementation of "what talks to this graph".

use std::path::{Path, PathBuf};

use serde::Serialize;

pub const AGENTS: [&str; 6] = [
    "claude",
    "codex",
    "gemini",
    "opencode",
    "kilo",
    "antigravity",
];

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
    if on_path("codex") || dir(".codex") || codex_app_installed() {
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

/// The Codex desktop app (merged into the unified ChatGPT app mid-2026)
/// shares `~/.codex/config.toml` and AGENTS.md discovery with the CLI, so it
/// counts as Codex being installed. macOS keeps bundle id com.openai.codex
/// under either app name; on Windows the app lands in %LOCALAPPDATA%\OpenAI.
fn codex_app_installed() -> bool {
    if cfg!(target_os = "macos") {
        return Path::new("/Applications/Codex.app").exists()
            || Path::new("/Applications/ChatGPT.app").exists();
    }
    if cfg!(target_os = "windows") {
        return std::env::var("LOCALAPPDATA")
            .is_ok_and(|d| Path::new(&d).join("OpenAI").join("Codex").exists());
    }
    false
}

pub fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let base = dir.join(bin);
        base.exists() || base.with_extension("exe").exists() || base.with_extension("cmd").exists()
    })
}

/// Is this repo already wired for the given agent? Cheap file probes only.
pub fn is_wired(repo: &Path, agent: &str) -> bool {
    let has_engram = |p: PathBuf| std::fs::read_to_string(p).is_ok_and(|s| s.contains("engram"));
    match agent {
        "claude" => has_engram(repo.join(".mcp.json")),
        "codex" => home_file(".codex/config.toml").is_some_and(|p| {
            std::fs::read_to_string(p).is_ok_and(|s| s.contains("[mcp_servers.engram]"))
        }),
        "gemini" => has_engram(repo.join(".gemini/settings.json")),
        "opencode" => has_engram(repo.join("opencode.json")),
        "kilo" => has_engram(repo.join("kilo.json")),
        "antigravity" => has_engram(repo.join(".agents/mcp_config.json")),
        _ => false,
    }
}

pub fn home_file(rel: &str) -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| PathBuf::from(h).join(rel))
}

/// Does this configured command launch the pre-rename binary? (v0.4.0 renamed
/// `engram` → `engram-alpha`; setup and doctor treat old-name wiring as
/// repairable, not "already wired".)
pub fn is_prerename_bin(cmd: &str) -> bool {
    matches!(
        Path::new(cmd).file_name().and_then(|n| n.to_str()),
        Some("engram") | Some("engram.exe")
    )
}

/// Does `.mcp.json` launch the pre-rename `engram` binary? (Re-running setup
/// repairs the entry in place.)
pub fn prerename_mcp_json(raw: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .is_some_and(|v| {
            v.pointer("/mcpServers/engram/command")
                .and_then(|c| c.as_str())
                .is_some_and(is_prerename_bin)
        })
}

/// Same check for codex's `[mcp_servers.engram]` command line.
pub fn prerename_codex_toml(raw: &str) -> bool {
    raw.lines()
        .skip_while(|l| l.trim() != "[mcp_servers.engram]")
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .any(|l| {
            let t = l.trim();
            t.starts_with("command") && t.split('"').nth(1).is_some_and(is_prerename_bin)
        })
}

/// One detected assistant's wiring state for a repo — the structured form
/// behind doctor's wiring section and the pane's System info.
#[derive(Debug, Serialize)]
pub struct WiringStatus {
    pub agent: &'static str,
    pub wired: bool,
    /// Wired, but launching the pre-rename `engram` binary.
    pub prerename: bool,
}

/// Wiring state of every *detected* assistant for this repo.
pub fn wiring(repo: &Path) -> Vec<WiringStatus> {
    detect_agents()
        .into_iter()
        .map(|agent| {
            let wired = is_wired(repo, agent);
            let prerename = wired
                && match agent {
                    "claude" => std::fs::read_to_string(repo.join(".mcp.json"))
                        .is_ok_and(|raw| prerename_mcp_json(&raw)),
                    "codex" => home_file(".codex/config.toml")
                        .and_then(|p| std::fs::read_to_string(p).ok())
                        .is_some_and(|raw| prerename_codex_toml(&raw)),
                    _ => false,
                };
            WiringStatus {
                agent,
                wired,
                prerename,
            }
        })
        .collect()
}
