//! Project upkeep (0.11.1, issue #19): the core keeps every project's
//! installed skills and instruction blocks on its own binary's text, the
//! first time a bridge binds that project — not only the repo `serve` was
//! started in. An automatic pass rewrites only copies git doesn't track; a
//! tracked one is recorded as held in `.engram/setup.json`, which the brief
//! and `doctor` flag, and `setup --refresh` updates it on purpose. Hooks and
//! rules are never touched here: only `setup` rewires.

use std::path::Path;

use engram_core::setup_record::{self, HeldCopy, SetupRecord};

use crate::skillsync::{self, Found, Mode, State};

/// One upkeep pass over `repo`: refresh what git doesn't track, then write
/// the upkeep half of the setup record (building one from what is wired when
/// the repo predates the record). A folder without `.engram/` is not an
/// Engram project and is left alone.
pub fn run(repo: &Path) -> Vec<Found> {
    if !repo.join(".engram").is_dir() {
        return Vec::new();
    }
    let found = skillsync::refresh(repo, Mode::Untracked);
    let mut record = setup_record::load(repo).unwrap_or_else(|| inferred(repo));
    record_pass(repo, &mut record, &found);
    let _ = setup_record::save(repo, &record);
    found
}

/// What `setup` records after wiring `agents` (its refresh ran in
/// [`Mode::All`], so nothing is held any more).
pub fn record_setup(repo: &Path, agents: &[&str], variant: Option<&str>, found: &[Found]) {
    let mut record = setup_record::load(repo).unwrap_or_else(|| inferred(repo));
    record.setup_version = Some(env!("CARGO_PKG_VERSION").into());
    let mut all: Vec<String> = record.agents.clone();
    all.extend(agents.iter().map(|a| a.to_string()));
    all.sort();
    all.dedup();
    record.agents = all;
    if let Some(v) = variant {
        record.variant = Some(v.into());
    }
    record_pass(repo, &mut record, found);
    let _ = setup_record::save(repo, &record);
}

/// A record for a repo set up before 0.11.1: the variant its stamps name.
/// The setup version and the agents stay unknown — several assistants are
/// wired machine-wide, so what is wired says nothing about what this repo
/// was set up for; only `setup` records agents.
fn inferred(repo: &Path) -> SetupRecord {
    SetupRecord {
        variant: skillsync::installed_variant(repo),
        ..Default::default()
    }
}

fn record_pass(repo: &Path, record: &mut SetupRecord, found: &[Found]) {
    record.upkeep_version = Some(env!("CARGO_PKG_VERSION").into());
    record.upkeep_at = Some(engram_core::now());
    record.held = held(repo, found);
}

fn held(repo: &Path, found: &[Found]) -> Vec<HeldCopy> {
    found
        .iter()
        .filter_map(|f| match &f.state {
            State::Held { from } => Some(HeldCopy {
                path: f
                    .path
                    .strip_prefix(repo)
                    .unwrap_or(&f.path)
                    .display()
                    .to_string(),
                from: from.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// The oldest `setup` whose hooks, rules and MCP entries still match what
/// this binary's `setup` writes. **Bump it to the release version whenever
/// `setup` starts writing any of them differently** — every project set up
/// before it is then reported as due a rewire by `status`, `doctor` and
/// `update` (issue #19). Skills and instruction blocks don't count: upkeep
/// carries those on its own.
pub const WIRING_SINCE: &str = "0.11.1";

/// One project's setup as this binary judges it (issue #19): what `status`,
/// `status --setup`, `doctor` and `update` report.
#[derive(Debug, PartialEq)]
pub struct Inspection {
    /// Which version last ran `setup` here, if the record says.
    pub setup_version: Option<String>,
    pub agents: Vec<String>,
    pub variant: Option<String>,
    /// Hooks, rules and MCP entries: is a `setup` run due?
    pub wiring: Wiring,
    /// Installed copies upkeep will update on the next session (untracked).
    pub skills_due: Vec<String>,
    /// Outdated copies git tracks: only `setup --refresh` updates them.
    pub held: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub enum Wiring {
    /// Set up by a version whose wiring this binary still writes.
    Current,
    /// Set up by an older wiring: `setup` rewires it.
    Due { from: String },
    /// No record of which version set it up (before 0.11.1, or never).
    Unknown,
}

/// Judge `repo` against this binary. `None`: not an Engram project.
pub fn inspect(repo: &Path) -> Option<Inspection> {
    if !repo.join(".engram").is_dir() {
        return None;
    }
    let record = setup_record::load(repo).unwrap_or_default();
    let wiring = match record.setup_version.as_deref() {
        Some(v) if skillsync::older(v, WIRING_SINCE) => Wiring::Due { from: v.into() },
        Some(_) => Wiring::Current,
        None => Wiring::Unknown,
    };
    let rel = |f: &Found| {
        f.path
            .strip_prefix(repo)
            .unwrap_or(&f.path)
            .display()
            .to_string()
    };
    let found = skillsync::refresh(repo, Mode::Check);
    let pick = |held: bool| {
        found
            .iter()
            .filter(|f| match f.state {
                State::Held { .. } => held,
                State::Refreshed { .. } | State::Added { .. } => !held,
                _ => false,
            })
            .map(rel)
            .collect()
    };
    Some(Inspection {
        variant: record
            .variant
            .clone()
            .or_else(|| skillsync::installed_variant(repo)),
        setup_version: record.setup_version,
        agents: record.agents,
        wiring,
        skills_due: pick(false),
        held: pick(true),
    })
}

impl Inspection {
    /// Nothing for the user to do.
    pub fn is_current(&self) -> bool {
        self.wiring == Wiring::Current && self.skills_due.is_empty() && self.held.is_empty()
    }

    /// One line per project: what it was set up with, then what is due.
    pub fn describe(&self) -> String {
        let mut what = vec![match &self.setup_version {
            Some(v) => format!("set up by v{v}"),
            None => "setup not recorded".into(),
        }];
        if !self.agents.is_empty() {
            what.push(self.agents.join(", "));
        }
        if let Some(v) = &self.variant {
            what.push(v.clone());
        }
        let mut due = Vec::new();
        match &self.wiring {
            Wiring::Due { from } => due.push(format!(
                "hooks and rules are v{from}'s — `engram-alpha setup` there rewires them"
            )),
            Wiring::Unknown => {
                due.push("`engram-alpha setup` there once records it (and rewires)".into())
            }
            Wiring::Current => {}
        }
        if !self.skills_due.is_empty() {
            due.push(format!(
                "{} — the core updates on the next session",
                count(self.skills_due.len(), "skill copy", "skill copies")
            ));
        }
        if !self.held.is_empty() {
            due.push(format!(
                "{} tracked by git ({}) — `engram-alpha setup --refresh` there",
                count(self.held.len(), "outdated copy", "outdated copies"),
                self.held.join(", ")
            ));
        }
        if due.is_empty() {
            due.push("current".into());
        }
        format!("{} · {}", what.join(" · "), due.join("; "))
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "setup_version": self.setup_version,
            "agents": self.agents,
            "variant": self.variant,
            "wiring": match &self.wiring {
                Wiring::Current => "current".to_string(),
                Wiring::Due { .. } => "due".to_string(),
                Wiring::Unknown => "unknown".to_string(),
            },
            "skills_due": self.skills_due,
            "held": self.held,
            "current": self.is_current(),
        })
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Every registered project with its inspection — `status --setup` and the
/// report `update` asks the new binary for. Reads the registry file, so it
/// works without a core.
pub fn inspect_registered() -> Vec<(engram_core::registry::ProjectEntry, Option<Inspection>)> {
    engram_core::registry::load()
        .projects
        .into_iter()
        .map(|p| {
            let inspection = inspect(Path::new(&p.root));
            (p, inspection)
        })
        .collect()
}

/// After a swap, `update` lets the NEW binary judge the projects — it knows
/// the new skill texts and wiring, this one doesn't. A binary that can't
/// (older than 0.11.1, a downgrade) says nothing.
pub fn report_from(bin: &Path) -> Vec<String> {
    std::process::Command::new(bin)
        .args(["status", "--setup"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The core has no terminal: a pass's report goes to its log.
pub fn log(repo: &Path, found: &[Found]) {
    for line in skillsync::report(repo, found) {
        tracing::info!("upkeep {}: {line}", repo.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::claude_skill;
    use crate::skillsync::stamp_skill;
    use std::path::PathBuf;
    use std::process::Command;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("engram-upkeep-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".claude/skills/engram")).unwrap();
        std::fs::create_dir_all(d.join(".engram")).unwrap();
        d
    }

    fn old_skill(variant: &str) -> String {
        stamp_skill(claude_skill(variant), "engram", variant)
            .replace(&format!("v{} ", env!("CARGO_PKG_VERSION")), "v0.10.2 ")
    }

    fn git(repo: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn untracked_copies_update_and_the_record_is_inferred() {
        let repo = tmp("untracked");
        let skill = repo.join(".claude/skills/engram/SKILL.md");
        std::fs::write(&skill, old_skill("normal")).unwrap();
        let found = run(&repo);
        assert!(
            found
                .iter()
                .any(|f| f.path == skill && matches!(f.state, State::Refreshed { .. }))
        );
        assert_eq!(
            std::fs::read_to_string(&skill).unwrap(),
            stamp_skill(claude_skill("normal"), "engram", "normal")
        );
        let rec = setup_record::load(&repo).unwrap();
        assert_eq!(
            rec.setup_version, None,
            "never set up by a recording binary"
        );
        assert_eq!(rec.variant.as_deref(), Some("normal"));
        assert_eq!(
            rec.upkeep_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(rec.held.is_empty());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn tracked_copies_are_held_flagged_and_refreshed_only_on_purpose() {
        let repo = tmp("tracked");
        git(&repo, &["init", "-q"]);
        let skill = repo.join(".claude/skills/engram/SKILL.md");
        let old = old_skill("aggressive");
        std::fs::write(&skill, &old).unwrap();
        git(&repo, &["add", ".claude"]);
        git(&repo, &["commit", "-q", "-m", "skills"]);

        // The automatic pass leaves the tracked copy byte for byte, and
        // doesn't add the sleep skill beside it either.
        let found = run(&repo);
        assert_eq!(std::fs::read_to_string(&skill).unwrap(), old);
        assert!(!repo.join(".claude/skills/engram-sleep/SKILL.md").exists());
        assert!(
            found.iter().all(|f| matches!(f.state, State::Held { .. })),
            "{found:?}"
        );
        let rec = setup_record::load(&repo).unwrap();
        assert_eq!(
            rec.held,
            vec![
                HeldCopy {
                    path: ".claude/skills/engram/SKILL.md".into(),
                    from: "0.10.2".into()
                },
                HeldCopy {
                    path: ".claude/skills/engram-sleep/SKILL.md".into(),
                    from: "not installed".into()
                },
            ]
        );
        assert!(setup_record::brief_notice(&repo).is_some());
        // A dry run says the same.
        assert!(
            skillsync::refresh(&repo, Mode::Check)
                .iter()
                .all(|f| matches!(f.state, State::Held { .. }))
        );

        // The deliberate refresh writes them and clears the flag.
        let found = skillsync::refresh(&repo, Mode::All);
        record_setup(&repo, &[], None, &found);
        assert_eq!(
            std::fs::read_to_string(&skill).unwrap(),
            stamp_skill(claude_skill("aggressive"), "engram", "aggressive")
        );
        assert!(repo.join(".claude/skills/engram-sleep/SKILL.md").exists());
        assert!(setup_record::load(&repo).unwrap().held.is_empty());
        assert_eq!(setup_record::brief_notice(&repo), None);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn setup_records_its_version_and_merges_agents() {
        let repo = tmp("setup");
        record_setup(&repo, &["windsurf"], Some("relaxed"), &[]);
        record_setup(&repo, &["cursor", "windsurf"], Some("normal"), &[]);
        let rec = setup_record::load(&repo).unwrap();
        assert_eq!(
            rec.setup_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            rec.agents,
            vec!["cursor".to_string(), "windsurf".to_string()]
        );
        assert_eq!(rec.variant.as_deref(), Some("normal"));
        // Upkeep keeps what setup recorded.
        run(&repo);
        let after = setup_record::load(&repo).unwrap();
        assert_eq!(after.agents, rec.agents);
        assert_eq!(after.setup_version, rec.setup_version);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_folder_without_engram_dir_is_left_alone() {
        let repo = tmp("bare");
        std::fs::remove_dir_all(repo.join(".engram")).unwrap();
        let skill = repo.join(".claude/skills/engram/SKILL.md");
        std::fs::write(&skill, old_skill("relaxed")).unwrap();
        assert!(run(&repo).is_empty());
        assert_eq!(
            std::fs::read_to_string(&skill).unwrap(),
            old_skill("relaxed")
        );
        assert!(!repo.join(".engram").exists());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn inspect_reports_wiring_due_skills_and_held_copies() {
        let bare = tmp("inspect-bare");
        std::fs::remove_dir_all(bare.join(".engram")).unwrap();
        assert_eq!(inspect(&bare), None, "not an Engram project");

        // Never recorded, one outdated untracked skill.
        let repo = tmp("inspect");
        std::fs::write(
            repo.join(".claude/skills/engram/SKILL.md"),
            old_skill("normal"),
        )
        .unwrap();
        let i = inspect(&repo).unwrap();
        assert_eq!(i.wiring, Wiring::Unknown);
        assert_eq!(i.variant.as_deref(), Some("normal"), "read off the stamp");
        // From before 0.11.0: the sleep skill is due beside it.
        assert_eq!(
            i.skills_due,
            vec![
                ".claude/skills/engram/SKILL.md".to_string(),
                ".claude/skills/engram-sleep/SKILL.md".into()
            ]
        );
        assert!(i.held.is_empty());
        let line = i.describe();
        assert!(line.starts_with("setup not recorded · normal · "), "{line}");
        assert!(line.contains("2 skill copies — the core updates"), "{line}");
        assert!(!i.is_current());

        // Set up by a wiring older than this binary's.
        setup_record::save(
            &repo,
            &SetupRecord {
                setup_version: Some("0.10.2".into()),
                agents: vec!["claude".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let i = inspect(&repo).unwrap();
        assert_eq!(
            i.wiring,
            Wiring::Due {
                from: "0.10.2".into()
            }
        );
        assert!(
            i.describe().contains("hooks and rules are v0.10.2's"),
            "{}",
            i.describe()
        );
        assert_eq!(i.to_json()["wiring"], "due");

        // Tracked by git → held, not due.
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", ".claude"]);
        git(&repo, &["commit", "-q", "-m", "skills"]);
        let i = inspect(&repo).unwrap();
        assert!(i.skills_due.is_empty(), "{i:?}");
        assert!(
            i.held
                .contains(&".claude/skills/engram/SKILL.md".to_string()),
            "{i:?}"
        );
        assert!(i.describe().contains("setup --refresh"));

        // A deliberate setup by this binary: nothing due.
        let found = skillsync::refresh(&repo, Mode::All);
        record_setup(&repo, &["claude"], Some("normal"), &found);
        let i = inspect(&repo).unwrap();
        assert!(i.is_current(), "{i:?}");
        assert_eq!(
            i.describe(),
            format!(
                "set up by v{} · claude · normal · current",
                env!("CARGO_PKG_VERSION")
            )
        );
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&bare);
    }

    #[test]
    fn the_wiring_line_is_never_ahead_of_the_binary() {
        assert!(!skillsync::older(env!("CARGO_PKG_VERSION"), WIRING_SINCE));
    }

    #[cfg(unix)]
    #[test]
    fn update_relays_the_new_binarys_report_and_skips_one_that_cannot() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("engram-upkeep-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, body: &str| {
            let bin = dir.join(name);
            std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            bin
        };
        let new = write(
            "new",
            r#"[ "$1 $2" = "status --setup" ] && echo "project setup: line one" && echo "  two""#,
        );
        let old = write(
            "old",
            "echo \"error: unexpected argument '--setup'\" >&2; exit 2",
        );
        // A just-written script can be briefly busy on Linux (see update.rs).
        let mut lines = Vec::new();
        for _ in 0..40 {
            lines = report_from(&new);
            if !lines.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(
            lines,
            vec!["project setup: line one".to_string(), "  two".into()]
        );
        assert!(report_from(&old).is_empty());
        assert!(report_from(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
