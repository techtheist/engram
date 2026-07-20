//! The machine-level project registry (PLAN §7C): `~/.engram/registry.json`,
//! one entry per known project. Every `serve`/`mcp`/`setup` run registers its
//! repo here — this file is how any project becomes aware the others exist.
//! First-class and obvious by design: plain JSON, inspectable with `cat`,
//! listed by `engram-alpha projects`-style surfaces and the pane.
//!
//! Stale entries are harmless — readers health-check a project's DB before
//! trusting it (the `daemon.json` rule). Last-write-wins on the file itself:
//! registration is idempotent upsert-by-root, so two daemons racing at worst
//! re-write each other's `last_seen`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Reserved selector: the user-level home graph (`~/.engram/home.db`).
pub const HOME_PROJECT: &str = "home";
/// Reserved selector: every registered project — reads only, never writes.
pub const ALL_PROJECTS: &str = "all";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// Stable id, minted once at first registration (URLs use this).
    pub id: String,
    /// Unique human slug (dir basename, deduped) — MCP accepts either.
    pub name: String,
    /// Absolute repo root.
    pub root: String,
    /// Absolute DB path.
    pub db: String,
    /// Last registration touch (a daemon/mcp/setup run), unix seconds.
    pub last_seen: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
}

impl Registry {
    /// Find one project by id or slug name. Reserved names never resolve here.
    pub fn resolve(&self, selector: &str) -> Option<&ProjectEntry> {
        self.projects
            .iter()
            .find(|p| p.id == selector || p.name == selector)
    }
}

/// The engram home dir (`ENGRAM_HOME` override for tests, else `~/.engram`).
pub fn engram_home() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ENGRAM_HOME") {
        return Some(PathBuf::from(dir));
    }
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| Path::new(&h).join(".engram"))
}

pub fn registry_path() -> Option<PathBuf> {
    engram_home().map(|d| d.join("registry.json"))
}

/// Where the user-level home graph lives (PLAN §7C).
pub fn home_db_path() -> Option<PathBuf> {
    engram_home().map(|d| d.join("home.db"))
}

/// Load the registry; a missing or unreadable file is an empty registry, not
/// an error — awareness of other projects is an upgrade, never a dependency.
pub fn load() -> Registry {
    registry_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Register (or refresh) a project by its repo root. Idempotent upsert: an
/// existing root keeps its id and name; a new one gets a fresh id and a
/// deduped slug. Returns the entry as written.
pub fn register(root: &Path, db: &Path) -> Result<ProjectEntry> {
    let root = root
        .canonicalize()
        .map_err(|e| Error::Io(format!("resolving {}: {e}", root.display())))?;
    let db = db.canonicalize().unwrap_or_else(|_| db.to_path_buf());
    let mut reg = load();
    let now = crate::store::now();

    let entry = match reg.projects.iter_mut().find(|p| Path::new(&p.root) == root) {
        Some(existing) => {
            existing.db = db.display().to_string();
            existing.last_seen = now;
            existing.clone()
        }
        None => {
            let name = unique_slug(&reg, &root);
            let entry = ProjectEntry {
                id: crate::id::new_id(),
                name,
                root: root.display().to_string(),
                db: db.display().to_string(),
                last_seen: now,
            };
            reg.projects.push(entry.clone());
            entry
        }
    };
    save(&reg)?;
    Ok(entry)
}

/// Drop one project from the registry (the data itself is untouched — this
/// only withdraws awareness).
pub fn unregister(selector: &str) -> Result<bool> {
    let mut reg = load();
    let before = reg.projects.len();
    reg.projects
        .retain(|p| p.id != selector && p.name != selector);
    let removed = reg.projects.len() < before;
    if removed {
        save(&reg)?;
    }
    Ok(removed)
}

fn save(reg: &Registry) -> Result<()> {
    let path = registry_path().ok_or_else(|| Error::Io("no home directory".into()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::Io(format!("creating {}: {e}", dir.display())))?;
    }
    let body = serde_json::to_string_pretty(reg)?;
    // Atomic write: a reader never sees a torn file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{body}\n"))
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| Error::Io(format!("writing {}: {e}", path.display())))
}

/// Kebab slug of the repo dir name, suffixed `-2`, `-3`… on collision with a
/// different root. Reserved names are never assignable.
fn unique_slug(reg: &Registry, root: &Path) -> String {
    let base = slug(
        root.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "project".into())
            .as_str(),
    );
    let taken = |candidate: &str| {
        candidate == HOME_PROJECT
            || candidate == ALL_PROJECTS
            || reg.projects.iter().any(|p| p.name == candidate)
    };
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|i| format!("{base}-{i}"))
        .find(|c| !taken(c))
        .expect("an unbounded range always yields a free slug")
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "project".into()
    } else {
        trimmed.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_kebab_and_never_reserved() {
        assert_eq!(slug("My Project!"), "my-project");
        assert_eq!(slug("engram"), "engram");
        assert_eq!(slug("---"), "project");
        let reg = Registry::default();
        // A repo literally named "home" must not shadow the reserved selector.
        let dir = std::env::temp_dir().join("home");
        assert_eq!(unique_slug(&reg, &dir), "home-2");
    }

    #[test]
    fn resolve_matches_id_or_name() {
        let reg = Registry {
            projects: vec![ProjectEntry {
                id: "abc123".into(),
                name: "engram".into(),
                root: "/tmp/engram".into(),
                db: "/tmp/engram/.engram/graph.db".into(),
                last_seen: 0,
            }],
        };
        assert!(reg.resolve("abc123").is_some());
        assert!(reg.resolve("engram").is_some());
        assert!(reg.resolve("nope").is_none());
    }
}
