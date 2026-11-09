//! The per-project setup record (0.11.1, issues #18 and #19):
//! `<repo>/.engram/setup.json` says which binary last wired this repo, for
//! which assistants and in which skill variant, and what the core's last
//! upkeep pass held back. `setup` writes it; the core writes the upkeep half
//! when a bridge first binds the project; `doctor`, `update` and the brief
//! read it. It lives beside the graph in the git-ignored `.engram/`, so it is
//! personal like the graph.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name inside the repo's `.engram/` directory.
pub const FILE: &str = "setup.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SetupRecord {
    /// The binary version that last ran `setup` here. `None` when the record
    /// was built by upkeep from what it found wired (a repo set up before
    /// 0.11.1): hooks and rules are of unknown age.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_version: Option<String>,
    /// Assistants wired here, sorted.
    #[serde(default)]
    pub agents: Vec<String>,
    /// The skill variant last installed (relaxed | normal | aggressive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// The binary version of the last upkeep pass over the installed skills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upkeep_version: Option<String>,
    /// Unix seconds of the last upkeep pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upkeep_at: Option<i64>,
    /// Outdated copies the last upkeep pass did not rewrite because git
    /// tracks them — `setup --refresh` updates them on purpose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<HeldCopy>,
}

/// One installed copy upkeep left alone because git tracks it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldCopy {
    /// Repo-relative path.
    pub path: String,
    /// The version that installed it ("pre-0.9.12" for unstamped copies).
    pub from: String,
}

pub fn path(repo: &Path) -> PathBuf {
    repo.join(".engram").join(FILE)
}

/// The record, if this repo has one that parses.
pub fn load(repo: &Path) -> Option<SetupRecord> {
    let raw = std::fs::read_to_string(path(repo)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write the record. Refuses to create `.engram/` — a folder that has none
/// is not an Engram project.
pub fn save(repo: &Path, record: &SetupRecord) -> std::io::Result<()> {
    let dir = repo.join(".engram");
    if !dir.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} is not an Engram project (no .engram/)", repo.display()),
        ));
    }
    let mut text = serde_json::to_string_pretty(record).map_err(std::io::Error::other)?;
    text.push('\n');
    std::fs::write(path(repo), text)
}

/// The brief's line about held copies, when there are any: the agent is the
/// one who can tell the user, and the update is theirs to make.
pub fn brief_notice(repo: &Path) -> Option<String> {
    let record = load(repo)?;
    // The record is written once per core run: a copy updated or added by
    // hand since then is no longer worth a line.
    let held: Vec<&HeldCopy> = record.held.iter().filter(|h| still_held(repo, h)).collect();
    if held.is_empty() {
        return None;
    }
    let list = held
        .iter()
        .map(|h| format!("{} ({})", h.path, h.from))
        .collect::<Vec<_>>()
        .join(", ");
    let now = record.upkeep_version.as_deref().unwrap_or("this version");
    Some(format!(
        "Engram's installed instructions here are older than v{now} and tracked by git, so they \
         were not updated automatically: {list}. Tell the user; `engram-alpha setup --refresh` \
         updates them (a change to commit)."
    ))
}

/// Is the held copy still as upkeep found it: still missing, or still
/// carrying the stamp of the version it was held at?
fn still_held(repo: &Path, h: &HeldCopy) -> bool {
    match std::fs::read_to_string(repo.join(&h.path)) {
        Err(_) => h.from == "not installed",
        Ok(text) if h.from.starts_with("pre-") => {
            !text.contains("engram-skill ") && !text.contains("<!-- engram:begin v")
        }
        Ok(text) => text.contains(&format!(" v{} ", h.from)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("engram-setuprec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_needs_an_engram_dir_and_round_trips() {
        let repo = tmp("roundtrip");
        let rec = SetupRecord {
            setup_version: Some("0.11.1".into()),
            agents: vec!["windsurf".into()],
            variant: Some("normal".into()),
            ..Default::default()
        };
        assert!(save(&repo, &rec).is_err(), "no .engram/ → not a project");
        std::fs::create_dir_all(repo.join(".engram")).unwrap();
        save(&repo, &rec).unwrap();
        assert_eq!(load(&repo), Some(rec));
        assert_eq!(brief_notice(&repo), None);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn held_copies_make_a_brief_notice() {
        let repo = tmp("notice");
        std::fs::create_dir_all(repo.join(".engram")).unwrap();
        let rec = SetupRecord {
            upkeep_version: Some("0.11.1".into()),
            held: vec![HeldCopy {
                path: ".claude/skills/engram/SKILL.md".into(),
                from: "0.10.2".into(),
            }],
            ..Default::default()
        };
        save(&repo, &rec).unwrap();
        let skill = repo.join(".claude/skills/engram/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(
            &skill,
            "---\n---\n<!-- engram-skill engram v0.10.2 relaxed sha256:x -->\n",
        )
        .unwrap();
        let line = brief_notice(&repo).unwrap();
        assert!(line.contains(".claude/skills/engram/SKILL.md (0.10.2)"));
        assert!(line.contains("setup --refresh"));
        // Updated by hand since the core's pass: no longer flagged.
        std::fs::write(
            &skill,
            "---\n---\n<!-- engram-skill engram v0.11.1 relaxed sha256:x -->\n",
        )
        .unwrap();
        assert_eq!(brief_notice(&repo), None);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn an_old_or_foreign_file_does_not_break_loading() {
        let repo = tmp("partial");
        std::fs::create_dir_all(repo.join(".engram")).unwrap();
        std::fs::write(path(&repo), r#"{"agents":["claude"],"future":1}"#).unwrap();
        assert_eq!(load(&repo).unwrap().agents, vec!["claude".to_string()]);
        std::fs::write(path(&repo), "not json").unwrap();
        assert_eq!(load(&repo), None);
        let _ = std::fs::remove_dir_all(&repo);
    }
}
