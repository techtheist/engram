//! The integration point the HTTP and MCP layers talk to: a `Store` plus an
//! `Embedder`. It keeps embeddings in lockstep with node writes and exposes
//! the retrieval surface (hybrid search) so callers never touch vectors
//! directly.

use crate::nli::Nli;
use crate::rag::{Embedder, Reranker};
use crate::sync::MutexExt;
use crate::types::*;
use crate::{Result, Store};

mod review;
pub use review::{
    AroundView, ConflictsView, CrowdHeat, CrowdingReport, Island, IslandPage, Neighbor, ReviewEdge,
    ReviewLine, ReviewMember, ReviewPage, ReviewPair, ReviewQuery, ReviewSubject, StandingConflict,
    SubjectBridge, SubjectSummary, Worth,
};

mod audit_log;
mod bridged;
mod brief;
mod calibrate;
mod edges;
mod embedding;
mod encryption;
mod graph;
mod graph_config;
mod history;
mod maintenance;
mod merge;
mod nodes;
mod provenance;
mod search;
mod standing;
mod suspects;
mod sweeps;
mod text;
mod write;

use bridged::*;
pub use brief::*;
use calibrate::*;
pub use embedding::*;
use encryption::*;
use nodes::*;
pub use search::*;
use suspects::*;
use sweeps::*;
pub use text::*;
pub use write::*;

/// A graph mutation, broadcast to listeners (the HTTP layer turns these into
/// SSE so the pane updates live — regardless of whether the write came from
/// the API or from Claude over MCP).
#[derive(Clone, Debug)]
pub enum ChangeEvent {
    NodeAdded(Node),
    NodeUpdated(Node),
    NodeDeleted(String),
    EdgeAdded(Edge),
    EdgeUpdated(Edge),
    EdgeDeleted(String),
    /// The suspected-conflict queue changed (scan found pairs, or one was
    /// judged) — coarse on purpose; the pane refetches the pending list.
    SuspectsChanged,
    /// The per-graph configuration was replaced — the pane
    /// refetches `/config` and re-derives colors/labels.
    ConfigChanged,
    /// Tasks changed — coarse; the pane refetches `/tasks`.
    TasksChanged,
}

/// How many 1-hop neighbors ride along with each search hit.
const NEIGHBOR_CAP: usize = 5;
/// How many nearest nodes the write-time duplicate/conflict checks consider.
const WRITE_CHECK_K: usize = 8;

pub type Listener = Box<dyn Fn(ChangeEvent) + Send + Sync>;

/// Who is writing right now — stamped on every audit row. In the daemon the
/// pane (HTTP) and Claude (MCP) share one engine behind a mutex, so each
/// front-end re-stamps this under its lock before every operation; a
/// process-wide constant would misattribute the other side's writes.
#[derive(Clone, Debug)]
pub struct AuditOrigin {
    /// pane | mcp | daemon | cli | library
    pub origin: String,
    pub session_id: Option<String>,
}

impl AuditOrigin {
    pub fn pane() -> Self {
        Self {
            origin: "pane".into(),
            session_id: None,
        }
    }
    pub fn mcp(session_id: String) -> Self {
        Self {
            origin: "mcp".into(),
            session_id: Some(session_id),
        }
    }
    pub fn daemon() -> Self {
        Self {
            origin: "daemon".into(),
            session_id: None,
        }
    }
    pub fn cli() -> Self {
        Self {
            origin: "cli".into(),
            session_id: None,
        }
    }
}

impl Default for AuditOrigin {
    fn default() -> Self {
        Self {
            origin: "library".into(),
            session_id: None,
        }
    }
}

/// One note waiting for its birth exchange to be harvested.
struct ParkedProvenance {
    note_id: String,
    ts: i64,
    parked_at: i64,
}

pub struct Engine {
    store: Box<dyn Store>,
    /// Shared so a checked write can embed with the engine lock released
    ///: the vectors are computed before the lock, and only reused
    /// under it when the model is still this one.
    embedder: std::sync::Arc<dyn Embedder>,
    /// The precision layer: optional cross-encoder re-scoring of
    /// search candidates. Absent in tests, under `--fake-embeddings`, and
    /// when the model can't load — search then keeps plain hybrid order.
    reranker: Option<Box<dyn Reranker>>,
    /// The logic layer: optional local NLI. Nominations only —
    /// suspect hints, claim checks, audit sweeps; never touches trust.
    nli: Option<std::sync::Arc<dyn Nli>>,
    /// Repo root for write-time code_ref checks (serve/mcp set it).
    repo_root: Option<std::path::PathBuf>,
    /// The history layer: where this graph's sibling `history.tepin`
    /// lives, and the open handle when `config().history.enabled`. Interior-
    /// mutable because config writes (`&self`) open/drop it live. Never a hub
    /// engine — the librarian sweep (decay/conflicts/drift) can't reach it.
    history_path: Option<std::path::PathBuf>,
    history: std::sync::Mutex<Option<Box<dyn Store>>>,
    /// born-in provenance parking lot: notes written mid-session
    /// whose birth exchange the harvester hasn't ingested yet. In-memory on
    /// purpose — provenance is a footnote, losing a few entries to a daemon
    /// restart costs a link, never knowledge.
    provenance_lot: std::sync::Mutex<Vec<ParkedProvenance>>,
    /// Desired at-rest encryption per store — `(graph, history)` — installed
    /// by the daemon from machine settings. `None` = leave the
    /// store's own recorded state alone: library engines and tests never
    /// run migrations or touch the OS keychain.
    desired_graph_enc: Option<bool>,
    desired_history_enc: Option<bool>,
    /// Legacy read fallback: rows sealed by 0.8.x are normally opened by the
    /// store driver, but any path that meets a sealed string directly loads
    /// the machine key lazily — only when one is actually seen.
    legacy_key: std::sync::OnceLock<Option<crate::seal::SealKey>>,
    listeners: Vec<Listener>,
    audit_origin: AuditOrigin,
    /// Binary-side context captured once per process — the enrichment every
    /// audit row carries.
    audit_cwd: Option<String>,
    audit_pid: i64,
    /// The operation the journal rows being written belong to —
    /// set for the span of a multi-row operation, see [`Engine::op_scope`].
    audit_op: std::sync::Mutex<Option<String>>,
    audit_version: String,
    /// When [`Engine::validate_graph`] last ran — the session-boundary
    /// trigger consults this so back-to-back connects don't re-sweep. Per
    /// graph, not per process: this engine IS the graph's process-side.
    last_validated: std::sync::atomic::AtomicI64,
    /// The crowding index, rebuilt when the note set or a note's
    /// text changes — see [`Engine::crowding`].
    crowd_cache: std::sync::Mutex<Option<std::sync::Arc<crate::crowding::CrowdIndex>>>,
}

impl Engine {
    pub fn new(store: impl Store + 'static, embedder: Box<dyn Embedder>) -> Self {
        Self::with_store(Box::new(store), embedder)
    }

    /// Backend-agnostic form for callers that went through [`crate::open_store`].
    pub fn with_store(store: Box<dyn Store>, embedder: Box<dyn Embedder>) -> Self {
        Self {
            store,
            embedder: std::sync::Arc::from(embedder),
            reranker: None,
            nli: None,
            repo_root: None,
            history_path: None,
            history: std::sync::Mutex::new(None),
            provenance_lot: std::sync::Mutex::new(Vec::new()),
            desired_graph_enc: None,
            desired_history_enc: None,
            legacy_key: std::sync::OnceLock::new(),
            listeners: Vec::new(),
            audit_origin: AuditOrigin::default(),
            audit_cwd: std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string()),
            audit_pid: i64::from(std::process::id()),
            audit_op: std::sync::Mutex::new(None),
            audit_version: env!("CARGO_PKG_VERSION").to_string(),
            last_validated: std::sync::atomic::AtomicI64::new(0),
            crowd_cache: std::sync::Mutex::new(None),
        }
    }

    /// Add a change listener (the daemon wires SSE here; the hub adds its
    /// conflict-alert tap). Listeners accumulate — every mutation reaches
    /// all of them.
    pub fn add_listener(&mut self, listener: Listener) {
        self.listeners.push(listener);
    }

    /// Install the optional reranker (serve/mcp with real embeddings).
    pub fn set_reranker(&mut self, reranker: Box<dyn Reranker>) {
        self.reranker = Some(reranker);
    }

    /// Whether search runs the precision layer (surfaced by `/system`).
    pub fn has_reranker(&self) -> bool {
        self.reranker.is_some()
    }

    /// Install the optional NLI layer (serve/mcp with real embeddings).
    pub fn set_nli(&mut self, nli: Box<dyn Nli>) {
        self.nli = Some(std::sync::Arc::from(nli));
    }

    /// Whether the logic layer is loaded (surfaced by `/system`).
    pub fn has_nli(&self) -> bool {
        self.nli.is_some()
    }

    /// Whether search runs on fake (deterministic, non-semantic) vectors —
    /// surfaced by `/system` so the pane can say so.
    pub fn embeddings_are_fake(&self) -> bool {
        self.embedder.is_fake()
    }

    /// The active embedding model's identity.
    pub fn embed_model_id(&self) -> EmbedModelId {
        EmbedModelId {
            name: self.embedder.name().to_string(),
            dim: self.embedder.dim(),
        }
    }

    /// Swap the embedding model on a live engine (model selection). The
    /// caller must follow with [`Engine::ensure_embed_model`] — vectors from
    /// two models must never mix.
    pub fn set_embedder(&mut self, embedder: Box<dyn Embedder>) {
        self.embedder = std::sync::Arc::from(embedder);
    }

    /// Where write-time code_ref checks resolve paths (set by serve/mcp from
    /// the DB location). Unset = ref checks are skipped, never guessed.
    pub fn set_repo_root(&mut self, root: std::path::PathBuf) {
        self.repo_root = Some(root);
    }

    /// The repo this engine's store belongs to, when known — drift scans on a
    /// scoped project must use *its* root, never the daemon's cwd.
    pub fn repo_root(&self) -> Option<&std::path::Path> {
        self.repo_root.as_deref()
    }

    /// [`Engine::notify`] for sibling modules (tasks).
    pub(crate) fn emit(&self, event: ChangeEvent) {
        self.notify(event);
    }

    fn notify(&self, event: ChangeEvent) {
        match self.listeners.as_slice() {
            [] => {}
            [one] => one(event),
            many => {
                for l in many {
                    l(event.clone());
                }
            }
        }
    }

    pub fn store(&self) -> &dyn Store {
        self.store.as_ref()
    }

    // ---- audit journal: every mutation appends one row with
    // before/after snapshots and this process's context. Reads (search touch,
    // brief inclusion) are deliberately not journaled — they'd drown the edits.

    /// The loaded logic layer, or the uniform sweeps-need-NLI error.
    fn require_nli(&self) -> Result<&dyn Nli> {
        self.nli.as_deref().ok_or_else(|| {
            crate::Error::Embedding(
                "the NLI model is not loaded — audit sweeps need the local logic layer".into(),
            )
        })
    }
}

/// Local alias so [`field_value_ok`] can return a plain-string error without
/// colliding with the crate-wide `Result`.
type Result2<T, E> = std::result::Result<T, E>;
