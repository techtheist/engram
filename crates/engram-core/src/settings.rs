//! Machine-level settings: `~/.engram/settings.json`, next to the registry —
//! the same file family as `daemon.json`/`registry.json` (plain JSON,
//! inspectable with `cat`, atomic writes, last-write-wins). Per-graph knobs
//! belong in [`crate::config::GraphConfig`]; this file is for the handful of
//! settings that describe the MACHINE, not any one graph.
//!
//! First (and so far only) setting: `default_agent_project` — which project a
//! db-less MCP session binds when nothing else names one (no `--db`, no
//! usable MCP root, no usable launch cwd). Unset = the home graph, exactly
//! the pre-setting behavior. The core serves it over loopback-only
//! `GET/POST /settings`; the pane's Settings surface edits it. Changing it
//! never rebinds already-connected sessions — it is read at bind time.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, Result, registry};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Registered project (stored as its stable id) an otherwise-unbindable
    /// agent session gets. `None` = the home graph (today's behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent_project: Option<String>,
    /// Settings this binary doesn't know about (a newer one's) ride along
    /// unharmed through a load-edit-save cycle.
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

/// `~/.engram/settings.json` (`ENGRAM_HOME` override honored, like the
/// registry).
pub fn settings_path() -> Option<PathBuf> {
    registry::engram_home().map(|d| d.join("settings.json"))
}

/// Load the machine settings; a missing or unreadable file is the default —
/// settings are an upgrade, never a dependency.
pub fn load() -> Settings {
    settings_path().map(|p| load_from(&p)).unwrap_or_default()
}

/// Persist the machine settings (atomic write, like the registry).
pub fn save(settings: &Settings) -> Result<()> {
    let path = settings_path().ok_or_else(|| Error::Io("no home directory".into()))?;
    save_to(&path, settings)
}

fn load_from(path: &std::path::Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_to(path: &std::path::Path, settings: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::Io(format!("creating {}: {e}", dir.display())))?;
    }
    let body = serde_json::to_string_pretty(settings)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{body}\n"))
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| Error::Io(format!("writing {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_default_and_roundtrip_preserves_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("engram-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");

        assert_eq!(load_from(&path), Settings::default());

        // A newer binary wrote a key this one doesn't know — editing the
        // known field must not drop it.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"default_agent_project":"abc","future_knob":true}"#,
        )
        .unwrap();
        let mut s = load_from(&path);
        assert_eq!(s.default_agent_project.as_deref(), Some("abc"));
        s.default_agent_project = None;
        save_to(&path, &s).unwrap();
        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back["future_knob"], serde_json::json!(true));
        assert!(back.get("default_agent_project").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
