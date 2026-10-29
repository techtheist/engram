//! "Is this path already git-ignored?" — the one question `serve`, `doctor`
//! and `setup` ask before they touch a repo's `.gitignore` (issue #12: an
//! anchored `/.engram/` used to read as missing, and `serve` appended a
//! duplicate `.engram/` to a tracked file on every start).
//!
//! Git answers first, so every rule it applies counts — anchoring, globs,
//! nested `.gitignore` files, `.git/info/exclude`, the global excludes file.
//! Outside a repo, or without git on PATH, a line matcher reads the repo's
//! own `.gitignore` and understands the common shapes of one entry.

use std::path::{Component, Path};
use std::process::{Command, Stdio};

/// The `.gitignore` entry for a graph store at `db`, as seen from `cwd`: its
/// top folder (`.engram/` for `.engram/graph.tepin`). None when there's no
/// folder of the repo's own to ignore — a store outside `cwd`, or one sitting
/// directly in it. An absolute `--db` inside `cwd` (scripts/deploy-pane.sh)
/// counts; its first component used to be the root, which wrote `//`.
pub fn db_dir_entry(db: &Path, cwd: &Path) -> Option<String> {
    let rel = if db.is_absolute() {
        db.strip_prefix(cwd).ok()?
    } else {
        db
    };
    let mut parts = rel.components().filter(|c| !matches!(c, Component::CurDir));
    let top = match parts.next()? {
        Component::Normal(top) => top,
        _ => return None,
    };
    parts.next()?;
    Some(format!("{}/", top.to_string_lossy()))
}

/// True when `rel` (relative to `repo`, a trailing `/` for a directory) is
/// already ignored, so nothing needs adding.
pub fn is_ignored(repo: &Path, rel: &str) -> bool {
    match git_check_ignore(repo, rel) {
        Some(ignored) => ignored,
        None => {
            let text = std::fs::read_to_string(repo.join(".gitignore")).unwrap_or_default();
            listed(&text, rel)
        }
    }
}

/// `git check-ignore`: exit 0 = ignored, 1 = not ignored, anything else
/// (128: not a repo) or a missing git = no answer. `--no-index` judges the
/// patterns even if the path happens to be tracked.
fn git_check_ignore(repo: &Path, rel: &str) -> Option<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["check-ignore", "-q", "--no-index", "--", rel])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// The fallback: does one `.gitignore` text ignore `rel` at the repo root?
/// Accepts `x`, `x/`, `/x`, `/x/`, `**/x`, `x/*` and `x/**` for `rel = "x/"`;
/// the last matching line wins, so a later `!x` re-includes it.
pub(crate) fn listed(gitignore: &str, rel: &str) -> bool {
    let want = rel.trim_matches('/');
    let mut ignored = false;
    for line in gitignore.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, pattern) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        if normalize(pattern) == want {
            ignored = !negated;
        }
    }
    ignored
}

/// One entry's root-level name: drop the anchoring `/` or `**/` prefix and
/// the directory suffixes (`/`, `/*`, `/**`).
fn normalize(pattern: &str) -> &str {
    let p = pattern
        .strip_prefix("**/")
        .or_else(|| pattern.strip_prefix('/'))
        .unwrap_or(pattern);
    let p = p
        .strip_suffix("/**")
        .or_else(|| p.strip_suffix("/*"))
        .unwrap_or(p);
    p.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::{db_dir_entry, is_ignored, listed};
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("engram-gitignore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn db_dir_entry_names_the_top_folder_inside_cwd_only() {
        let cwd = Path::new("/work/repo");
        for (db, want) in [
            (".engram/graph.tepin", Some(".engram/")),
            ("./.engram/graph.db", Some(".engram/")),
            ("/work/repo/.engram/graph.db", Some(".engram/")),
            ("/work/repo/data/mem/graph.tepin", Some("data/")),
            ("/elsewhere/.engram/graph.tepin", None),
            ("/work/repo/graph.tepin", None),
            ("graph.tepin", None),
            ("../other/.engram/graph.tepin", None),
        ] {
            assert_eq!(db_dir_entry(Path::new(db), cwd).as_deref(), want, "{db}");
        }
    }

    #[test]
    fn listed_accepts_every_root_shape_of_the_entry() {
        for line in [
            ".engram/",
            ".engram",
            "/.engram/",
            "/.engram",
            "**/.engram",
            "**/.engram/",
            ".engram/*",
            ".engram/**",
            "  /.engram/  ",
            "/.engram/\r",
        ] {
            assert!(
                listed(&format!("target/\n{line}\n"), ".engram/"),
                "{line:?}"
            );
        }
    }

    #[test]
    fn listed_refuses_other_names_comments_and_negations() {
        for text in [
            "",
            "# .engram/\n",
            ".engram-old/\n",
            "foo/.engram/\n",
            ".engram/\n!.engram/\n",
            "!/.engram\n",
        ] {
            assert!(!listed(text, ".engram/"), "{text:?}");
        }
        // The last matching line wins: re-ignored after a negation.
        assert!(listed(".engram/\n!.engram/\n/.engram/\n", ".engram/"));
    }

    #[test]
    fn listed_matches_file_entries_too() {
        assert!(listed("/.cursor/mcp.json\n", ".cursor/mcp.json"));
        assert!(!listed(".cursor/\n", ".cursor/mcp.json"));
    }

    /// Outside a git repo the fallback reads the repo's own .gitignore.
    #[test]
    fn is_ignored_reads_the_file_outside_a_repo() {
        let dir = scratch("file");
        assert!(!is_ignored(&dir, ".engram/"));
        std::fs::write(dir.join(".gitignore"), "/.engram/\n").unwrap();
        assert!(is_ignored(&dir, ".engram/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Inside a repo git decides — anchored entries, nested rules and the
    /// info/exclude file all count (skipped when git isn't installed).
    #[test]
    fn is_ignored_asks_git_inside_a_repo() {
        let dir = scratch("git");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !git(&["init", "-q"]) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        // A machine whose global excludes already cover .engram/ has nothing
        // left to show here.
        if is_ignored(&dir, ".engram/") {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        std::fs::create_dir_all(dir.join(".git/info")).unwrap();
        std::fs::write(dir.join(".git/info/exclude"), "/.engram/\n").unwrap();
        assert!(is_ignored(&dir, ".engram/"));
        // Still nothing in .gitignore — the fallback alone would say no.
        assert!(!dir.join(".gitignore").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
