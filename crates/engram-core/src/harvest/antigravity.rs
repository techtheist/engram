//! Antigravity adapter: `~/.gemini/antigravity-cli/brain/<conversation>/`
//! `.system_generated/logs/transcript.jsonl` — append-only JSONL, one step
//! per line (format verified against a live install, 2026-08-12).
//!
//! Kept: `USER_INPUT` steps (the prose inside `<USER_REQUEST>…</USER_REQUEST>`
//! — the rest of the step is settings/metadata scaffolding) and
//! `PLANNER_RESPONSE` steps that carry `content` (an empty one is a
//! tool-call step). Dropped: MCP_TOOL / VIEW_FILE / CHECKPOINT /
//! CONVERSATION_HISTORY and every other machine step.
//!
//! Routing: the transcript itself never names the workspace. Two sibling
//! files in the antigravity home do — `history.jsonl` rows carry
//! `{conversationId, workspace}`, and `cache/last_conversations.json` maps
//! workspace → conversation id. Both are consulted; no route means the
//! delta is skipped, not hoarded (the map is written before any step that
//! matters in practice).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::{Harness, HistoryAdapter, HistoryEvent, RawRef, Role, parse_iso8601};
use crate::Result;

pub struct AntigravityAdapter {
    root: Option<PathBuf>,
}

impl AntigravityAdapter {
    pub fn new() -> Self {
        Self {
            root: crate::harness::home_file(".gemini/antigravity-cli"),
        }
    }

    /// Test constructor: an explicit antigravity home.
    pub fn with_root(root: PathBuf) -> Self {
        Self { root: Some(root) }
    }
}

impl Default for AntigravityAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryAdapter for AntigravityAdapter {
    fn harness(&self) -> Harness {
        Harness::Antigravity
    }

    fn discover(&self) -> Vec<PathBuf> {
        let Some(root) = &self.root else {
            return vec![];
        };
        let mut files = Vec::new();
        for conv in std::fs::read_dir(root.join("brain")).into_iter().flatten().flatten() {
            let t = conv.path().join(".system_generated/logs/transcript.jsonl");
            if t.is_file() {
                files.push(t);
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

        // ancestors: [file, logs, .system_generated, <conversation>, brain, home]
        let session_id = path
            .ancestors()
            .nth(3)
            .and_then(|d| d.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(workspace) = workspace_of(path, &session_id) else {
            return Ok((vec![], len)); // no routing fact yet — skipped, not hoarded
        };

        let mut events = Vec::new();
        let mut last_ts = crate::store::now();
        let mut line_start = offset;
        for line in buf.split_inclusive(|b| *b == b'\n') {
            if !line.ends_with(b"\n") {
                break; // partial tail — the writer is mid-append; next sweep
            }
            let line_end = line_start + line.len() as u64;
            if let Some((role, text, ts, step)) = parse_step(line, &mut last_ts) {
                events.push(HistoryEvent {
                    harness: Harness::Antigravity,
                    project_hint: workspace.clone(),
                    session_id: session_id.clone(),
                    event_id: format!("step-{step}"),
                    role,
                    timestamp: ts,
                    text,
                    raw_ref: RawRef {
                        path: path.to_path_buf(),
                        start: line_start,
                        end: line_end,
                    },
                });
            }
            line_start = line_end;
            offset = line_end;
        }
        Ok((events, offset))
    }
}

/// One transcript step → a conversational turn, or None for machine traffic.
fn parse_step(line: &[u8], last_ts: &mut i64) -> Option<(Role, String, i64, u64)> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let step = v.get("step_index")?.as_u64()?;
    let content = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
    let (role, text) = match v.get("type")?.as_str()? {
        "USER_INPUT" => (Role::User, user_request(content)?),
        // An empty PLANNER_RESPONSE is a tool-call step; prose ones carry
        // the assistant's actual reply.
        "PLANNER_RESPONSE" if !content.trim().is_empty() => {
            (Role::Assistant, content.to_string())
        }
        _ => return None,
    };
    let ts = v
        .get("created_at")
        .and_then(|t| t.as_str())
        .and_then(parse_iso8601)
        .unwrap_or(*last_ts);
    *last_ts = ts;
    Some((role, text, ts, step))
}

/// The person's words inside a USER_INPUT step: the `<USER_REQUEST>` block —
/// everything around it (ADDITIONAL_METADATA, USER_SETTINGS_CHANGE, …) is
/// harness scaffolding. A step without the tag is all scaffolding.
fn user_request(content: &str) -> Option<String> {
    let start = content.find("<USER_REQUEST>")? + "<USER_REQUEST>".len();
    let end = content[start..].find("</USER_REQUEST>")? + start;
    let text = content[start..end].trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// conversation id → workspace, from the antigravity home two levels above
/// `brain/`: `history.jsonl` rows and the `cache/last_conversations.json`
/// reverse map.
fn workspace_of(transcript: &Path, conversation: &str) -> Option<PathBuf> {
    let home = transcript.ancestors().nth(5)?;
    if let Ok(raw) = std::fs::read_to_string(home.join("history.jsonl")) {
        for line in raw.lines().rev() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v.get("conversationId").and_then(|c| c.as_str()) == Some(conversation)
                && let Some(ws) = v.get("workspace").and_then(|w| w.as_str())
            {
                return Some(PathBuf::from(ws));
            }
        }
    }
    let raw = std::fs::read_to_string(home.join("cache/last_conversations.json")).ok()?;
    let map: serde_json::Value = serde_json::from_str(&raw).ok()?;
    map.as_object()?
        .iter()
        .find(|(_, id)| id.as_str() == Some(conversation))
        .map(|(ws, _)| PathBuf::from(ws))
}
