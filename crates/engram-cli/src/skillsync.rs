//! Installed skills and instruction blocks follow the binary (0.9.12).
//!
//! Everything `setup` writes for an assistant — the engram and digest skills,
//! the section between the `<!-- engram:begin … -->` markers in AGENTS.md /
//! GEMINI.md — carries a stamp: the version that wrote it, its variant, and
//! the SHA-256 of the text as installed. On update (`serve`, `setup`), a copy
//! whose text still matches its own fingerprint is rewritten with this
//! binary's text in the same variant; a copy that no longer matches was
//! edited by someone, and is left exactly as it is. Copies from before the
//! stamp are recognised only by matching, byte for byte, a text engram
//! shipped ([`crate::shipped_skills::LEGACY`]) — anything else is not ours to
//! touch. Symlinked skills (a repo sourcing its skill deliberately) are never
//! followed.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::setup::{CLAUDE_DIGEST, CLAUDE_SLEEP, agent_block, claude_skill, is_symlink};
use crate::shipped_skills::LEGACY;

/// Skill directories setup installs into, per harness.
/// The release that first shipped the sleep skill.
const SLEEP_SINCE: &str = "0.11.0";

const SKILL_DIRS: [&str; 5] = [
    ".claude/skills",
    ".cursor/skills",
    ".devin/skills",
    ".windsurf/skills",
    ".codex/skills",
];
/// Files that carry the marker-delimited instructions block.
const BLOCK_FILES: [&str; 2] = ["AGENTS.md", "GEMINI.md"];

pub const MARK_BEGIN_PREFIX: &str = "<!-- engram:begin";
pub const MARK_END: &str = "<!-- engram:end -->";
const STAMP_PREFIX: &str = "<!-- engram-skill ";

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn older(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parse(a) < parse(b)
}

/// The skill text for an installed kind (`engram` / `digest` / `sleep`).
fn shipped(kind: &str, variant: &str) -> &'static str {
    match kind {
        "digest" => CLAUDE_DIGEST,
        "sleep" => CLAUDE_SLEEP,
        _ => claude_skill(variant),
    }
}

/// A skill as written to disk: the stamp line goes right after the YAML
/// frontmatter, so the file still opens with `---` for every harness.
pub fn stamp_skill(text: &str, kind: &str, variant: &str) -> String {
    let line = format!(
        "{STAMP_PREFIX}{kind} v{} {variant} sha256:{} — installed by engram-alpha; updated with it unless you edit this file -->\n",
        version(),
        sha(text)
    );
    match frontmatter_end(text) {
        Some(at) => format!("{}{line}{}", &text[..at], &text[at..]),
        None => format!("{line}{text}"),
    }
}

fn frontmatter_end(text: &str) -> Option<usize> {
    let rest = text.strip_prefix("---\n")?;
    let close = rest.find("\n---\n")?;
    Some(4 + close + 5)
}

struct Stamp {
    kind: String,
    version: String,
    variant: String,
    hash: String,
}

/// Split a stamped file into its stamp and the text as installed.
fn read_stamp(file: &str) -> Option<(Stamp, String)> {
    let start = file.find(STAMP_PREFIX)?;
    let end = start + file[start..].find('\n')? + 1;
    let line = &file[start..end];
    let mut words = line[STAMP_PREFIX.len()..].split_whitespace();
    let kind = words.next()?.to_string();
    let version = words.next()?.trim_start_matches('v').to_string();
    let variant = words.next()?.to_string();
    let hash = words.next()?.strip_prefix("sha256:")?.to_string();
    let text = format!("{}{}", &file[..start], &file[end..]);
    Some((
        Stamp {
            kind,
            version,
            variant,
            hash,
        },
        text,
    ))
}

/// The variant a pre-stamp engram skill names in its title line.
fn titled_variant(text: &str) -> Option<&'static str> {
    let title = text
        .lines()
        .find(|l| l.starts_with("# Engram — project memory ("))?;
    ["Aggressive", "Normal", "Relaxed"]
        .into_iter()
        .find(|v| title.contains(&format!("({v})")))
        .map(|v| match v {
            "Aggressive" => "aggressive",
            "Normal" => "normal",
            _ => "relaxed",
        })
}

fn legacy_variant(kind: &str, text: &str) -> Option<&'static str> {
    let h = sha(text);
    let hit = LEGACY.iter().find(|(k, _, s)| *k == kind && *s == h)?;
    Some(match kind {
        "engram" => titled_variant(text).unwrap_or(hit.1),
        _ => hit.1,
    })
}

/// What a refresh pass found for one installed copy.
#[derive(Debug, PartialEq)]
pub enum State {
    /// Already this binary's text.
    Current,
    /// Ours and untouched — rewritten (or due, on a dry run).
    Refreshed { from: String, variant: String },
    /// Ours, but someone edited it: left as it is.
    Edited,
    /// A skill this binary ships that the repo predates (the sleep skill,
    /// 0.11.0) — written beside the engram skill the same refresh updates.
    Added { variant: String },
    /// Ours, untouched and outdated (or missing), but git tracks it: an
    /// automatic pass leaves it alone, since rewriting it would dirty the
    /// working tree (0.11.1). `setup --refresh` updates it on purpose.
    Held { from: String },
}

/// How far a refresh pass may write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// Write nothing; report what a pass would do (doctor).
    Check,
    /// Rewrite only copies git doesn't track — the core's upkeep and `serve`.
    Untracked,
    /// Rewrite every untouched copy — `setup`, a deliberate act.
    All,
}

#[derive(Debug)]
pub struct Found {
    pub path: PathBuf,
    pub state: State,
}

/// Decide one skill file's fate; `Some(new text)` when it should be rewritten.
fn plan_skill(file: &str, kind: &str) -> Option<(State, Option<String>)> {
    if let Some((stamp, text)) = read_stamp(file) {
        if stamp.kind != kind {
            return None;
        }
        if sha(&text) != stamp.hash {
            return Some((State::Edited, None));
        }
        if !older(&stamp.version, version()) {
            return Some((State::Current, None));
        }
        let next = stamp_skill(shipped(kind, &stamp.variant), kind, &stamp.variant);
        return Some((
            State::Refreshed {
                from: stamp.version,
                variant: stamp.variant,
            },
            Some(next),
        ));
    }
    match legacy_variant(kind, file) {
        Some(variant) => Some((
            State::Refreshed {
                from: "pre-0.9.12".into(),
                variant: variant.into(),
            },
            Some(stamp_skill(shipped(kind, variant), kind, variant)),
        )),
        // Unstamped and not a text we shipped: if it looks like ours, it was
        // edited; otherwise it is someone else's skill under our name.
        None if file.contains("# Engram — project memory (") && kind == "engram" => {
            Some((State::Edited, None))
        }
        None => None,
    }
}

/// The instructions block as written between the markers.
pub fn stamped_block(variant: &str) -> String {
    let body = agent_block(variant);
    format!(
        "{MARK_BEGIN_PREFIX} v{} {variant} sha256:{} -->\n{body}{MARK_END}\n",
        version(),
        sha(body)
    )
}

/// Locate the marker block: (start of begin line, end of end marker line,
/// begin line, body between).
fn find_block(file: &str) -> Option<(usize, usize, &str, &str)> {
    let start = file.find(MARK_BEGIN_PREFIX)?;
    let begin_end = start + file[start..].find('\n')? + 1;
    let end_at = begin_end + file[begin_end..].find(MARK_END)?;
    let mut after = end_at + MARK_END.len();
    if file[after..].starts_with('\n') {
        after += 1;
    }
    Some((
        start,
        after,
        &file[start..begin_end],
        &file[begin_end..end_at],
    ))
}

/// Replace (or plan to replace) one file's block; the user's text around it
/// is never touched.
fn plan_block(file: &str) -> Option<(State, Option<String>)> {
    let (start, after, begin, body) = find_block(file)?;
    let words: Vec<&str> = begin
        .trim_end()
        .trim_end_matches("-->")
        .split_whitespace()
        .collect();
    // `<!-- engram:begin v0.9.12 aggressive sha256:… -->` (stamped) or the
    // plain pre-0.9.12 `<!-- engram:begin -->`.
    let (from, variant) = match words.as_slice() {
        [_, _, ver, variant, hash] => {
            if hash.strip_prefix("sha256:") != Some(sha(body).as_str()) {
                return Some((State::Edited, None));
            }
            let ver = ver.trim_start_matches('v');
            if !older(ver, version()) {
                return Some((State::Current, None));
            }
            (ver.to_string(), variant.to_string())
        }
        _ => match legacy_variant("block", body) {
            Some(v) => ("pre-0.9.12".to_string(), v.to_string()),
            None => return Some((State::Edited, None)),
        },
    };
    let next = format!(
        "{}{}{}",
        &file[..start],
        stamped_block(&variant),
        &file[after..]
    );
    Some((State::Refreshed { from, variant }, Some(next)))
}

/// Swap `file`'s marker block for `block`: `Ok(None)` when there is no
/// block, `Err(())` when the existing one was edited (keep it).
pub fn replace_block(file: &str, block: &str) -> Result<Option<String>, ()> {
    let Some((start, after, _, _)) = find_block(file) else {
        return Ok(None);
    };
    if matches!(plan_block(file), Some((State::Edited, _))) {
        return Err(());
    }
    Ok(Some(format!("{}{block}{}", &file[..start], &file[after..])))
}

/// Is the skill at `path` ours but edited by hand? (Absent, untouched or
/// foreign files are not.)
pub fn is_edited(path: &Path, kind: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|f| plan_skill(&f, kind))
        .is_some_and(|(state, _)| state == State::Edited)
}

/// Walk every installed copy in `repo` and rewrite the untouched outdated
/// ones as far as `mode` allows. Only copies that are ours appear in the
/// result.
pub fn refresh(repo: &Path, mode: Mode) -> Vec<Found> {
    let mut found = Vec::new();
    // `probe` is the file whose git status decides: the copy itself, or for
    // a skill being added, the engram skill it is added beside.
    let mut visit = |path: PathBuf, probe: &Path, plan: Option<(State, Option<String>)>| {
        let Some((state, next)) = plan else { return };
        if next.is_some() && mode != Mode::All && git_tracked(repo, probe) {
            let from = match state {
                State::Refreshed { from, .. } => from,
                _ => "not installed".into(),
            };
            found.push(Found {
                path,
                state: State::Held { from },
            });
            return;
        }
        // A write that fails (read-only checkout) reports nothing.
        if mode != Mode::Check
            && let Some(next) = next
            && (path
                .parent()
                .is_some_and(|d| std::fs::create_dir_all(d).is_err())
                || std::fs::write(&path, next).is_err())
        {
            return;
        }
        found.push(Found { path, state });
    };
    for dir in SKILL_DIRS {
        // Whether this pass lifts the engram skill from before 0.11.0: the
        // one moment a missing sleep skill is added (later, a deleted one
        // stays deleted).
        let mut upgrading = false;
        let engram_skill = repo.join(dir).join("engram/SKILL.md");
        for (name, kind) in [
            ("engram", "engram"),
            ("engram-digest", "digest"),
            ("engram-sleep", "sleep"),
        ] {
            let skill_dir = repo.join(dir).join(name);
            let path = skill_dir.join("SKILL.md");
            if is_symlink(&skill_dir) || is_symlink(&path) {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(file) => {
                    let plan = plan_skill(&file, kind);
                    if let Some((State::Refreshed { from, .. }, _)) = &plan
                        && kind == "engram"
                    {
                        upgrading = older(from, SLEEP_SINCE);
                    }
                    visit(path.clone(), &path, plan);
                }
                Err(_) if kind == "sleep" && upgrading => visit(
                    path,
                    &engram_skill,
                    Some((
                        State::Added {
                            variant: "sleep".into(),
                        },
                        Some(stamp_skill(CLAUDE_SLEEP, "sleep", "sleep")),
                    )),
                ),
                Err(_) => {}
            }
        }
    }
    for name in BLOCK_FILES {
        let path = repo.join(name);
        if is_symlink(&path) {
            continue;
        }
        if let Ok(file) = std::fs::read_to_string(&path) {
            visit(path.clone(), &path, plan_block(&file));
        }
    }
    found
}

/// Does git track `path` in `repo`? No git, or not a repository: no.
pub fn git_tracked(repo: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(repo).unwrap_or(path);
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(rel)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The variant this repo's instructions were installed in, read off the
/// first stamped engram skill or instructions block.
pub fn installed_variant(repo: &Path) -> Option<String> {
    for dir in SKILL_DIRS {
        if let Ok(file) = std::fs::read_to_string(repo.join(dir).join("engram/SKILL.md"))
            && let Some((stamp, _)) = read_stamp(&file)
            && stamp.kind == "engram"
        {
            return Some(stamp.variant);
        }
    }
    BLOCK_FILES.iter().find_map(|name| {
        let file = std::fs::read_to_string(repo.join(name)).ok()?;
        let (_, _, begin, _) = find_block(&file)?;
        match begin.split_whitespace().collect::<Vec<_>>().as_slice() {
            [_, _, _, variant, _, ..] => Some(variant.to_string()),
            _ => None,
        }
    })
}

/// One line per copy a refresh changed or kept because it was edited.
pub fn report(repo: &Path, found: &[Found]) -> Vec<String> {
    found
        .iter()
        .filter_map(|f| {
            let rel = f.path.strip_prefix(repo).unwrap_or(&f.path).display();
            match &f.state {
                State::Current => None,
                State::Refreshed { from, variant } => Some(format!(
                    "updated {rel} ({variant}, {from} → {})",
                    version()
                )),
                State::Edited => Some(format!(
                    "kept {rel} — it was edited, so updates skip it (delete it and run `engram-alpha setup` for the current text)"
                )),
                State::Added { variant } => Some(format!(
                    "added {rel} (the {variant} skill, new in v{})",
                    version()
                )),
                State::Held { from } => Some(format!(
                    "kept {rel} ({from}) — git tracks it, so only `engram-alpha setup --refresh` updates it"
                )),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("engram-skillsync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".claude/skills/engram")).unwrap();
        d
    }

    #[test]
    fn a_stamp_round_trips_and_keeps_the_frontmatter_first() {
        let s = stamp_skill(claude_skill("normal"), "engram", "normal");
        assert!(s.starts_with("---\nname: engram\n"));
        let (stamp, text) = read_stamp(&s).unwrap();
        assert_eq!(text, claude_skill("normal"));
        assert_eq!(stamp.variant, "normal");
        assert_eq!(stamp.hash, sha(claude_skill("normal")));
        assert_eq!(plan_skill(&s, "engram").unwrap().0, State::Current);
    }

    #[test]
    fn untouched_copies_update_and_edited_ones_are_kept() {
        let repo = tmp("skills");
        let path = repo.join(".claude/skills/engram/SKILL.md");
        // An older stamped copy, untouched → updated, variant kept.
        let old = stamp_skill(claude_skill("aggressive"), "engram", "aggressive")
            .replace(&format!("v{} ", version()), "v0.9.11 ");
        std::fs::write(&path, &old).unwrap();
        let found = refresh(&repo, Mode::All);
        assert!(
            matches!(&found[0].state, State::Refreshed { variant, .. } if variant == "aggressive")
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            stamp_skill(claude_skill("aggressive"), "engram", "aggressive")
        );
        // The same copy edited by hand → left byte for byte.
        let edited = old.replace("Engram is a local", "Engram (ours) is a local");
        std::fs::write(&path, &edited).unwrap();
        let found = refresh(&repo, Mode::All);
        assert_eq!(found[0].state, State::Edited);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn upgrading_from_before_sleep_adds_the_sleep_skill_once() {
        let repo = tmp("sleep");
        let engram = repo.join(".claude/skills/engram/SKILL.md");
        let sleep = repo.join(".claude/skills/engram-sleep/SKILL.md");
        let old = stamp_skill(claude_skill("relaxed"), "engram", "relaxed")
            .replace(&format!("v{} ", version()), "v0.10.4 ");
        std::fs::write(&engram, &old).unwrap();

        // A dry run (doctor) reports it and writes nothing.
        let dry = refresh(&repo, Mode::Check);
        assert!(dry.iter().any(|f| f.path == sleep
            && matches!(&f.state, State::Added { variant } if variant == "sleep")));
        assert!(!sleep.exists());

        let found = refresh(&repo, Mode::All);
        assert!(found.iter().any(|f| f.path == sleep));
        assert_eq!(
            std::fs::read_to_string(&sleep).unwrap(),
            stamp_skill(CLAUDE_SLEEP, "sleep", "sleep")
        );
        let lines = report(&repo, &found);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("added .claude/skills/engram-sleep")),
            "{lines:?}"
        );

        // Deleted after the upgrade: it stays deleted.
        std::fs::remove_dir_all(sleep.parent().unwrap()).unwrap();
        assert!(refresh(&repo, Mode::All).iter().all(|f| f.path != sleep));
        assert!(!sleep.exists());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn pre_stamp_copies_update_only_when_they_match_a_shipped_text() {
        let repo = tmp("legacy");
        let path = repo.join(".claude/skills/engram/SKILL.md");
        // Every legacy fingerprint is a real shipped text: rebuild one from
        // the table's own kind and check a byte-identical copy is recognised.
        let (_, variant, h) = LEGACY.iter().find(|(k, _, _)| *k == "engram").unwrap();
        assert_eq!(h.len(), 64, "{variant}");
        // An unstamped engram skill that is NOT a shipped text was edited.
        std::fs::write(
            &path,
            "---\nname: engram\n---\n\n# Engram — project memory (Relaxed)\nmine\n",
        )
        .unwrap();
        let found = refresh(&repo, Mode::All);
        assert_eq!(found[0].state, State::Edited);
        assert!(std::fs::read_to_string(&path).unwrap().contains("mine"));
        // A foreign skill under our name is not ours at all.
        std::fs::write(&path, "---\nname: engram\n---\nsomeone else's\n").unwrap();
        assert!(refresh(&repo, Mode::All).is_empty());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn blocks_update_inside_the_markers_and_never_touch_the_rest() {
        let repo = tmp("block");
        let path = repo.join("AGENTS.md");
        let old = stamped_block("normal").replace(&format!("v{} ", version()), "v0.9.11 ");
        std::fs::write(&path, format!("# Mine\n\nmy rules\n\n{old}\nafter\n")).unwrap();
        let found = refresh(&repo, Mode::All);
        assert!(matches!(&found[0].state, State::Refreshed { variant, .. } if variant == "normal"));
        let now = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            now,
            format!("# Mine\n\nmy rules\n\n{}\nafter\n", stamped_block("normal"))
        );
        // Edited inside the markers → kept.
        let edited = now.replace("Not code structure", "Not code structure (really)");
        std::fs::write(&path, &edited).unwrap();
        assert_eq!(refresh(&repo, Mode::All)[0].state, State::Edited);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn real_0_9_11_copies_are_recognised_and_updated() {
        let repo = tmp("real");
        let skill = repo.join(".claude/skills/engram/SKILL.md");
        std::fs::write(&skill, include_str!("testdata/skill-relaxed-0.9.11.md")).unwrap();
        let agents = repo.join("AGENTS.md");
        let block = include_str!("testdata/block-aggressive-0.9.11.md");
        std::fs::write(
            &agents,
            format!("my own rules\n\n<!-- engram:begin -->\n{block}<!-- engram:end -->\n"),
        )
        .unwrap();
        let found = refresh(&repo, Mode::All);
        // The skill, the block, and the sleep skill a pre-0.11.0 repo lacks.
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(
            found
                .iter()
                .any(|f| matches!(&f.state, State::Added { variant } if variant == "sleep"))
        );
        assert_eq!(
            std::fs::read_to_string(&skill).unwrap(),
            stamp_skill(claude_skill("relaxed"), "engram", "relaxed")
        );
        assert_eq!(
            std::fs::read_to_string(&agents).unwrap(),
            format!("my own rules\n\n{}", stamped_block("aggressive"))
        );
        // A second pass finds all three current.
        assert!(
            refresh(&repo, Mode::All)
                .iter()
                .all(|f| f.state == State::Current)
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn version_order_is_numeric() {
        assert!(older("0.9.9", "0.9.12"));
        assert!(!older("0.9.12", "0.9.12"));
        assert!(!older("0.10.0", "0.9.12"));
    }
}
