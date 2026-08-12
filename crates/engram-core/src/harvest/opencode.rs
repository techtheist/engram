//! opencode adapter: `~/.local/share/opencode/storage/message/<session>/msg_*.json`
//! — one JSON file per message, which makes incremental updates free: a
//! fully-ingested file never changes, so its cursor (= its length) skips it
//! forever. Routing comes from the session index under
//! `storage/session/**/ses_*.json`, whose documents carry the project
//! `directory`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{Harness, HistoryAdapter, HistoryEvent, RawRef, Role};
use crate::Result;

pub struct OpencodeAdapter {
    root: Option<PathBuf>,
    /// sessionID → project directory, rebuilt lazily from the session index.
    dirs: Mutex<HashMap<String, Option<PathBuf>>>,
}

impl OpencodeAdapter {
    pub fn new() -> Self {
        let root = crate::harness::home_file(".local/share/opencode/storage");
        Self {
            root,
            dirs: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self {
            root: Some(root),
            dirs: Mutex::new(HashMap::new()),
        }
    }

    /// The project directory a session belongs to, from the session index.
    fn dir_of(&self, session: &str) -> Option<PathBuf> {
        if let Some(hit) = self.dirs.lock().ok()?.get(session) {
            return hit.clone();
        }
        // One index sweep fills the cache for every session at once.
        let mut found: HashMap<String, Option<PathBuf>> = HashMap::new();
        if let Some(root) = &self.root {
            let mut stack = vec![root.join("session")];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    if path.extension().is_none_or(|e| e != "json") {
                        continue;
                    }
                    let Ok(raw) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
                        continue;
                    };
                    let Some(id) = v.get("id").and_then(|i| i.as_str()) else {
                        continue;
                    };
                    let d = ["directory", "path", "cwd", "worktree"]
                        .iter()
                        .find_map(|k| v.get(*k).and_then(|p| p.as_str()))
                        .map(PathBuf::from);
                    found.insert(id.to_string(), d);
                }
            }
        }
        let mut cache = self.dirs.lock().ok()?;
        cache.extend(found);
        cache.entry(session.to_string()).or_insert(None).clone()
    }
}

impl Default for OpencodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryAdapter for OpencodeAdapter {
    fn harness(&self) -> Harness {
        Harness::Opencode
    }

    fn discover(&self) -> Vec<PathBuf> {
        let Some(root) = &self.root else {
            return vec![];
        };
        let mut files = Vec::new();
        let msgs = root.join("message");
        for session in std::fs::read_dir(&msgs).into_iter().flatten().flatten() {
            for entry in std::fs::read_dir(session.path()).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("msg_"))
                {
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
            return Ok((vec![], len));
        };
        let role = match v.get("role").and_then(|r| r.as_str()) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => return Ok((vec![], len)),
        };
        let session_id = v
            .get("sessionID")
            .or_else(|| v.get("sessionId"))
            .and_then(|s| s.as_str().map(str::to_string))
            .or_else(|| {
                path.parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let Some(hint) = self.dir_of(&session_id) else {
            return Ok((vec![], len)); // no routing fact — skipped
        };
        // Text parts only; tool/step/patch parts are machine traffic.
        let text = v
            .get("parts")
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .or_else(|| {
                v.get("content")
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Ok((vec![], len));
        }
        let ts = v
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(|t| t.as_i64())
            .map(|ms| if ms > 100_000_000_000 { ms / 1000 } else { ms })
            .unwrap_or_else(crate::store::now);
        let event_id = v
            .get("id")
            .and_then(|i| i.as_str().map(str::to_string))
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        Ok((
            vec![HistoryEvent {
                harness: Harness::Opencode,
                project_hint: hint,
                session_id,
                event_id,
                role,
                timestamp: ts,
                text,
                raw_ref: RawRef {
                    path: path.to_path_buf(),
                    start: 0,
                    end: len,
                },
            }],
            len,
        ))
    }
}
