//! Gemini CLI adapter: `~/.gemini/tmp/<project-dir>/chats/*.json` — one JSON
//! document per saved chat, project-scoped by directory. Tool executions are
//! separate records and dropped; only user text and model prose survive.
//!
//! The format is tolerant by necessity (the CLI has reshaped it): messages
//! live under `messages` or `history`; a turn's text is a `content` string
//! or Gemini-style `parts: [{text}]`; roles are `user` / `model`. Routing
//! uses an explicit `projectPath`/`cwd`/`directory` field when the file
//! carries one — the directory name in the path is a hash and is never
//! decoded. Files rewrite in place, so the cursor is just "the length we
//! last parsed"; per-message ids (or stable indexes) make the reparse
//! idempotent through the harvester's seen-set.

use std::path::{Path, PathBuf};

use super::{Harness, HistoryAdapter, HistoryEvent, RawRef, Role, parse_iso8601};
use crate::Result;

pub struct GeminiAdapter {
    root: Option<PathBuf>,
}

impl GeminiAdapter {
    pub fn new() -> Self {
        Self {
            root: crate::harness::home_file(".gemini/tmp"),
        }
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self { root: Some(root) }
    }
}

impl Default for GeminiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryAdapter for GeminiAdapter {
    fn harness(&self) -> Harness {
        Harness::Gemini
    }

    fn discover(&self) -> Vec<PathBuf> {
        let Some(root) = &self.root else {
            return vec![];
        };
        let mut files = Vec::new();
        for project in std::fs::read_dir(root).into_iter().flatten().flatten() {
            let chats = project.path().join("chats");
            for entry in std::fs::read_dir(&chats).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    fn poll(&self, path: &Path, _cursor: u64) -> Result<(Vec<HistoryEvent>, u64)> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;
        let len = raw.len() as u64;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Ok((vec![], len)); // mid-write or foreign file — skip
        };
        let Some(hint) = ["projectPath", "cwd", "directory", "project"]
            .iter()
            .find_map(|k| v.get(*k).and_then(|p| p.as_str()))
            .map(PathBuf::from)
            // Newer CLIs drop the in-file field but write a `.project_root`
            // beside the chats directory (verified live 2026-08-12).
            .or_else(|| {
                let root_file = path.parent()?.parent()?.join(".project_root");
                let ws = std::fs::read_to_string(root_file).ok()?;
                let ws = ws.trim();
                (!ws.is_empty()).then(|| PathBuf::from(ws))
            })
        else {
            return Ok((vec![], len)); // no routing fact — skipped, not hoarded
        };
        let session_id = v
            .get("sessionId")
            .and_then(|s| s.as_str().map(str::to_string))
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        let file_ts = v
            .get("startTime")
            .or_else(|| v.get("lastUpdated"))
            .and_then(|t| t.as_str())
            .and_then(parse_iso8601)
            .unwrap_or_else(crate::store::now);

        let empty = vec![];
        let messages = v
            .get("messages")
            .or_else(|| v.get("history"))
            .and_then(|m| m.as_array())
            .unwrap_or(&empty);
        let mut events = Vec::new();
        let mut last_ts = file_ts;
        for (i, m) in messages.iter().enumerate() {
            let role = match m
                .get("role")
                .or_else(|| m.get("type"))
                .and_then(|r| r.as_str())
            {
                Some("user") => Role::User,
                Some("model") | Some("assistant") | Some("gemini") => Role::Assistant,
                _ => continue, // tool executions, info records
            };
            let text = match m.get("content").or_else(|| m.get("text")) {
                Some(serde_json::Value::String(s)) => s.clone(),
                _ => m
                    .get("parts")
                    .and_then(|p| p.as_array())
                    .map(|parts| {
                        parts
                            .iter()
                            // functionCall / functionResponse parts are tool
                            // traffic; only plain text survives.
                            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default(),
            };
            if text.trim().is_empty() {
                continue;
            }
            let ts = m
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(parse_iso8601)
                .unwrap_or(last_ts);
            last_ts = ts;
            events.push(HistoryEvent {
                harness: Harness::Gemini,
                project_hint: hint.clone(),
                session_id: session_id.clone(),
                event_id: m
                    .get("id")
                    .and_then(|i| i.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("{session_id}#{i}")),
                role,
                timestamp: ts,
                text,
                raw_ref: RawRef {
                    path: path.to_path_buf(),
                    start: 0,
                    end: len,
                },
            });
        }
        Ok((events, len))
    }
}
