//! IBM Bob adapter: one SQLite store for BOTH surfaces — the Bob IDE and
//! BobShell share `~/.bob/db/bob.db` (`tasks` + `messages`; the IDE's
//! "cross-workspace task browsing" is backed by it). Format verified live
//! against a Bob IDE 2.x install (2026-08-13); the BobShell half is the same
//! tables by construction, unverified only because the CLI's login was down.
//! One adapter, one harness toggle, deliberately.
//!
//! Shape: `tasks(id, project_id 'file:…' URI, directory, task_type, …)`,
//! `messages(id, task_id, role, data JSON, created_at ms)`. Kept: `user` and
//! `assistant` rows with non-empty prose `content` (assistant tool-call rows
//! carry empty content and drop out naturally); `system`/`tool` rows never
//! leave the query. Tasks typed `subagent` are sidechain traffic and drop at
//! parse level, the standing rule. Routing: the task's `project_id` file URI
//! (fallback: its `directory` column).
//!
//! The cursor is a TIMESTAMP (max `messages.created_at` ingested, in ms), not
//! a byte offset — a database has no append frontier. Bob flushes message
//! batches with one shared `created_at`, so the query re-reads the boundary
//! millisecond (`>=`) and the harvester's seen-set dedupes the overlap.

use std::path::{Path, PathBuf};

use super::{Harness, HistoryAdapter, HistoryEvent, RawRef, Role};
use crate::Result;

pub struct BobAdapter {
    db: Option<PathBuf>,
}

impl BobAdapter {
    pub fn new() -> Self {
        Self {
            db: crate::harness::home_file(".bob/db/bob.db"),
        }
    }

    /// Test constructor: an explicit database file.
    pub fn with_db(db: PathBuf) -> Self {
        Self { db: Some(db) }
    }
}

impl Default for BobAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryAdapter for BobAdapter {
    fn harness(&self) -> Harness {
        Harness::Bob
    }

    fn discover(&self) -> Vec<PathBuf> {
        self.db.iter().filter(|p| p.is_file()).cloned().collect()
    }

    fn poll(&self, path: &Path, cursor: u64) -> Result<(Vec<HistoryEvent>, u64)> {
        let conn = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.role, m.data, m.created_at,
                        t.id, t.project_id, t.directory, t.task_type
                 FROM messages m JOIN tasks t ON t.id = m.task_id
                 WHERE m.created_at >= ?1 AND m.role IN ('user', 'assistant')
                 ORDER BY m.created_at ASC, m.id ASC",
            )
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;
        let rows = stmt
            .query_map([cursor as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, String>(7)?,
                ))
            })
            .map_err(|e| crate::Error::Io(format!("{}: {e}", path.display())))?;

        let mut events = Vec::new();
        let mut frontier = cursor;
        for row in rows.flatten() {
            let (msg_id, role, data, row_ms, task, project_id, directory, task_type) = row;
            frontier = frontier.max(row_ms as u64);
            if task_type == "subagent" {
                continue; // sidechain traffic, dropped at parse level
            }
            let Some(hint) = workspace_of(&project_id, directory.as_deref()) else {
                continue; // playground / URI-less task — no routing fact
            };
            let role = match role.as_str() {
                "user" => Role::User,
                _ => Role::Assistant,
            };
            let Ok(data) = serde_json::from_str::<serde_json::Value>(&data) else {
                continue;
            };
            let text = prose_of(&data);
            if text.trim().is_empty() {
                continue; // assistant tool-call rows carry empty content
            }
            // Bob batches rows with one flush-time created_at; the message's
            // own moment lives in _meta when present.
            let ts_ms = data
                .pointer("/_meta/timestamp")
                .and_then(|t| t.as_i64())
                .unwrap_or(row_ms);
            events.push(HistoryEvent {
                harness: Harness::Bob,
                project_hint: hint,
                session_id: task,
                event_id: msg_id,
                role,
                timestamp: ts_ms / 1000,
                text,
                raw_ref: RawRef {
                    path: path.to_path_buf(),
                    start: 0,
                    end: 0, // a row, not a byte range — `event` is the pointer
                },
            });
        }
        Ok((events, frontier))
    }
}

/// The task's workspace: `project_id` is a `file:` URI ("file:/Users/…" as
/// written, "file:///…" tolerated); the `directory` column is the fallback.
fn workspace_of(project_id: &str, directory: Option<&str>) -> Option<PathBuf> {
    let from_uri = project_id
        .strip_prefix("file://")
        .or_else(|| project_id.strip_prefix("file:"))
        .filter(|p| !p.is_empty());
    from_uri
        .or(directory.filter(|d| !d.is_empty()))
        .map(PathBuf::from)
}

/// The prose of a message row: `content` as a string, or the `text` blocks
/// of an Anthropic-style array. Tool calls, envContext and metadata never
/// count as prose.
fn prose_of(data: &serde_json::Value) -> String {
    match data.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}
