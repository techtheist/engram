//! Claude Code adapter: `~/.claude/projects/<slug>/<session>.jsonl`, typed
//! JSONL records. The richest of the first wave and the one we dogfood on.
//!
//! Kept at parse level (decision 00bgfte7usll): `user`/`assistant` records
//! only, and of those only prose — string content, or `text` blocks. Dropped:
//! every other record type (`system`, `attachment`, `mode`, `ai-title`,
//! `file-history-*`, …), sidechain (subagent) traffic, synthetic `isMeta`
//! records, tool-result echoes (`toolUseResult` — they say `role: user` but
//! are machine traffic), and `tool_use`/`tool_result`/`thinking` blocks.
//! Routing uses the `cwd` every conversational record carries — never the
//! lossy directory slug.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::{Harness, HistoryAdapter, HistoryEvent, RawRef, Role, parse_iso8601};
use crate::Result;

pub struct ClaudeCodeAdapter {
    root: Option<PathBuf>,
}

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self {
            root: crate::harness::home_file(".claude/projects"),
        }
    }

    /// Test/bench constructor: read transcripts from an explicit directory.
    pub fn with_root(root: PathBuf) -> Self {
        Self { root: Some(root) }
    }
}

impl Default for ClaudeCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryAdapter for ClaudeCodeAdapter {
    fn harness(&self) -> Harness {
        Harness::ClaudeCode
    }

    fn discover(&self) -> Vec<PathBuf> {
        let Some(root) = &self.root else {
            return vec![];
        };
        let mut files = Vec::new();
        for project in std::fs::read_dir(root).into_iter().flatten().flatten() {
            if !project.path().is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(project.path()).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "jsonl") {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    fn poll(&self, path: &Path, cursor: u64) -> Result<(Vec<HistoryEvent>, u64)> {
        let mut file = std::fs::File::open(path)
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;
        let len = file
            .metadata()
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?
            .len();
        // Shorter than the cursor = rotated/recreated; start over.
        let mut offset = if len < cursor { 0 } else { cursor };
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;
        let mut buf = Vec::with_capacity((len - offset) as usize);
        file.read_to_end(&mut buf)
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;

        let fallback_session = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut events = Vec::new();
        let mut last_ts = crate::store::now();
        let mut line_start = offset;
        for line in buf.split_inclusive(|b| *b == b'\n') {
            if !line.ends_with(b"\n") {
                break; // partial tail — the writer is mid-append; next sweep
            }
            let line_end = line_start + line.len() as u64;
            if let Some(ev) = parse_record(line, path, line_start, line_end, &fallback_session, &mut last_ts) {
                events.push(ev);
            }
            line_start = line_end;
            offset = line_end;
        }
        Ok((events, offset))
    }
}

fn parse_record(
    line: &[u8],
    path: &Path,
    start: u64,
    end: u64,
    fallback_session: &str,
    last_ts: &mut i64,
) -> Option<HistoryEvent> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let role = match v.get("type")?.as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    if v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true)
        || v.get("isMeta").and_then(|b| b.as_bool()) == Some(true)
        || v.get("toolUseResult").is_some()
    {
        return None;
    }
    let text = extract_text(v.get("message")?.get("content")?);
    if text.trim().is_empty() || is_scaffolding(&text) {
        return None;
    }
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(parse_iso8601)
        .unwrap_or(*last_ts);
    *last_ts = ts;
    Some(HistoryEvent {
        harness: Harness::ClaudeCode,
        project_hint: PathBuf::from(v.get("cwd").and_then(|c| c.as_str())?),
        session_id: v
            .get("sessionId")
            .and_then(|s| s.as_str())
            .unwrap_or(fallback_session)
            .to_string(),
        event_id: v.get("uuid")?.as_str()?.to_string(),
        role,
        timestamp: ts,
        text,
        raw_ref: RawRef {
            path: path.to_path_buf(),
            start,
            end,
        },
    })
}

/// Harness scaffolding wearing conversational records (found on the first
/// real backfill, 2026-08-12): slash-command invocations, their local
/// stdout, and hook/system-reminder wrappers all arrive as `type: user`
/// without `isMeta` — machine traffic, not the person talking.
fn is_scaffolding(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<command-name>")
        || t.starts_with("<command-message>")
        || t.starts_with("<local-command-caveat>")
        || t.starts_with("<local-command-stdout>")
        || t.starts_with("<system-reminder>")
        || t.starts_with("[SYSTEM NOTIFICATION")
        || t.starts_with("<task-notification>")
}

/// User content is a plain string or a block array; assistant content is
/// always a block array. Prose = the `text` blocks, nothing else.
fn extract_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}
