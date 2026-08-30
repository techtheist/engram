//! The history layer (0.8.4): coding-assistant chat transcripts as an
//! episodic record BENEATH the curated graph — Session/Message nodes in a
//! sibling `history.tepin` store per project, cross-linked to curated memory
//! by `born-in` provenance, and reachable only through the sectioned search
//! fall-through and the pane's history view.
//!
//! Isolation is physical, not a filter: history nodes live in their own
//! store, so curated search, the brief, drift, decay and the suspect scan
//! never see them. The history store is opened by the [`crate::Engine`] that
//! owns the curated store and is deliberately never registered with the hub —
//! the daemon's librarian sweep can't reach it. History nodes are records,
//! not knowledge: no trust, no staleness, no conflicts.
//!
//! Knobs live in [`crate::config::HistoryConfig`] on the CURATED graph's
//! config (the pane edits it there); this store's own config document only
//! carries the chat ontology below.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::config::{GraphConfig, OntologyConfig, TypeRoles, VerbRoles, hidden_brief, tdef, vdef};
use crate::store::Store;
use crate::types::Durability;

/// The sibling store's file name, always beside the curated store.
pub const HISTORY_STORE_FILE: &str = "history.tepin";

/// Node types of the chat ontology (ontology-as-data, the LongMemEval move —
/// outside the curated 8-type count, in a store of their own).
pub const SESSION_TYPE: &str = "Session";
pub const MESSAGE_TYPE: &str = "Message";

/// Verbs: the chain IS the history.
/// `Message in Session`; `Message next Message` / `Session next Session`;
/// curated node `born-in` Message (provenance, written by the MCP layer).
pub const VERB_IN: &str = "in";
pub const VERB_NEXT: &str = "next";
pub const VERB_BORN_IN: &str = "born-in";

/// One history-layer search hit: a snippet plus the handles to expand it
/// (decision 00bgftfdusll — snippets + handles, the model decides how much
/// raw dialogue to spend context on). Never score-blended with curated hits.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryHit {
    pub message_id: String,
    /// The harness's session identity — what `expand_history` takes.
    pub session: String,
    pub session_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub timestamp: i64,
    pub snippet: String,
    pub score: f64,
    /// Earlier statements of the same thing, newest-first, folded under this
    /// hit by `order: "recent"` (0.8.7) — the episodic echo of the curated
    /// graph's `replaces` chain, except the chain is inferred at query time
    /// rather than judged, because nobody curates a transcript.
    ///
    /// Folding is PRESENTATION: every hit the search delivered is still here,
    /// nested instead of cut. A wrong fold costs shape, never recall.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prior: Vec<HistoryHit>,
}

/// One message of an expanded exchange, in conversation order.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryMessageView {
    pub message_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub timestamp: i64,
    pub text: String,
}

/// One session row for the pane's history browser: lanes are ordered by
/// `started`, blocks sized by `messages`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistorySessionView {
    /// The Session node id (pane selection handle).
    pub node_id: String,
    /// The harness's session identity — what messages key on.
    pub session: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    pub started: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<i64>,
    pub messages: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Where a curated node was born (its `born-in` edge, resolved): enough for
/// the provenance line on a search hit and the pane's history chip.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BornIn {
    pub session: String,
    pub message_id: String,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
}

/// One curated note born inside a recorded session — the reverse of
/// [`BornIn`], for "what did this conversation leave in memory".
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionNote {
    /// The curated node's id (fetch it with `get_node`).
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub title: String,
    /// The turn of the birth exchange, when the message recorded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub message_id: String,
    pub timestamp: i64,
}

/// Where a project's history store lives: `history.tepin` beside the
/// (resolved) curated store — `.engram/graph.tepin` → `.engram/history.tepin`.
pub fn history_store_path(db: &Path) -> PathBuf {
    let resolved = crate::store::resolve_db_path(db);
    match resolved.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(HISTORY_STORE_FILE),
        _ => PathBuf::from(HISTORY_STORE_FILE),
    }
}

/// The history store's own config: the chat ontology. Session and Message
/// plus the three history verbs; `replaces`/`conflicts-with` ride along
/// dormant only because validation requires exactly one of each role — no
/// scan ever runs here to act on them.
pub fn history_ontology() -> GraphConfig {
    let plain = TypeRoles {
        worklist: false,
        anchor: false,
        rank_prior: 0.0,
        highlight: true,
        // The harvester stamps versions explicitly; nothing auto-stamps here.
        versioned: false,
        tombstone: false,
    };
    GraphConfig {
        ontology: OntologyConfig {
            preset: "history".into(),
            types: vec![
                tdef(
                    SESSION_TYPE,
                    262,
                    "one recorded conversation with a coding assistant",
                    Durability::Stable,
                    plain.clone(),
                    hidden_brief(),
                ),
                tdef(
                    MESSAGE_TYPE,
                    199,
                    "one turn of a recorded conversation",
                    Durability::Stable,
                    plain,
                    hidden_brief(),
                ),
            ],
            verbs: vec![
                vdef(VERB_IN, "Message in Session", VerbRoles::default()),
                vdef(VERB_NEXT, "Message next Message", VerbRoles::default()),
                vdef(
                    VERB_BORN_IN,
                    "Decision born-in Message",
                    VerbRoles::default(),
                ),
                vdef(
                    "replaces",
                    "Session replaces Session",
                    VerbRoles {
                        supersession: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "conflicts-with",
                    "Session conflicts-with Session",
                    VerbRoles {
                        contradiction: true,
                        ..VerbRoles::default()
                    },
                ),
            ],
        },
        ..GraphConfig::default()
    }
}

// ---------------------------------------------------------------------------
// at-rest sealing (decision 00bgftfausll, caution 00bgftfbusll) — the codec,
// key, and state machinery moved to `crate::seal` in 0.9.0 when the curated
// graph gained the same option; re-exported here for the history-era names.
// ---------------------------------------------------------------------------

pub use crate::seal::{SEAL_PREFIX, is_sealed};

/// 0.8.4 name for [`crate::seal::SealKey`] — the same machine key seals both
/// stores.
pub type HistoryKey = crate::seal::SealKey;

/// Open (creating if absent) a project's history store and make sure it
/// carries the chat ontology. A store that already has a config keeps it —
/// reopening never rewrites.
pub fn open_history_store(path: &Path) -> Result<Box<dyn Store>> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| crate::Error::Io(format!("creating {}: {e}", dir.display())))?;
    }
    let store = crate::store::open_store(path)?;
    if store.graph_config()?.is_none() {
        store.set_graph_config(&serde_json::to_string(&history_ontology())?)?;
    }
    Ok(store)
}
