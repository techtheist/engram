//! Kilo Code adapter: the VS Code extension's global storage —
//! `…/User/globalStorage/kilocode.kilo-code/tasks/<task>/ui_messages.json`,
//! one JSON array per task (the Cline lineage Kilo forked from). Files
//! rewrite in place, so the cursor is "the length we last parsed" and the
//! reparse stays idempotent through the harvester's seen-set.
//!
//! Kept: `say: "task"` and `say: "user_feedback"` (the person),
//! `say: "text"` and `say: "completion_result"` (the assistant's prose).
//! Everything else — api_req_started, tool asks, browser traffic — is
//! machine noise and dropped.
//!
//! Routing: the task folder never names its workspace; the extension's
//! `taskHistory` global state does. It lives in the editor's `state.vscdb`
//! (SQLite `ItemTable`, key `kilocode.kilo-code`) one directory above the
//! extension's storage — read-only, resolved per task id.

use std::path::{Path, PathBuf};

use super::{Harness, HistoryAdapter, HistoryEvent, RawRef, Role};
use crate::Result;

const EXTENSION_ID: &str = "kilocode.kilo-code";

pub struct KiloAdapter {
    /// Candidate `globalStorage` directories (one per installed editor).
    roots: Vec<PathBuf>,
}

impl KiloAdapter {
    pub fn new() -> Self {
        let mut roots = Vec::new();
        let editors = ["Code", "Code - Insiders", "VSCodium", "Cursor", "Windsurf"];
        for editor in editors {
            if cfg!(target_os = "macos") {
                if let Some(p) = crate::harness::home_file(&format!(
                    "Library/Application Support/{editor}/User/globalStorage"
                )) {
                    roots.push(p);
                }
            } else if cfg!(target_os = "windows") {
                if let Ok(appdata) = std::env::var("APPDATA") {
                    roots.push(
                        PathBuf::from(appdata)
                            .join(editor)
                            .join("User/globalStorage"),
                    );
                }
            } else if let Some(p) =
                crate::harness::home_file(&format!(".config/{editor}/User/globalStorage"))
            {
                roots.push(p);
            }
        }
        Self { roots }
    }

    /// Test constructor: explicit `globalStorage` directories.
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

impl Default for KiloAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryAdapter for KiloAdapter {
    fn harness(&self) -> Harness {
        Harness::Kilo
    }

    fn discover(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for root in &self.roots {
            let tasks = root.join(EXTENSION_ID).join("tasks");
            for task in std::fs::read_dir(&tasks).into_iter().flatten().flatten() {
                let f = task.path().join("ui_messages.json");
                if f.is_file() {
                    files.push(f);
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
        let Ok(serde_json::Value::Array(messages)) = serde_json::from_str(&raw) else {
            return Ok((vec![], len)); // mid-write or foreign file — skip
        };
        // tasks/<task>/ui_messages.json
        let task_id = path
            .parent()
            .and_then(|d| d.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(workspace) = workspace_of(path, &task_id) else {
            return Ok((vec![], len)); // no routing fact — skipped, not hoarded
        };

        let mut events = Vec::new();
        let mut last_ts = crate::store::now();
        for m in &messages {
            if m.get("type").and_then(|t| t.as_str()) != Some("say") {
                continue; // asks are tool/permission traffic
            }
            let role = match m.get("say").and_then(|s| s.as_str()) {
                Some("task") | Some("user_feedback") => Role::User,
                Some("text") | Some("completion_result") => Role::Assistant,
                _ => continue,
            };
            let text = m
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            if text.trim().is_empty() {
                continue;
            }
            let ts_ms = m
                .get("ts")
                .and_then(|t| t.as_i64())
                .unwrap_or(last_ts * 1000);
            let ts = ts_ms / 1000;
            last_ts = ts;
            events.push(HistoryEvent {
                harness: Harness::Kilo,
                project_hint: workspace.clone(),
                session_id: task_id.clone(),
                // The millisecond stamp is the message's stable identity
                // across whole-file reparses.
                event_id: format!("ev-{ts_ms}"),
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

/// task id → workspace, from the editor's `state.vscdb` two directories
/// above the task folder (…/globalStorage/kilocode.kilo-code/tasks/<task>).
fn workspace_of(ui_messages: &Path, task_id: &str) -> Option<PathBuf> {
    // ancestors: [file, <task>, tasks, kilocode.kilo-code, globalStorage]
    let global_storage = ui_messages.ancestors().nth(4)?;
    let db = global_storage.join("state.vscdb");
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let raw: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?1",
            [EXTENSION_ID],
            |row| row.get(0),
        )
        .ok()?;
    let state: serde_json::Value = serde_json::from_str(&raw).ok()?;
    state
        .get("taskHistory")?
        .as_array()?
        .iter()
        .find(|t| t.get("id").and_then(|i| i.as_str()) == Some(task_id))?
        .get("workspace")?
        .as_str()
        .map(PathBuf::from)
}
