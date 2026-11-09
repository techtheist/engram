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

/// What `update` says about the registered projects once the new binary is
/// in place (issue #19): their skills follow on the next session, tracked
/// copies and hooks/rules need a deliberate step.
pub fn after_update(tag: &str) -> Vec<String> {
    let new = tag.trim_start_matches('v');
    let mut held = Vec::new();
    let mut rewire = Vec::new();
    for p in engram_core::registry::load().projects {
        let repo = Path::new(&p.root);
        let Some(rec) = setup_record::load(repo) else {
            continue;
        };
        if !rec.held.is_empty() {
            held.push(p.name.clone());
        }
        if rec.setup_version.as_deref() != Some(new) {
            rewire.push(p.name);
        }
    }
    let mut out = Vec::new();
    if !rewire.is_empty() {
        out.push(format!(
            "installed skills follow on each project's next session; hooks and rules stay as \
             written until `engram-alpha setup` runs there: {}",
            rewire.join(", ")
        ));
    }
    if !held.is_empty() {
        out.push(format!(
            "git tracks some installed instructions in {} — `engram-alpha setup --refresh` \
             there updates them",
            held.join(", ")
        ));
    }
    out
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
}
