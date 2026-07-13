//! Digestion tier 1 (PLAN §7B): the offline code scan. Walks the working
//! tree gitignore-aware and collects `FIXME` / `TODO` markers as *candidate*
//! nodes — a FIXME nominates a Problem, a TODO nominates an Intent. Pure
//! nomination: nothing here writes to the graph; the digest skill judges the
//! candidates and authors real nodes through the normal write path (with all
//! its dedup/warning/suspect checks).
//!
//! Robustness is the contract: a malformed, huge, binary, oddly-encoded or
//! unreadable file is skipped and counted, never an error — a file-walk flaw
//! must not take down the daemon.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::types::NodeType;

/// Files larger than this are skipped (generated/vendored blobs, not
/// hand-written code carrying a marker worth a memory node).
const MAX_FILE_BYTES: u64 = 1_000_000;
/// Global candidate cap — reported via `truncated`, never silent.
const MAX_CANDIDATES: usize = 500;
/// Marker text is clipped to this many characters (minified lines).
const MAX_TEXT_CHARS: usize = 200;

/// `TODO` / `FIXME` as standalone uppercase words, optionally with an
/// `(author)` tag, followed by the marker text. Uppercase-only on purpose:
/// lowercase "todo" in prose or identifiers is not a work marker.
static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(TODO|FIXME)\b(?:\([^)\n]*\))?[:\s]*(.*)").expect("static marker pattern")
});

#[derive(Debug, Clone, Serialize)]
pub struct DigestCandidate {
    /// The literal marker found: "TODO" | "FIXME".
    pub marker: String,
    /// The node type the marker nominates: TODO → Intent, FIXME → Problem.
    pub suggested_type: NodeType,
    /// The marker's own text (secret-scrubbed, comment closers stripped).
    /// May be empty — a bare `FIXME` line still marks a spot worth reading.
    pub text: String,
    /// Repo-relative path, `/`-separated — ready to use as a code_ref.
    pub file: String,
    /// 1-based line number (context for the reader, NOT for code_refs).
    pub line: usize,
}

/// What the scan covered, so the skill can report honestly (no silent caps).
#[derive(Debug, Clone, Serialize)]
pub struct DigestScan {
    pub candidates: Vec<DigestCandidate>,
    pub files_scanned: usize,
    /// Files the walk surfaced but could not or would not read (binary,
    /// oversized, unreadable) — skipped, never fatal.
    pub files_skipped: usize,
    /// True when the candidate cap cut the scan short — run again after
    /// digesting, or digest directory by directory.
    pub truncated: bool,
}

/// Walk `root` and collect marker candidates. Infallible by design: walk
/// errors (permission, loop, dangling link) skip the entry and continue.
pub fn scan(root: &Path) -> DigestScan {
    let mut out = DigestScan {
        candidates: Vec::new(),
        files_scanned: 0,
        files_skipped: 0,
        truncated: false,
    };
    let walk = ignore::WalkBuilder::new(root)
        // Respect .gitignore even when the target isn't a git checkout —
        // digesting an unpacked tarball should still skip node_modules.
        .require_git(false)
        .filter_entry(|entry| {
            // Log/build droppings are trash for digestion even when a repo
            // forgot to gitignore them (PLAN §7B).
            !(entry.file_type().is_some_and(|t| t.is_dir())
                && is_trash_dir(&entry.file_name().to_string_lossy()))
        })
        .build();
    for entry in walk {
        let Ok(entry) = entry else {
            out.files_skipped += 1;
            continue;
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        match scan_file(entry.path(), root, &mut out.candidates) {
            Ok(()) => out.files_scanned += 1,
            Err(()) => out.files_skipped += 1,
        }
        if out.candidates.len() >= MAX_CANDIDATES {
            out.truncated = true;
            out.candidates.truncate(MAX_CANDIDATES);
            break;
        }
    }
    out
}

fn is_trash_dir(name: &str) -> bool {
    matches!(name, "log" | "logs" | "tmp" | "temp")
}

/// Err(()) means "count as skipped" — any read problem, binary content, or
/// oversize. Never panics, never propagates io::Error.
fn scan_file(path: &Path, root: &Path, candidates: &mut Vec<DigestCandidate>) -> Result<(), ()> {
    let meta = std::fs::metadata(path).map_err(|_| ())?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(());
    }
    let bytes = std::fs::read(path).map_err(|_| ())?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(()); // binary
    }
    let text = String::from_utf8_lossy(&bytes);
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    for (idx, line) in text.lines().enumerate() {
        let Some(cap) = MARKER.captures(line) else {
            continue;
        };
        let marker = cap[1].to_string();
        candidates.push(DigestCandidate {
            suggested_type: if marker == "FIXME" {
                NodeType::Problem
            } else {
                NodeType::Intent
            },
            marker,
            text: clean_text(&cap[2]),
            file: rel.clone(),
            line: idx + 1,
        });
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
    }
    Ok(())
}

/// Strip trailing comment closers, clip, and scrub secrets — the redaction
/// pass runs on every ingested item (PLAN §7B guardrails).
fn clean_text(raw: &str) -> String {
    let mut t = raw.trim();
    for closer in ["*/", "-->", "--}}", "#}"] {
        t = t.strip_suffix(closer).unwrap_or(t).trim_end();
    }
    let clipped: String = t.chars().take(MAX_TEXT_CHARS).collect();
    crate::redact::scrub(&clipped)
}
