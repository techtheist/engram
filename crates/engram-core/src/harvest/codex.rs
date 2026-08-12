//! Codex CLI adapter: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`,
//! typed JSONL events. Files can be huge (700 MB seen in the wild) — reads
//! are buffered line-by-line, never slurped.
//!
//! Record taxonomy (verified on real rollouts): `session_meta` carries the
//! session id and cwd (the routing fact — individual records carry neither);
//! prose lives in `response_item` events whose payload is
//! `{type: "message", role: user|assistant, content: [input_text|output_text]}`.
//! Everything else — `reasoning`, `custom_tool_call(_output)`,
//! `function_call(_output)`, `event_msg`, `world_state`, `turn_context`,
//! `developer` messages — is dropped at parse level. Codex also injects
//! harness scaffolding as user-role messages (`<environment_context>`,
//! `<user_instructions>`, …); those are machine traffic and dropped by their
//! leading tag.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::{Harness, HistoryAdapter, HistoryEvent, RawRef, Role, parse_iso8601};
use crate::Result;

pub struct CodexAdapter {
    root: Option<PathBuf>,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            root: crate::harness::home_file(".codex/sessions"),
        }
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self { root: Some(root) }
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// The session identity a rollout's `session_meta` line carries.
struct Meta {
    session_id: Option<String>,
    cwd: Option<PathBuf>,
}

fn read_meta(v: &serde_json::Value, meta: &mut Meta) {
    if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return;
    }
    let Some(p) = v.get("payload") else { return };
    if meta.session_id.is_none() {
        meta.session_id = p
            .get("id")
            .or_else(|| p.get("session_id"))
            .and_then(|s| s.as_str().map(str::to_string));
    }
    if meta.cwd.is_none() {
        meta.cwd = p.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from);
    }
}

/// Harness scaffolding injected with `role: user` — not the person talking.
fn is_scaffolding(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<environment_context>")
        || t.starts_with("<user_instructions>")
        || t.starts_with("<turn_context>")
        || t.starts_with("<permissions")
}

impl HistoryAdapter for CodexAdapter {
    fn harness(&self) -> Harness {
        Harness::Codex
    }

    fn discover(&self) -> Vec<PathBuf> {
        let Some(root) = &self.root else {
            return vec![];
        };
        // YYYY/MM/DD nesting, three levels then files.
        let mut files = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "jsonl") {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    fn poll(&self, path: &Path, cursor: u64) -> Result<(Vec<HistoryEvent>, u64)> {
        let file = std::fs::File::open(path)
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;
        let len = file
            .metadata()
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?
            .len();
        let start_at = if len < cursor { 0 } else { cursor };
        let mut reader = BufReader::with_capacity(1 << 20, file);

        let mut meta = Meta {
            session_id: None,
            cwd: None,
        };
        // Resuming mid-file (daemon restart): the routing facts live on the
        // FIRST line — read it before seeking to the cursor.
        if start_at > 0 {
            let mut head = Vec::new();
            let _ = reader.read_until(b'\n', &mut head);
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&head) {
                read_meta(&v, &mut meta);
            }
        }
        reader
            .seek(SeekFrom::Start(start_at))
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;

        let fallback_session = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut events = Vec::new();
        let mut offset = start_at;
        let mut last_ts = crate::store::now();
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader
                .read_until(b'\n', &mut line)
                .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;
            if n == 0 {
                break;
            }
            if !line.ends_with(b"\n") {
                break; // partial tail mid-append — next sweep
            }
            let (start, end) = (offset, offset + n as u64);
            offset = end;
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            read_meta(&v, &mut meta);
            if v.get("type").and_then(|t| t.as_str()) != Some("response_item") {
                continue;
            }
            let Some(p) = v.get("payload") else { continue };
            if p.get("type").and_then(|t| t.as_str()) != Some("message") {
                continue;
            }
            let role = match p.get("role").and_then(|r| r.as_str()) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue, // developer / system scaffolding
            };
            let text = p
                .get("content")
                .and_then(|c| c.as_array())
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| {
                            matches!(
                                b.get("type").and_then(|t| t.as_str()),
                                Some("input_text") | Some("output_text") | Some("text")
                            )
                        })
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_default();
            if text.trim().is_empty() || (role == Role::User && is_scaffolding(&text)) {
                continue;
            }
            let ts = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(parse_iso8601)
                .unwrap_or(last_ts);
            last_ts = ts;
            let Some(hint) = meta.cwd.clone() else {
                continue; // no cwd yet — unroutable, skip the record
            };
            events.push(HistoryEvent {
                harness: Harness::Codex,
                project_hint: hint,
                session_id: meta
                    .session_id
                    .clone()
                    .unwrap_or_else(|| fallback_session.clone()),
                event_id: p
                    .get("id")
                    .and_then(|i| i.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("off-{start}")),
                role,
                timestamp: ts,
                text,
                raw_ref: RawRef {
                    path: path.to_path_buf(),
                    start,
                    end,
                },
            });
        }
        Ok((events, offset))
    }
}
