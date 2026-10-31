//! The integration point the HTTP and MCP layers talk to: a `Store` plus an
//! `Embedder`. It keeps embeddings in lockstep with node writes and exposes
//! the retrieval surface (hybrid search) so callers never touch vectors
//! directly.

use crate::nli::Nli;
use crate::rag::{Embedder, Reranker};
use crate::types::*;
use crate::{Result, Store};

mod review;
pub use review::{
    AroundView, ConflictsView, CrowdHeat, CrowdingReport, Island, IslandPage, Neighbor, ReviewEdge,
    ReviewLine, ReviewMember, ReviewPage, ReviewPair, ReviewQuery, ReviewSubject, StandingConflict,
    SubjectSummary, Worth,
};

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
    /// The per-graph configuration was replaced (PLAN §7D) — the pane
    /// refetches `/config` and re-derives colors/labels.
    ConfigChanged,
    /// Tasks (0.10.0) changed — coarse; the pane refetches `/tasks`.
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
    /// (0.9.11): the vectors are computed before the lock, and only reused
    /// under it when the model is still this one.
    embedder: std::sync::Arc<dyn Embedder>,
    /// The precision layer (PLAN §7A): optional cross-encoder re-scoring of
    /// search candidates. Absent in tests, under `--fake-embeddings`, and
    /// when the model can't load — search then keeps plain hybrid order.
    reranker: Option<Box<dyn Reranker>>,
    /// The logic layer (PLAN §7A): optional local NLI. Nominations only —
    /// suspect hints, claim checks, audit sweeps; never touches trust.
    nli: Option<std::sync::Arc<dyn Nli>>,
    /// Repo root for write-time code_ref checks (serve/mcp set it).
    repo_root: Option<std::path::PathBuf>,
    /// The history layer (0.8.4): where this graph's sibling `history.tepin`
    /// lives, and the open handle when `config().history.enabled`. Interior-
    /// mutable because config writes (`&self`) open/drop it live. Never a hub
    /// engine — the librarian sweep (decay/conflicts/drift) can't reach it.
    history_path: Option<std::path::PathBuf>,
    history: std::sync::Mutex<Option<Box<dyn Store>>>,
    /// born-in provenance parking lot (0.8.4): notes written mid-session
    /// whose birth exchange the harvester hasn't ingested yet. In-memory on
    /// purpose — provenance is a footnote, losing a few entries to a daemon
    /// restart costs a link, never knowledge.
    provenance_lot: std::sync::Mutex<Vec<ParkedProvenance>>,
    /// Desired at-rest encryption per store — `(graph, history)` — installed
    /// by the daemon from machine settings (0.9.0). `None` = leave the
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
    /// audit row carries (PLAN §10 audit journal).
    audit_cwd: Option<String>,
    audit_pid: i64,
    /// The operation the journal rows being written belong to (0.9.10) —
    /// set for the span of a multi-row operation, see [`Engine::op_scope`].
    audit_op: std::sync::Mutex<Option<String>>,
    audit_version: String,
    /// When [`Engine::validate_graph`] last ran — the session-boundary
    /// trigger consults this so back-to-back connects don't re-sweep. Per
    /// graph, not per process: this engine IS the graph's process-side.
    last_validated: std::sync::atomic::AtomicI64,
    /// The crowding index (0.11.0), rebuilt when the note set or a note's
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
            audit_pid: std::process::id() as i64,
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

    /// The active embedding model's identity (PLAN §7A model selection).
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

    /// Tell this engine where its history store lives (serve wires it to
    /// `history.tepin` beside the curated store). Opens right away when the
    /// graph's history config is enabled.
    pub fn set_history_path(&mut self, path: std::path::PathBuf) {
        self.history_path = Some(path);
        self.sync_history_store();
    }

    /// Where this graph's history store lives, when serve installed it.
    pub fn history_path(&self) -> Option<&std::path::Path> {
        self.history_path.as_deref()
    }

    /// Run `f` against the history layer's store — `None` when the layer is
    /// disabled, pathless, or failed to open. The closure shape (rather than
    /// a borrow) is what lets config writes swap the handle under `&self`.
    pub fn with_history<R>(&self, f: impl FnOnce(&dyn Store) -> R) -> Option<R> {
        match self.history.lock() {
            Ok(guard) => guard.as_deref().map(f),
            Err(_) => None,
        }
    }

    /// Whether the history layer is live on this engine right now.
    pub fn history_open(&self) -> bool {
        self.history.lock().is_ok_and(|g| g.is_some())
    }

    /// Install the daemon's desired at-rest encryption (0.9.0, from machine
    /// settings): `Some(true)` = keep that store sealed, `Some(false)` =
    /// keep it plaintext, `None` = leave its recorded state alone. Replaces
    /// 0.8.4's `enable_history_sealing` opt-in — the truth now lives in
    /// each store's own meta, and this is only the daemon's INTENT.
    pub fn set_desired_encryption(&mut self, graph: Option<bool>, history: Option<bool>) {
        self.desired_graph_enc = graph;
        self.desired_history_enc = history;
    }

    /// Decrypt one stored string for a reader. The store driver normally
    /// opens sealed rows itself; this is the legacy fallback for strings
    /// sealed by 0.8.x paths — the key loads lazily, only when a sealed
    /// string is actually met.
    fn unseal_str(&self, s: &str) -> String {
        if !crate::seal::is_sealed(s) {
            return s.to_string();
        }
        self.legacy_key
            .get_or_init(crate::seal::SealKey::load_or_create)
            .as_ref()
            .and_then(|k| k.unseal(s))
            .unwrap_or_else(|| crate::seal::SEALED_PLACEHOLDER.to_string())
    }

    /// Write one node into the history layer and embed it there — the
    /// harvester's write path. Deliberately NOT [`Engine::add_node`]: no
    /// audit row, no change event, no dupe verdicts, no version stamp —
    /// history is records, not knowledge.
    ///
    /// Sealing is the STORE DRIVER's job since 0.9.0 (its recorded state
    /// decides); this path scrubs plaintext and embeds the scrubbed
    /// plaintext — vectors stay deliberately open, the documented inversion
    /// risk. `Ok(None)` when the layer is disabled or closed.
    pub fn add_history_node(&self, mut n: NewNode) -> Result<Option<Node>> {
        n.title = crate::redact::scrub(&n.title);
        n.body = n.body.as_deref().map(crate::redact::scrub);
        let Some(node) = self.with_history(|s| s.add_node(n)).transpose()? else {
            return Ok(None);
        };
        // The driver returned the node it stored, opened for us — plaintext.
        let mut texts = vec![embed_text(
            &node.title,
            node.body.as_deref(),
            &node.tags,
            &node.code_refs,
        )];
        texts.extend(claim_texts(&node.title, node.body.as_deref()));
        let vectors = self.embedder.embed(&texts)?;
        self.with_history(|s| s.upsert_embeddings(&node.id, &vectors))
            .transpose()?;
        Ok(Some(node))
    }

    /// Write one edge into the history layer (`in`/`next`/`born-in` chains).
    /// `Ok(None)` when the layer is disabled or closed.
    pub fn add_history_edge(&self, e: NewEdge) -> Result<Option<Edge>> {
        self.with_history(|s| s.add_edge(e)).transpose()
    }

    /// Park a freshly-created curated note for born-in provenance (decision
    /// 00bgftf9usll): the harvester lags the live transcript, so the note's
    /// birth exchange resolves on a later tick via
    /// [`Engine::resolve_provenance`]. No-op when the history layer is off.
    pub fn park_provenance(&self, note_id: &str, ts: i64) {
        if !self.history_open() {
            return;
        }
        let Ok(mut lot) = self.provenance_lot.lock() else {
            return;
        };
        // Bounded: a graph nobody harvests must not grow a queue forever.
        if lot.len() >= 256 {
            lot.remove(0);
        }
        lot.push(ParkedProvenance {
            note_id: note_id.to_string(),
            ts,
            parked_at: crate::store::now(),
        });
    }

    /// Rebuild the parking lot after a restart: recent assistant-written
    /// notes that never got their born-in edge re-park from the store, so a
    /// daemon restart (deploys are routine here) costs no provenance. The
    /// in-memory lot stays the fast path; this is its recovery. Bounded by
    /// recency so pre-history notes never enter.
    pub fn repark_recent_provenance(&self, window_secs: i64) -> Result<usize> {
        if !self.history_open() {
            return Ok(0);
        }
        let now = crate::store::now();
        let already: std::collections::HashSet<String> = match self.provenance_lot.lock() {
            Ok(lot) => lot.iter().map(|p| p.note_id.clone()).collect(),
            Err(_) => return Ok(0),
        };
        let mut reparked = 0;
        for node in self.store.all_nodes()? {
            if node.source != crate::types::Source::Agent
                || node.valid_until.is_some()
                || now - node.created_at > window_secs
                || already.contains(&node.id)
                || self.born_in_of(&node.id).is_some()
            {
                continue;
            }
            self.park_provenance(&node.id, node.created_at);
            reparked += 1;
        }
        Ok(reparked)
    }

    /// Resolve parked notes to their birth exchange: the closest preceding
    /// assistant Message, preferring sessions still alive after the note's
    /// moment (concurrent sessions on one project can't steal provenance
    /// from a dead one). An entry resolves once ingestion has caught up past
    /// its timestamp, or best-effort after a grace period; either way it
    /// leaves the lot. Returns how many `born-in` edges were written.
    pub fn resolve_provenance(&self) -> Result<usize> {
        const GRACE_SECS: i64 = 900;
        {
            let Ok(lot) = self.provenance_lot.lock() else {
                return Ok(0);
            };
            if lot.is_empty() {
                return Ok(0);
            }
        }
        // One history scan amortized over every parked entry.
        let msgs: Vec<(String, i64, bool, Option<String>)> = self
            .with_history(|s| s.all_nodes())
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n.node_type.as_str() == crate::history::MESSAGE_TYPE)
            .map(|n| {
                let assistant = n
                    .props
                    .as_ref()
                    .and_then(|p| p.get("role"))
                    .and_then(|r| r.as_str())
                    == Some("assistant");
                (n.id, n.created_at, assistant, n.session_id)
            })
            .collect();
        let ingested_up_to = msgs.iter().map(|m| m.1).max().unwrap_or(i64::MIN);
        let now = crate::store::now();
        let Ok(mut lot) = self.provenance_lot.lock() else {
            return Ok(0);
        };
        let mut edges = Vec::new();
        lot.retain(|p| {
            if ingested_up_to < p.ts && now - p.parked_at <= GRACE_SECS {
                return true; // the transcript hasn't caught up — wait
            }
            let alive: std::collections::HashSet<&str> = msgs
                .iter()
                .filter(|m| m.1 >= p.ts)
                .filter_map(|m| m.3.as_deref())
                .collect();
            let best = msgs.iter().filter(|m| m.2 && m.1 <= p.ts).max_by_key(|m| {
                let in_alive = m.3.as_deref().is_some_and(|s| alive.contains(s));
                (in_alive, m.1, m.0.clone())
            });
            if let Some(m) = best {
                edges.push((p.note_id.clone(), m.0.clone()));
            }
            false // resolved or expired — either way, done
        });
        drop(lot);
        let n = edges.len();
        let ts = now;
        for (note, msg) in edges {
            // born-in is the one half-resident edge: it lives in the history
            // store but its `from` is a curated node. `add_edge` validates
            // endpoints, so it goes through the verbatim upsert path instead.
            let edge = crate::types::Edge {
                id: crate::id::new_id(),
                edge_type: crate::types::EdgeType::parse(crate::history::VERB_BORN_IN)?,
                from_id: note,
                to_id: msg,
                source: crate::types::Source::Agent,
                created_at: ts,
                confidence: None,
                strength: None,
                note: None,
                valid_from: Some(ts),
                valid_until: None,
                status: None,
            };
            self.with_history(|s| s.upsert_edge(&edge)).transpose()?;
        }
        Ok(n)
    }

    /// Search the history layer: hybrid since 0.9.0 — vector candidates
    /// fused with the blind-index BM25 channel (identical sealed or
    /// plaintext; before 0.9.0 no keyword index existed over history at
    /// all, caution 00bgftfbusll), then the cross-encoder re-scores the
    /// candidate texts at query time. Scores live on the reranker's scale
    /// but are NEVER blended with curated scores (the 0.8.1 register
    /// lesson) — the caller renders these as their own labeled section.
    /// Empty when the layer is off or `search_fallthrough` gates it.
    pub fn search_history(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::history::HistoryHit>> {
        self.search_history_filtered(query, limit, &SearchFilter::default())
    }

    /// [`Engine::search_history`] scoped to a time window and ordering
    /// (0.8.7) — the same grammar the curated layer reads, against message
    /// timestamps. Recorded dialogue is the layer where "when" is most often
    /// the whole question ("what did we try on Tuesday"), so the window
    /// filters candidates before the cross-encoder re-scores them and before
    /// the section gate decides whether the section exists at all.
    pub fn search_history_filtered(
        &self,
        query: &str,
        limit: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<crate::history::HistoryHit>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let window = filter.window;
        let qv = self.embedder.embed_one(query)?;
        let mut hits = self
            .with_history(|s| -> Result<Vec<crate::history::HistoryHit>> {
                let k = (limit * 8).clamp(16, 64)
                    * if window.is_open() {
                        1
                    } else {
                        self.store.config().policy.window_overfetch.max(1)
                    };
                let mut session_titles = std::collections::HashMap::new();
                for n in s.nodes_by_type_active(
                    &crate::types::NodeType::parse(crate::history::SESSION_TYPE)?,
                    usize::MAX,
                )? {
                    if let Some(sid) = &n.session_id {
                        session_titles.insert(sid.clone(), n.title);
                    }
                }
                let mk_hit = |n: &Node, score: f64| {
                    let p = |k: &str| n.props.as_ref().and_then(|m| m.get(k).cloned());
                    let session = n.session_id.clone().unwrap_or_default();
                    crate::history::HistoryHit {
                        message_id: n.id.clone(),
                        session_title: session_titles
                            .get(&session)
                            .map(|t| self.unseal_str(t))
                            .unwrap_or_default(),
                        session,
                        harness: p("harness").and_then(|v| v.as_str().map(str::to_string)),
                        role: p("role")
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_else(|| "assistant".into()),
                        turn: p("turn").and_then(|v| v.as_u64()),
                        timestamp: n.created_at,
                        snippet: crate::harvest::truncate_words(
                            &self.unseal_str(n.body.as_deref().unwrap_or(&n.title)),
                            240,
                        ),
                        score,
                        prior: Vec::new(),
                    }
                };
                // The keyword channel (0.9.0): history finally has BM25 —
                // the same blind index the curated store runs, identical
                // sealed or plaintext. Fused with search_hybrid's weights so
                // both layers rank by one rulebook; scores still NEVER
                // blend across layers.
                let pol = self.store.config().policy.clone();
                let kw_hits = s.search_fts(query, &[], k)?;
                let kw_max = kw_hits.iter().map(|h| h.score).fold(0.0f64, f64::max);
                let kw_norm: std::collections::HashMap<String, f64> = kw_hits
                    .iter()
                    .filter(|_| kw_max > 0.0)
                    .map(|h| (h.id.clone(), h.score / kw_max))
                    .collect();
                let sem_scaled = |sem: f64| {
                    ((sem - pol.semantic_floor) / (1.0 - pol.semantic_floor)).clamp(0.0, 1.0)
                };
                let mut out = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for (id, dist) in s.search_vec(&qv, k)? {
                    // Claim chunks share the node id — first (closest) wins.
                    if !seen.insert(id.clone()) {
                        continue;
                    }
                    let Some(n) = s.get_node(&id)? else { continue };
                    if n.node_type.as_str() != crate::history::MESSAGE_TYPE {
                        continue;
                    }
                    // A message's created_at IS when it was said — the one
                    // place in this system where the capture clock and the
                    // event clock are the same clock.
                    if !window.contains(n.created_at) {
                        continue;
                    }
                    let kw = kw_norm.get(&id).copied().unwrap_or(0.0);
                    let sem = (1.0 - dist).clamp(0.0, 1.0);
                    let score =
                        pol.keyword_weight * kw + (1.0 - pol.keyword_weight) * sem_scaled(sem);
                    out.push(mk_hit(&n, score));
                }
                // Keyword-only candidates: matched terms but slipped the
                // vector net — exactly the class the old vector-only path
                // could never see.
                for h in &kw_hits {
                    if !seen.insert(h.id.clone()) {
                        continue;
                    }
                    let Some(n) = s.get_node(&h.id)? else {
                        continue;
                    };
                    if n.node_type.as_str() != crate::history::MESSAGE_TYPE
                        || !window.contains(n.created_at)
                    {
                        continue;
                    }
                    let kw = kw_norm.get(&h.id).copied().unwrap_or(0.0);
                    out.push(mk_hit(&n, pol.keyword_weight * kw));
                }
                Ok(out)
            })
            .transpose()?
            .unwrap_or_default();
        if let Some(reranker) = &self.reranker
            && !hits.is_empty()
        {
            // Re-score over the FULL text (re-read per candidate — the same
            // shape the encrypted store needs later: decrypt N candidates in
            // memory at query time, nothing persistent).
            let docs: Vec<String> = hits
                .iter()
                .map(|h| {
                    self.with_history(|s| {
                        s.get_node(&h.message_id)
                            .ok()
                            .flatten()
                            .and_then(|n| n.body)
                            .map(|b| self.unseal_str(&b))
                            .unwrap_or_else(|| h.snippet.clone())
                    })
                    .unwrap_or_else(|| h.snippet.clone())
                })
                .collect();
            if let Ok(logits) = reranker.rank(query, &docs) {
                for (h, l) in hits.iter_mut().zip(logits) {
                    h.score = 1.0 / (1.0 + (-f64::from(l)).exp());
                }
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        // The same session-diverse cut the curated path runs: dialogue is
        // where one session's restatements crowd hardest (an empty session
        // string means "unknown" and is never demoted).
        session_diverse_select(
            &mut hits,
            limit,
            self.store.config().policy.session_diversity_demote,
            |h| Some(h.session.as_str()).filter(|s| !s.is_empty()),
        );
        // Section gate, not per-hit trim (measured 2026-08-11): the per-hit
        // delivery floor halved false-fall-through noise but cost 0.03
        // end-to-end and 0.09 oblique dialogue recall — the 0.8.1 register
        // lesson (0.22 was calibrated on curated notes, not chat). Gating on
        // the TOP hit instead makes an all-noise section vanish while a
        // section with any real match keeps its full candidate list — low-
        // ranked gold survives.
        if self.reranker.is_some()
            && hits
                .first()
                .is_none_or(|top| top.score < self.store.config().policy.delivery_floor)
        {
            hits.clear();
        }
        // Ordering is the last word, after the gate — so a time-ordered read
        // is the same SET the relevance-ordered read would have delivered.
        order_history_hits(&mut hits, filter.order);
        if filter.order == crate::timespec::SearchOrder::Recent {
            hits = self.collapse_restatements(hits)?;
        }
        Ok(hits)
    }

    /// Fold restatements under their newest form (0.8.7), for `order:
    /// "recent"` only.
    ///
    /// A transcript has no `replaces` edges — nobody curates dialogue — so the
    /// supersession chain the curated graph gets from judgment has to be
    /// inferred here from the text itself. Hits arrive newest-first, so the
    /// first statement of a thing IS its latest statement; every later
    /// near-duplicate becomes one of its `prior` generations.
    ///
    /// Three things keep this honest. It runs ONLY under the recency ordering,
    /// so the default path is untouched and owes no receipt. It never removes
    /// a hit — folding is nesting, so recall is unchanged BY CONSTRUCTION and
    /// a mistuned threshold costs shape, not answers. And the threshold is a
    /// per-graph knob (`history.recency_collapse`), because this project has
    /// already learned once that absolute score thresholds don't transfer
    /// between registers.
    fn collapse_restatements(
        &self,
        hits: Vec<crate::history::HistoryHit>,
    ) -> Result<Vec<crate::history::HistoryHit>> {
        let threshold = self.store.config().history.recency_collapse;
        if hits.len() < 2 || !(0.0..1.0).contains(&threshold) {
            return Ok(hits);
        }
        let texts: Vec<String> = hits.iter().map(|h| h.snippet.clone()).collect();
        let vecs = self.embedder.embed(&texts)?;
        if vecs.len() != hits.len() {
            return Ok(hits);
        }
        let mut heads: Vec<(usize, crate::history::HistoryHit)> = Vec::new();
        for (i, hit) in hits.into_iter().enumerate() {
            let head = heads
                .iter_mut()
                .find(|(j, _)| cosine(&vecs[*j], &vecs[i]) >= threshold);
            match head {
                Some((_, head)) => head.prior.push(hit),
                None => heads.push((i, hit)),
            }
        }
        Ok(heads.into_iter().map(|(_, h)| h).collect())
    }

    /// Every message of one session, decrypted in memory and in
    /// conversation order — the pane's history view reads whole sessions.
    pub fn history_messages(
        &self,
        session: &str,
    ) -> Result<Vec<crate::history::HistoryMessageView>> {
        let msgs = self
            // Indexed (0.10.0): only this conversation's nodes are read and
            // unsealed, not the whole recording.
            .with_history(|s| -> Result<Vec<crate::types::Node>> {
                Ok(s.nodes_in_session(session)?
                    .into_iter()
                    .filter(|n| n.node_type.as_str() == crate::history::MESSAGE_TYPE)
                    .collect())
            })
            .transpose()?
            .unwrap_or_default();
        let mut views: Vec<crate::history::HistoryMessageView> = msgs
            .into_iter()
            .map(|n| {
                let p = |k: &str| n.props.as_ref().and_then(|m| m.get(k).cloned());
                crate::history::HistoryMessageView {
                    role: p("role")
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| "assistant".into()),
                    turn: p("turn").and_then(|v| v.as_u64()),
                    timestamp: n.created_at,
                    text: self.unseal_str(&n.body.unwrap_or_default()),
                    message_id: n.id,
                }
            })
            .collect();
        views.sort_by_key(|v| (v.turn.unwrap_or(u64::MAX), v.timestamp));
        Ok(views)
    }

    /// The exchange around one turn of a session, decrypted in memory —
    /// `expand_history(session, turn, window)`. The model decides how much
    /// raw dialogue to spend context on; nothing is auto-injected.
    pub fn expand_history(
        &self,
        session: &str,
        turn: u64,
        window: u64,
    ) -> Result<Vec<crate::history::HistoryMessageView>> {
        let views = self.history_messages(session)?;
        if views.is_empty() {
            return Ok(views);
        }
        let center = views
            .iter()
            .position(|v| v.turn == Some(turn))
            .unwrap_or_else(|| {
                // Nearest by turn when the exact one was filtered/absent.
                views
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.turn.unwrap_or(u64::MAX).abs_diff(turn))
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        let window = (window as usize).min(views.len());
        let lo = center.saturating_sub(window);
        let hi = (center + window + 1).min(views.len());
        Ok(views[lo..hi].to_vec())
    }

    /// Every recorded session, newest lane first — the history browser's
    /// spine.
    /// Sessions overlapping a time window, newest lane first (0.8.7) — the
    /// "what was I doing last Tuesday" entry point, and the surface that makes
    /// a temporal question answerable WITHOUT a search hit to start from.
    ///
    /// A session counts as inside the window when any part of it is: a session
    /// that began before the window and ran into it is exactly the one a
    /// person asking about that window means.
    pub fn list_history_sessions_in(
        &self,
        window: crate::timespec::TimeWindow,
    ) -> Result<Vec<crate::history::HistorySessionView>> {
        let mut out = self.list_history_sessions()?;
        if !window.is_open() {
            out.retain(|s| {
                let ended = s.ended.unwrap_or(s.started);
                window.after.is_none_or(|a| ended >= a)
                    && window.before.is_none_or(|b| s.started < b)
            });
        }
        Ok(out)
    }

    pub fn list_history_sessions(&self) -> Result<Vec<crate::history::HistorySessionView>> {
        let mut out: Vec<crate::history::HistorySessionView> = self
            // Indexed by type (0.10.0): the Session nodes alone, not every
            // recorded message.
            .with_history(|s| -> Result<Vec<crate::types::Node>> {
                Ok(s.nodes_of_type(crate::history::SESSION_TYPE)?
                    .into_iter()
                    .filter(|n| n.valid_until.is_none())
                    .collect())
            })
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|n| {
                let p = |k: &str| n.props.as_ref().and_then(|m| m.get(k).cloned());
                crate::history::HistorySessionView {
                    session: n.session_id.clone().unwrap_or_default(),
                    title: self.unseal_str(&n.title),
                    harness: p("harness").and_then(|v| v.as_str().map(str::to_string)),
                    started: n.created_at,
                    ended: p("ended").and_then(|v| v.as_i64()),
                    messages: p("messages").and_then(|v| v.as_u64()).unwrap_or(0),
                    version: n.version,
                    node_id: n.id,
                }
            })
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.started));
        Ok(out)
    }

    /// Per-session hard delete (pane-only, like every hard delete): the
    /// Session node, its messages, and their edges leave the history store —
    /// and the session's transcript path joins `history.exclude_paths`, or
    /// the harvester would quietly resurrect it from the file that still
    /// sits on disk. The exclusion is visible (and reversible) in settings.
    pub fn delete_history_session(&self, session: &str) -> Result<usize> {
        let (removed, transcript) = self
            .with_history(|s| -> Result<(usize, Option<String>)> {
                let nodes: Vec<Node> = s.nodes_in_session(session)?;
                let transcript = nodes
                    .iter()
                    .find(|n| n.node_type.as_str() == crate::history::SESSION_TYPE)
                    .and_then(|n| n.props.as_ref())
                    .and_then(|p| p.get("path"))
                    .and_then(|v| v.as_str().map(str::to_string));
                for n in &nodes {
                    s.delete_node(&n.id)?;
                }
                Ok((nodes.len(), transcript))
            })
            .transpose()?
            .unwrap_or((0, None));
        if let Some(path) = transcript {
            let mut cfg = (*self.store.config()).clone();
            if !cfg.history.exclude_paths.contains(&path) {
                cfg.history.exclude_paths.push(path);
                self.set_graph_config(&cfg)?;
            }
        }
        if removed > 0 {
            self.audit(
                "history_session_deleted",
                "graph",
                session,
                Some(format!("{removed} recorded node(s) removed")),
                None,
                None,
                None,
            )?;
        }
        Ok(removed)
    }

    /// Counts for the pane's History settings (None = layer closed).
    pub fn history_stats(&self) -> Option<crate::StoreStats> {
        self.with_history(|s| s.stats().ok()).flatten()
    }

    /// Wholesale history delete — the user-only escape hatch the sibling
    /// store exists for. Drops the handle, removes the file, and (when the
    /// layer is enabled) reopens fresh with a re-seeded ontology. The caller
    /// (HTTP layer) also bumps the hub's history epoch so the running
    /// harvester forgets its cursors and caches.
    pub fn reset_history(&self) -> Result<()> {
        let Some(path) = self.history_path.clone() else {
            return Ok(());
        };
        {
            let Ok(mut slot) = self.history.lock() else {
                return Err(crate::Error::Io("history handle poisoned".into()));
            };
            *slot = None; // close before deleting — redb is single-handle
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| crate::Error::Io(format!("removing {}: {e}", path.display())))?;
            }
        }
        self.sync_history_store();
        self.audit(
            "history_reset",
            "graph",
            "",
            Some("history layer wiped by the user".into()),
            None,
            None,
            None,
        )?;
        Ok(())
    }

    /// Where a curated node was born, when its `born-in` edge exists — the
    /// provenance line on search hits and the pane's history chip.
    pub fn born_in_of(&self, node_id: &str) -> Option<crate::history::BornIn> {
        self.with_history(|s| {
            let edge = s
                .edges_out(node_id)
                .ok()?
                .into_iter()
                .find(|e| e.edge_type.as_str() == crate::history::VERB_BORN_IN)?;
            let msg = s.get_node(&edge.to_id).ok()??;
            Some(crate::history::BornIn {
                session: msg.session_id.clone().unwrap_or_default(),
                turn: msg
                    .props
                    .as_ref()
                    .and_then(|p| p.get("turn"))
                    .and_then(|v| v.as_u64()),
                timestamp: msg.created_at,
                message_id: msg.id,
            })
        })
        .flatten()
    }

    /// Every curated note born inside one recorded session, in turn order —
    /// the reverse of [`Self::born_in_of`]: what a conversation left in
    /// memory. The `born-in` edges live in the history store (half-resident,
    /// curated `from`), so the scan walks the session's messages there and
    /// resolves the notes against the curated store.
    pub fn session_notes(&self, session: &str) -> Result<Vec<crate::history::SessionNote>> {
        // (note id, turn, message id, message ts) per born-in edge.
        type Born = Vec<(String, Option<u64>, String, i64)>;
        let born: Born = self
            .with_history(|s| -> Result<Born> {
                let mut out = Vec::new();
                for n in s.nodes_in_session(session)? {
                    if n.node_type.as_str() != crate::history::MESSAGE_TYPE {
                        continue;
                    }
                    let turn = n
                        .props
                        .as_ref()
                        .and_then(|p| p.get("turn"))
                        .and_then(|v| v.as_u64());
                    for e in s.edges_in(&n.id)? {
                        if e.edge_type.as_str() == crate::history::VERB_BORN_IN {
                            out.push((e.from_id.clone(), turn, n.id.clone(), n.created_at));
                        }
                    }
                }
                Ok(out)
            })
            .transpose()?
            .unwrap_or_default();
        let mut notes = Vec::new();
        for (note_id, turn, message_id, timestamp) in born {
            let Some(node) = self.store.get_node(&note_id)? else {
                continue; // the note was hard-deleted; its edge is a ghost
            };
            notes.push(crate::history::SessionNote {
                id: node.id,
                node_type: node.node_type.as_str().to_string(),
                title: node.title,
                turn,
                message_id,
                timestamp,
            });
        }
        notes.sort_by_key(|n| (n.turn.unwrap_or(u64::MAX), n.timestamp));
        Ok(notes)
    }

    /// Replace a history node in place (the harvester's cursor bumps and
    /// title upgrades live in `Session.props`). A legacy-sealed field
    /// arriving from an old read path is opened first; storage-level
    /// sealing is the driver's job. Re-embeds only when asked, always from
    /// plaintext. Returns whether the layer was open to take the write.
    pub fn upsert_history_node(&self, node: &Node, re_embed: bool) -> Result<bool> {
        let mut stored = node.clone();
        stored.title = self.unseal_str(&stored.title);
        stored.body = stored.body.as_deref().map(|b| self.unseal_str(b));
        let wrote = self
            .with_history(|s| {
                s.upsert_node(&stored)?;
                Ok::<_, crate::Error>(())
            })
            .transpose()?;
        if wrote.is_some() && re_embed {
            let mut texts = vec![embed_text(
                &stored.title,
                stored.body.as_deref(),
                &stored.tags,
                &stored.code_refs,
            )];
            texts.extend(claim_texts(&stored.title, stored.body.as_deref()));
            let vectors = self.embedder.embed(&texts)?;
            self.with_history(|s| s.upsert_embeddings(&stored.id, &vectors))
                .transpose()?;
        }
        Ok(wrote.is_some())
    }

    /// Reconcile the open handle with `config().history.enabled` — runs on
    /// path install and after every config write, so the pane's toggle takes
    /// effect without a daemon restart. Disabling drops the handle; the file
    /// stays on disk (delete is a user gesture). A freshly-opened store is
    /// also reconciled with the daemon's desired encryption (0.9.0), which
    /// seals pre-0.9 plaintext/mixed history stores on first open.
    fn sync_history_store(&self) {
        let Ok(mut slot) = self.history.lock() else {
            return;
        };
        let enabled = self.store.config().history.enabled;
        if enabled && slot.is_none() {
            if let Some(path) = &self.history_path {
                match crate::history::open_history_store(path) {
                    Ok(s) => {
                        if let Err(e) =
                            reconcile_encryption(&*s, self.desired_history_enc, &mut |_, _| {})
                        {
                            eprintln!("engram: history encryption reconcile failed: {e}");
                        }
                        *slot = Some(s);
                    }
                    Err(e) => {
                        eprintln!(
                            "engram: couldn't open history store {}: {e}",
                            path.display()
                        )
                    }
                }
            }
        } else if !enabled && slot.is_some() {
            *slot = None;
        }
    }

    /// Reconcile the CURATED store with the daemon's desired encryption
    /// (0.9.0) — run at open, after `ensure_*`. Returns rows reprocessed.
    pub fn ensure_encryption(&self) -> Result<usize> {
        reconcile_encryption(self.store.as_ref(), self.desired_graph_enc, &mut |_, _| {})
    }

    /// The recorded at-rest states — `(curated, history)`; history is `None`
    /// while the layer is closed. For `/encryption` and doctor.
    pub fn encryption_states(
        &self,
    ) -> (
        crate::seal::EncryptionState,
        Option<crate::seal::EncryptionState>,
    ) {
        (
            self.store.encryption_state(),
            self.with_history(|s| s.encryption_state()),
        )
    }

    /// Encrypt or decrypt a whole store NOW — the pane's two switches land
    /// here (`target`: "graph" | "history"). Runs the full rewrite with
    /// `progress(done, total)` callbacks, journals the change, and returns
    /// rows reprocessed. The store's recorded state is the source of truth
    /// throughout: killed anywhere, the next reconcile finishes the job.
    pub fn set_store_encryption(
        &self,
        target: &str,
        enable: bool,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<usize> {
        let n = match target {
            "graph" => reconcile_encryption(self.store.as_ref(), Some(enable), progress)?,
            "history" => self
                .with_history(|s| reconcile_encryption(s, Some(enable), progress))
                .transpose()?
                .ok_or_else(|| {
                    crate::Error::Config(
                        "the history layer is not open — enable session recording first".into(),
                    )
                })?,
            other => {
                return Err(crate::Error::Config(format!(
                    "unknown encryption target {other:?} — graph | history"
                )));
            }
        };
        self.audit(
            "encryption_changed",
            "graph",
            "",
            Some(format!(
                "{target} → {} ({n} rows reprocessed)",
                if enable { "sealed" } else { "plaintext" }
            )),
            None,
            None,
            None,
        )?;
        self.notify(ChangeEvent::ConfigChanged);
        Ok(n)
    }

    /// Path-shaped code_refs that don't resolve against the repo root right
    /// now — the write-time half of the drift check, so the writer learns in
    /// the same turn instead of at the next drift scan.
    fn missing_refs(&self, refs: &[String]) -> Vec<String> {
        let Some(root) = &self.repo_root else {
            return Vec::new();
        };
        refs.iter()
            .filter(|r| ref_is_path(r) && !root.join(r.as_str()).exists())
            .cloned()
            .collect()
    }

    /// Stamp who the following writes belong to. Front-ends sharing this
    /// engine call it under their mutex lock before every operation.
    pub fn set_audit_origin(&mut self, origin: AuditOrigin) {
        self.audit_origin = origin;
    }

    /// Journal a session-level activity event (mcp_session_started /
    /// mcp_session_ended / brief_served, …): AI activity around the graph,
    /// not just mutations of it — so a session's whole arc is retrievable
    /// later. `entity_id` is the acting session, making
    /// `audit(entity_id = session)` page one session's lifecycle directly.
    pub fn audit_activity(&self, action: &str, note: Option<String>) -> Result<()> {
        let session = self.audit_origin.session_id.clone().unwrap_or_default();
        self.audit(action, "session", &session, note, None, None, None)
    }

    /// The graph's current working version (version tracking, 0.7.0).
    pub fn current_version(&self) -> Result<Option<String>> {
        self.store.current_version()
    }

    /// Set (or clear) the current working version — the version every new
    /// node of a version-bound type is stamped with while tracking is on.
    /// Journaled under entity_id "version", so `audit(entity_id="version")`
    /// pages the switch history directly.
    pub fn set_current_version(&self, version: Option<&str>) -> Result<Option<String>> {
        if let Some(v) = version
            && (v.trim().is_empty() || v.len() > 32)
        {
            return Err(crate::Error::Config(
                "version must be 1..=32 non-blank characters".into(),
            ));
        }
        let previous = self.store.current_version()?;
        // Setting a version IS the "I want version tracking" gesture: when
        // tracking is off the stamp would be silently ignored, so enable it
        // here rather than confuse a caller who was asked to set the
        // version. Clearing never toggles anything.
        if version.is_some() && !self.store.config().versioning.enabled {
            let mut cfg = self.graph_config();
            cfg.versioning.enabled = true;
            self.set_graph_config(&cfg)?;
        }
        self.store.set_current_version(version)?;
        self.audit(
            "version_switched",
            "graph",
            "version",
            Some(format!(
                "{} → {}",
                previous.as_deref().unwrap_or("(unset)"),
                version.unwrap_or("(unset)")
            )),
            None,
            None,
            None,
        )?;
        self.notify(ChangeEvent::ConfigChanged);
        // Version rules on tasks ("close once past 0.9.12") fire now.
        self.settle_tasks()?;
        Ok(previous)
    }

    /// One full graph-health pass — the session-boundary validation: the
    /// decay pass archives what has expired, the conflict scan queues fresh
    /// look-alike pairs, and the drift scan counts unresolved code_refs, so
    /// a session starts (and leaves) with the graph prepared rather than
    /// waiting for the six-hourly sweep. Journaled as a `graph_validated`
    /// activity row; returns the summary note.
    pub fn validate_graph(&self) -> Result<String> {
        let prelude = self.validate_prelude()?;
        let suspects = self.scan_conflicts()?;
        self.validate_finish(prelude, suspects)
    }

    /// [`Engine::validate_graph`] for an engine shared behind a mutex — the
    /// shape every long-running caller has (the session-boundary pass, the
    /// core's six-hourly sweep). The cheap parts run under one lock; the
    /// conflict scan — the part whose cost grows with the graph and the
    /// logic model (~60 s at 580 notes with Laya) — releases the lock after
    /// every node, so tool calls and pane requests interleave instead of
    /// queueing behind the sweep. Holding it for the whole scan starved the
    /// core's async workers until even `/health` went silent and every
    /// bridge exited with "the engram core went away" (found 2026-09-24).
    pub fn validate_graph_shared(engine: &std::sync::Mutex<Engine>) -> Result<String> {
        let prelude = lock_engine(engine)?.validate_prelude()?;
        let suspects = Engine::scan_conflicts_shared(engine)?;
        lock_engine(engine)?.validate_finish(prelude, suspects)
    }

    /// [`Engine::scan_conflicts`] one node per lock — see
    /// [`Engine::validate_graph_shared`]. A node archived or deleted between
    /// two steps is skipped; a node written meanwhile was already scanned by
    /// its own write.
    pub fn scan_conflicts_shared(engine: &std::sync::Mutex<Engine>) -> Result<usize> {
        let ids = lock_engine(engine)?.scan_ids()?;
        let mut added = 0;
        for id in ids {
            added += lock_engine(engine)?.scan_one(&id)?;
            // Releasing is not enough: std's Mutex is not fair, and a loop
            // that unlocks and relocks at once wins every race — a waiting
            // caller measured 12 s behind a 12.5 s sweep. A millisecond
            // off the lock per node hands it over (~0.6 s at 580 notes).
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if added > 0 {
            lock_engine(engine)?.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(added)
    }

    /// Decay, supersession healing and calibration — the part of a
    /// validation pass that runs before the conflict scan.
    fn validate_prelude(&self) -> Result<(usize, usize, Option<String>)> {
        self.last_validated
            .store(crate::store::now(), std::sync::atomic::Ordering::Relaxed);
        let ttl = self.store.config().policy.decay_ttl_days;
        let archived = self.decay(ttl, false)?.len();
        let retired = self.retire_superseded_sweep()?;
        // Calibrate before scanning, so this session's conflict pass already
        // runs under the floor the graph's own judgments support.
        let tuned = self.auto_tune()?;
        Ok((archived, retired, tuned))
    }

    /// Drift count and the journaled summary — the part after the scan.
    fn validate_finish(
        &self,
        (archived, retired, tuned): (usize, usize, Option<String>),
        suspects: usize,
    ) -> Result<String> {
        let drift = match self.repo_root().map(std::path::Path::to_path_buf) {
            Some(root) => self.scan_code_refs(&root)?.len(),
            None => 0,
        };
        let note = format!(
            "{archived} decayed, {retired} superseded, {suspects} new suspect{}, {drift} drifted ref{}{}",
            if suspects == 1 { "" } else { "s" },
            if drift == 1 { "" } else { "s" },
            match &tuned {
                Some(t) => format!(", {t}"),
                None => String::new(),
            },
        );
        self.audit_activity("graph_validated", Some(note.clone()))?;
        Ok(note)
    }

    /// Whether a fresh [`Engine::validate_graph`] run is due — false within
    /// `min_interval_secs` of the last one on THIS graph.
    pub fn validation_due(&self, min_interval_secs: i64) -> bool {
        let last = self
            .last_validated
            .load(std::sync::atomic::Ordering::Relaxed);
        crate::store::now() - last >= min_interval_secs
    }

    /// Per-graph calibration — one button, several dials (user decision
    /// 2026-08-03: `policy.auto_tune` stays the single switch as calibration
    /// grows parameters). Each dial fits one policy value from evidence this
    /// graph itself produced, applies only a move of at least
    /// [`policy::AUTO_TUNE_MIN_DELTA`], and every move lands in one
    /// `auto_tuned` activity row. `policy.auto_tune = false` opts a graph
    /// out entirely.
    ///
    /// Dial one, the conflict floor — the automated form of the manual
    /// 0.85→0.88 retune (2026-07-13). Every resolved suspect is a labeled
    /// (similarity, verdict) pair: a dismissal is a false positive the floor
    /// exists to prevent, a confirmation is what it must never lose. Models
    /// nominate, people judge — and what people judged is calibration data,
    /// so this fits FROM human verdicts only. Engages over
    /// [`policy::AUTO_TUNE_MIN_NOTES`] current notes with at least
    /// [`policy::AUTO_TUNE_MIN_JUDGED`] judgments,
    /// [`policy::AUTO_TUNE_MIN_EACH`] per side.
    ///
    /// Dial two, the weak line — fits `weak_evidence_top` as a quantile of
    /// the top scores phantom probes (questions about invented subjects
    /// guaranteed absent from any graph) still reach here. The fixed 0.85
    /// default is only right near 2000 notes; the measured calibrated line
    /// runs 0.56→0.81 from 100 to 2000 (tricks bench, 2026-08-03). Engages
    /// over [`policy::WEAK_LINE_MIN_NOTES`] notes, reranker-gated: the line
    /// is read against the cross-encoder's calibrated scale, so there is
    /// nothing to fit without one.
    pub fn auto_tune(&self) -> Result<Option<String>> {
        use crate::policy::{AUTO_TUNE_MIN_NOTES, WEAK_LINE_MIN_NOTES};
        let mut cfg = self.graph_config();
        if !cfg.policy.auto_tune {
            return Ok(None);
        }
        let notes = self.store.stats()?.nodes;
        let mut moves = Vec::new();
        if notes > AUTO_TUNE_MIN_NOTES
            && let Some(m) = self.fit_conflict_floor(&mut cfg)?
        {
            moves.push(m);
        }
        if notes > WEAK_LINE_MIN_NOTES
            && let Some(m) = self.fit_weak_line(&mut cfg)?
        {
            moves.push(m);
        }
        if moves.is_empty() {
            return Ok(None);
        }
        self.set_graph_config(&cfg)?;
        let note = moves.join("; ");
        self.audit_activity("auto_tuned", Some(note.clone()))?;
        Ok(Some(note))
    }

    /// Auto-tune dial one: fit the conflict-suspect floor from judged
    /// history by maximizing balanced accuracy over midpoint candidates,
    /// preferring the higher threshold on ties (a quieter queue). The move
    /// is damped ([`policy::AUTO_TUNE_DAMPING`]) and the damped target is
    /// clamped into [[`policy::AUTO_TUNE_FLOOR_MIN`], `duplicate_similarity`).
    /// Mutates `cfg` and returns the journal fragment; the caller persists.
    fn fit_conflict_floor(&self, cfg: &mut crate::config::GraphConfig) -> Result<Option<String>> {
        use crate::policy::{
            AUTO_TUNE_FLOOR_MIN, AUTO_TUNE_MIN_DELTA, AUTO_TUNE_MIN_EACH, AUTO_TUNE_MIN_JUDGED,
        };
        let judged: Vec<(f64, bool)> = self
            .store
            .all_suspects()?
            .into_iter()
            // Only pairs the SIMILARITY path could have raised inform the
            // similarity dial: the NLI path (0.9.4) nominates below the
            // floor, and letting those verdicts into the fit would drag the
            // floor toward the NLI band it never reads.
            .filter(|s| s.similarity >= AUTO_TUNE_FLOOR_MIN)
            .filter_map(|s| match s.status {
                SuspectStatus::Confirmed => Some((s.similarity, true)),
                SuspectStatus::Dismissed => Some((s.similarity, false)),
                SuspectStatus::Suspected => None,
            })
            .collect();
        let confirmed = judged.iter().filter(|(_, c)| *c).count();
        let dismissed = judged.len() - confirmed;
        if judged.len() < AUTO_TUNE_MIN_JUDGED
            || confirmed < AUTO_TUNE_MIN_EACH
            || dismissed < AUTO_TUNE_MIN_EACH
        {
            return Ok(None);
        }

        let mut sims: Vec<f64> = judged.iter().map(|(s, _)| *s).collect();
        sims.sort_by(f64::total_cmp);
        sims.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        let old = cfg.policy.conflict_suspect_similarity;
        let mut best = (old, f64::NEG_INFINITY);
        for w in sims.windows(2) {
            let t = (w[0] + w[1]) / 2.0;
            let kept = judged.iter().filter(|(s, c)| *c && *s >= t).count() as f64;
            let silenced = judged.iter().filter(|(s, c)| !*c && *s < t).count() as f64;
            let balanced = (kept / confirmed as f64 + silenced / dismissed as f64) / 2.0;
            if balanced > best.1 + 1e-9 || (balanced > best.1 - 1e-9 && t > best.0) {
                best = (t, balanced);
            }
        }
        let fitted = best.0;
        // Damped, then hard-clamped: the dial travels half the distance to
        // the fit per pass (policy::AUTO_TUNE_DAMPING), and the clamp reads
        // the DAMPED value — a glitched fit, or a corrupt stored dial, lands
        // back inside the band on the very next pass.
        let (lo, hi) = (AUTO_TUNE_FLOOR_MIN, cfg.policy.duplicate_similarity - 0.001);
        let anchor = if old.is_finite() {
            old.clamp(lo, hi)
        } else {
            hi
        };
        let target = if fitted.is_finite() {
            (anchor + (fitted - anchor) * crate::policy::AUTO_TUNE_DAMPING).clamp(lo, hi)
        } else {
            anchor
        };
        if (target - old).abs() < AUTO_TUNE_MIN_DELTA {
            return Ok(None);
        }

        cfg.policy.conflict_suspect_similarity = target;
        Ok(Some(format!(
            "conflict floor {old:.3} -> {target:.3} (fit {fitted:.3}, damped) from {} judged pairs ({confirmed} confirmed / {dismissed} dismissed)",
            judged.len()
        )))
    }

    /// Auto-tune dial two: fit the weak-evidence line as
    /// `policy.weak_line_quantile` of the top scores that
    /// `policy.weak_line_probes` phantom probes reach on this graph — the
    /// split-conformal question "how high can a score climb here when the
    /// answer does not exist". Probes never stamp `last_seen` (calibration
    /// is not retrieval). The move is damped ([`policy::AUTO_TUNE_DAMPING`])
    /// and the damped target clamps into the floor-relative band up to
    /// [`policy::WEAK_LINE_MAX`]. Mutates `cfg` and returns the journal
    /// fragment; the caller persists.
    fn fit_weak_line(&self, cfg: &mut crate::config::GraphConfig) -> Result<Option<String>> {
        use crate::policy::{AUTO_TUNE_MIN_DELTA, WEAK_LINE_ABOVE_FLOOR, WEAK_LINE_MAX};
        let Some(reranker) = &self.reranker else {
            return Ok(None);
        };
        // Probes borrow vocabulary from the graph's own titles: a probe that
        // asks in generic wording under-reads how high THIS graph's scores
        // can climb for a subject that does not exist (first live fit,
        // 2026-08-04: in-register questions reached 0.32 against a 0.25 line
        // fitted from generic probes). The coined subject keeps the question
        // unanswerable; the borrowed words keep it in register.
        let mut notes = self.store.all_nodes()?;
        notes.retain(|n| n.valid_until.is_none());
        // Content order, not id order (0.9.10): ids are minted from the
        // clock, so the same corpus written twice sampled different
        // transplants and fitted a different line every run (eval Finding
        // 00dvpn0t6gx6). A content hash is fixed for a given graph and still
        // spreads the sample across it.
        notes.sort_by_cached_key(|n| (content_hash(&n.title, n.body.as_deref()), n.id.clone()));
        let vocab: Vec<String> = notes.iter().filter_map(|n| probe_terms(&n.title)).collect();
        // Two probe families, each answering "how high can a score climb
        // here when the answer does not exist" from a different angle:
        // question-shaped templates over borrowed vocabulary, and ICT
        // transplants — real sentences with their subjects coined out. The
        // fit is the max of the per-family quantiles: the line must clear
        // whichever register this graph's noise speaks loudest in.
        let total = cfg.policy.weak_line_probes;
        let top_of = |probe: &str, source: Option<&str>| -> Result<f64> {
            let qv = self.embedder.embed_one(probe)?;
            // Probes calibrate against the WHOLE graph — a windowed noise
            // sample would fit a line to a slice of history.
            let mut hits = self.store.search_hybrid(
                probe,
                Some(&qv),
                &[],
                12,
                Default::default(),
                &Default::default(),
            )?;
            // A transplant is asked of the graph WITHOUT the note it was cut
            // from (0.9.11, engram Problem 00e353wiopga). Swapping two words
            // makes a short generated fact unanswerable, but a long real
            // sentence keeps ~30 words pointing straight at its source: on
            // this repo's graph two probes reached 1.04 and 1.01 against
            // their own notes, pinned the line at its 0.95 ceiling and made
            // every search read "weak". Held out, the source can no longer
            // answer, and what the probe still reaches is the noise the line
            // exists to clear.
            if let Some(source) = source {
                hits.retain(|h| h.id != source);
            }
            if hits.is_empty() {
                return Ok(0.0);
            }
            self.rerank(reranker.as_ref(), probe, &mut hits);
            Ok(hits.iter().map(|h| h.score).fold(f64::MIN, f64::max))
        };
        let mut template_tops = Vec::with_capacity(total.div_ceil(2));
        let mut transplant_tops = Vec::with_capacity(total / 2);
        for i in 0..total {
            if i % 2 == 0 {
                let terms = (!vocab.is_empty()).then(|| vocab[(i / 2) % vocab.len()].as_str());
                template_tops.push(top_of(&phantom_probe(i / 2, terms), None)?);
            } else {
                let n = &notes[(i / 2) * 17 % notes.len()];
                transplant_tops.push(top_of(
                    &transplant_probe(i / 2, &n.title, n.body.as_deref()),
                    Some(&n.id),
                )?);
            }
        }
        template_tops.sort_by(f64::total_cmp);
        transplant_tops.sort_by(f64::total_cmp);
        let q = cfg.policy.weak_line_quantile;
        let fitted = quantile(&template_tops, q).max(quantile(&transplant_tops, q));
        if std::env::var_os("ENGRAM_WEAK_LINE_DEBUG").is_some() {
            eprintln!(
                "weak line fit: template q{:.0} {:.3} {:?}\n               transplant q{:.0} {:.3} {:?}",
                q * 100.0,
                quantile(&template_tops, q),
                template_tops
                    .iter()
                    .map(|x| (x * 100.0).round() / 100.0)
                    .collect::<Vec<_>>(),
                q * 100.0,
                quantile(&transplant_tops, q),
                transplant_tops
                    .iter()
                    .map(|x| (x * 100.0).round() / 100.0)
                    .collect::<Vec<_>>(),
            );
        }
        let old = cfg.policy.weak_evidence_top;
        // Damped, then hard-clamped. The lower clamp is floor-relative,
        // never absolute: a line at or under the delivery floor could never
        // fire, but the score scale itself is per-graph evidence (see
        // policy::WEAK_LINE_ABOVE_FLOOR). The clamp reads the DAMPED value
        // (policy::AUTO_TUNE_DAMPING), so one noisy probe register cannot
        // teleport the line and a glitched fit stays inside the band.
        let lo = (cfg.policy.delivery_floor + WEAK_LINE_ABOVE_FLOOR).clamp(0.0, WEAK_LINE_MAX);
        let anchor = if old.is_finite() {
            old.clamp(lo, WEAK_LINE_MAX)
        } else {
            WEAK_LINE_MAX
        };
        let target = if fitted.is_finite() {
            (anchor + (fitted - anchor) * crate::policy::AUTO_TUNE_DAMPING).clamp(lo, WEAK_LINE_MAX)
        } else {
            anchor
        };
        if (target - old).abs() < AUTO_TUNE_MIN_DELTA {
            return Ok(None);
        }
        cfg.policy.weak_evidence_top = target;
        Ok(Some(format!(
            "weak line {old:.3} -> {target:.3} (fit {fitted:.3}, damped) from {} phantom probes at q{:.0}, two families",
            template_tops.len() + transplant_tops.len(),
            q * 100.0
        )))
    }

    /// Nodes whose `code_refs` cover a repo-relative file path — the
    /// file-read match hook's lookup (PLAN §10 ambient hooks). A ref matches
    /// when it names the file exactly or a directory above it. Only current,
    /// non-stale knowledge surfaces (ambient value must not be ambient
    /// noise), strongest trust first.
    pub fn match_code_refs(&self, path: &str, limit: usize) -> Result<Vec<Node>> {
        let path = path.trim().trim_start_matches("./").trim_end_matches('/');
        if path.is_empty() {
            return Ok(Vec::new());
        }
        let covers = |r: &str| {
            let r = r.trim().trim_start_matches("./").trim_end_matches('/');
            !r.is_empty() && (r == path || path.starts_with(&format!("{r}/")))
        };
        let mut hits: Vec<Node> = self
            .store
            .all_nodes()?
            .into_iter()
            .filter(|n| {
                n.valid_until.is_none() && !n.stale && n.code_refs.iter().any(|r| covers(r))
            })
            .collect();
        hits.sort_by(|a, b| {
            b.trust
                .total_cmp(&a.trust)
                .then((b.created_at, &b.id).cmp(&(a.created_at, &a.id)))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// [`Engine::notify`] for sibling modules (tasks).
    pub(crate) fn emit(&self, event: ChangeEvent) {
        self.notify(event)
    }

    /// The acting session, as audit rows stamp it.
    pub(crate) fn audit_session(&self) -> Option<String> {
        self.audit_origin
            .session_id
            .clone()
            .filter(|s| !s.is_empty())
    }

    /// The acting front-end: pane | mcp | daemon | cli | library.
    pub(crate) fn audit_origin_name(&self) -> String {
        self.audit_origin.origin.clone()
    }

    /// Journal one row for a sibling module (tasks), under this engine's
    /// origin and session.
    pub(crate) fn audit_row(
        &self,
        action: &str,
        entity: &str,
        entity_id: &str,
        title: Option<String>,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) -> Result<()> {
        self.audit(action, entity, entity_id, title, before, after, None)
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

    // ---- audit journal (PLAN §10): every mutation appends one row with
    // before/after snapshots and this process's context. Reads (search touch,
    // brief inclusion) are deliberately not journaled — they'd drown the edits.

    /// One page of the journal, newest first (keyset pagination on `seq`).
    /// A row an undo reverted carries `undone_by`, unless that undo was
    /// itself undone.
    pub fn audit_log(
        &self,
        before: Option<i64>,
        entity_id: Option<&str>,
        limit: usize,
    ) -> Result<AuditPage> {
        let mut page = self.store.audit_page(before, entity_id, limit)?;
        let (undone, _) = self.undo_state()?;
        for e in &mut page.entries {
            e.undone_by = undone.get(&e.seq).copied();
        }
        Ok(page)
    }

    /// What the journal says about undos (0.9.10): original seq → the
    /// `undone` row that reverted it (only undos that still stand), and every
    /// operation an undo wrote.
    fn undo_state(
        &self,
    ) -> Result<(
        std::collections::HashMap<i64, i64>,
        std::collections::HashSet<String>,
    )> {
        let marks = self.store.audit_rows(&AuditQuery {
            action: Some(UNDONE.into()),
            ..AuditQuery::default()
        })?;
        let target = |m: &AuditEntry| m.entity_id.parse::<i64>().ok();
        let targets: std::collections::HashSet<i64> = marks.iter().filter_map(target).collect();
        let undo_ops: std::collections::HashSet<String> =
            marks.iter().filter_map(|m| m.op_id.clone()).collect();
        // An undo stands until one of its own writes is undone.
        let mut reverted = std::collections::HashSet::new();
        for op in &undo_ops {
            let rows = self.store.audit_rows(&AuditQuery {
                op_id: Some(op.clone()),
                ..AuditQuery::default()
            })?;
            if rows
                .iter()
                .any(|r| r.action != UNDONE && targets.contains(&r.seq))
            {
                reverted.insert(op.clone());
            }
        }
        let mut undone = std::collections::HashMap::new();
        for m in &marks {
            if m.op_id.as_ref().is_some_and(|op| reverted.contains(op)) {
                continue;
            }
            if let Some(t) = target(m) {
                undone.insert(t, m.seq);
            }
        }
        Ok((undone, undo_ops))
    }

    /// Undo one journal row, or — when it belongs to an operation (a merge, a
    /// delete with its cascade, an earlier undo) — the whole operation
    /// (0.9.10). User-only, like hard delete: served to the pane over HTTP,
    /// never to MCP. `dry_run` reports what would happen and changes nothing.
    pub fn undo_entry(&self, seq: i64, dry_run: bool) -> Result<UndoReport> {
        let Some(row) = self
            .store
            .audit_rows(&AuditQuery {
                seq: Some(seq),
                ..AuditQuery::default()
            })?
            .pop()
        else {
            return Err(crate::Error::NotFound(format!("audit row #{seq}")));
        };
        let rows = match &row.op_id {
            Some(op) => self.store.audit_rows(&AuditQuery {
                op_id: Some(op.clone()),
                ..AuditQuery::default()
            })?,
            None => vec![row],
        };
        self.undo_rows(rows, dry_run)
    }

    /// Undo everything one session wrote, newest first (0.9.10): the notes it
    /// created are removed, its edits and verdicts are reverted. Rows that
    /// changed afterwards in another session are skipped and reported.
    pub fn undo_session(&self, session_id: &str, dry_run: bool) -> Result<UndoReport> {
        let rows = self.store.audit_rows(&AuditQuery {
            session_id: Some(session_id.to_string()),
            ..AuditQuery::default()
        })?;
        if !rows.iter().any(undoable) {
            return Err(crate::Error::NotFound(format!(
                "session {session_id} wrote nothing that can be undone"
            )));
        }
        self.undo_rows(rows, dry_run)
    }

    fn undo_rows(&self, rows: Vec<AuditEntry>, dry_run: bool) -> Result<UndoReport> {
        let _op = self.op_scope();
        let this_op = self.audit_op.lock().ok().and_then(|op| op.clone());
        let (undone, undo_ops) = self.undo_state()?;
        let mut rows: Vec<AuditEntry> = rows.into_iter().filter(undoable).collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.seq));
        let batch: std::collections::HashSet<i64> = rows.iter().map(|r| r.seq).collect();
        let mut report = UndoReport {
            dry_run,
            ..UndoReport::default()
        };
        // What a dry run has "done" so far, so later rows see it: id → exists.
        let mut sim: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for row in rows {
            let item = |note: Option<String>| UndoItem {
                seq: row.seq,
                action: row.action.clone(),
                entity: row.entity.clone(),
                entity_id: row.entity_id.clone(),
                title: row.title.clone(),
                effect: undo_effect(&row).to_string(),
                note,
            };
            if let Some(by) = undone.get(&row.seq) {
                report
                    .skipped
                    .push(item(Some(format!("already undone by #{by}"))));
                continue;
            }
            // Changed since: a later write to the same entity that is not part
            // of this undo, not itself undone, and not written by an undo.
            let later = self.store.audit_rows(&AuditQuery {
                entity_id: Some(row.entity_id.clone()),
                after: Some(row.seq),
                ..AuditQuery::default()
            })?;
            let newer = later.iter().find(|r| {
                undoable(r)
                    && !batch.contains(&r.seq)
                    && !undone.contains_key(&r.seq)
                    && !r
                        .op_id
                        .as_ref()
                        .is_some_and(|op| undo_ops.contains(op) || this_op.as_ref() == Some(op))
            });
            if let Some(newer) = newer {
                report.skipped.push(item(Some(format!(
                    "changed since: #{} ({}) is newer — undo it first",
                    newer.seq, newer.action
                ))));
                continue;
            }
            match self.undo_one(&row, dry_run, &mut sim)? {
                Ok(note) => {
                    if !dry_run {
                        self.audit(
                            UNDONE,
                            "audit",
                            &row.seq.to_string(),
                            Some(format!(
                                "undid #{} — {} {} {}",
                                row.seq,
                                row.action,
                                row.entity,
                                row.title.as_deref().unwrap_or(&row.entity_id)
                            )),
                            None,
                            None,
                            None,
                        )?;
                    }
                    report.undone.push(item(note));
                }
                Err(reason) => report.skipped.push(item(Some(reason))),
            }
        }
        Ok(report)
    }

    /// Revert one row. The outer `Err` is a store failure; the inner one is
    /// the reason this row cannot be undone. `Ok(Some(note))` reports what
    /// else the undo touched.
    fn undo_one(
        &self,
        row: &AuditEntry,
        dry_run: bool,
        sim: &mut std::collections::HashMap<String, bool>,
    ) -> Result<std::result::Result<Option<String>, String>> {
        let node_exists = |sim: &std::collections::HashMap<String, bool>, id: &str| {
            Ok::<bool, crate::Error>(match sim.get(id) {
                Some(present) => *present,
                None => self.store.get_node(id)?.is_some(),
            })
        };
        let edge_exists = |sim: &std::collections::HashMap<String, bool>, id: &str| {
            Ok::<bool, crate::Error>(match sim.get(id) {
                Some(present) => *present,
                None => self.store.get_edge(id)?.is_some(),
            })
        };
        let id = row.entity_id.as_str();
        match (row.entity.as_str(), &row.before, &row.after) {
            // A created note: remove it, and the links it gathered.
            ("node", None, Some(_)) => {
                if !node_exists(sim, id)? {
                    return Ok(Err("the note is already gone".into()));
                }
                let links = self.store.edges_out(id)?.len() + self.store.edges_in(id)?.len();
                if !dry_run {
                    self.delete_node(id)?;
                }
                sim.insert(id.to_string(), false);
                Ok(Ok((links > 0).then(|| {
                    format!(
                        "also removes {links} link{}",
                        if links == 1 { "" } else { "s" }
                    )
                })))
            }
            // A deleted note: bring it back as it was.
            ("node", Some(before), None) => {
                if node_exists(sim, id)? {
                    return Ok(Err("a note with this id exists again".into()));
                }
                let node: Node = serde_json::from_value(before.clone())?;
                if !dry_run {
                    self.restore_node(&node, None)?;
                }
                sim.insert(id.to_string(), true);
                Ok(Ok(None))
            }
            // An edit, verdict or trust change: put the note back as it was.
            ("node", Some(before), Some(_)) => {
                if !node_exists(sim, id)? {
                    return Ok(Err("the note is gone".into()));
                }
                let node: Node = serde_json::from_value(before.clone())?;
                if !dry_run {
                    let current = self.store.get_node(id)?;
                    self.restore_node(&node, current.as_ref())?;
                }
                Ok(Ok(None))
            }
            ("edge", None, Some(_)) => {
                if !edge_exists(sim, id)? {
                    return Ok(Err("the link is already gone".into()));
                }
                if !dry_run {
                    self.delete_edge(id)?;
                }
                sim.insert(id.to_string(), false);
                Ok(Ok(None))
            }
            ("edge", Some(before), after) => {
                let edge: Edge = serde_json::from_value(before.clone())?;
                let exists = edge_exists(sim, id)?;
                match after {
                    None if exists => return Ok(Err("the link exists again".into())),
                    Some(_) if !exists => return Ok(Err("the link is gone".into())),
                    _ => {}
                }
                if !node_exists(sim, &edge.from_id)? || !node_exists(sim, &edge.to_id)? {
                    return Ok(Err("one of its notes is gone".into()));
                }
                if !dry_run {
                    let current = self.store.get_edge(id)?;
                    self.restore_edge(&edge, current.as_ref())?;
                }
                sim.insert(id.to_string(), true);
                Ok(Ok(None))
            }
            _ => Ok(Err("this row cannot be undone".into())),
        }
    }

    /// Put a note back exactly as a journal snapshot recorded it, re-embedded.
    fn restore_node(&self, node: &Node, current: Option<&Node>) -> Result<()> {
        self.store.upsert_node(node)?;
        self.embed_node(node)?;
        let stored = self
            .store
            .get_node(&node.id)?
            .unwrap_or_else(|| node.clone());
        self.audit_node(RESTORED, current, Some(&stored))?;
        self.notify(match current {
            Some(_) => ChangeEvent::NodeUpdated(stored),
            None => ChangeEvent::NodeAdded(stored),
        });
        Ok(())
    }

    /// Put a link back exactly as a journal snapshot recorded it. A restored
    /// `conflicts-with` demotes again, a restored earlier form lifts it.
    fn restore_edge(&self, edge: &Edge, current: Option<&Edge>) -> Result<()> {
        self.store.upsert_edge(edge)?;
        self.audit_edge(RESTORED, current, Some(edge))?;
        self.notify(match current {
            Some(_) => ChangeEvent::EdgeUpdated(edge.clone()),
            None => ChangeEvent::EdgeAdded(edge.clone()),
        });
        self.reconcile_conflict_demotion(edge)?;
        Ok(())
    }

    /// Mark the journal rows written from here until the scope drops as one
    /// operation. Nested scopes join the outer one.
    fn op_scope(&self) -> OpScope<'_> {
        let Ok(mut op) = self.audit_op.lock() else {
            return OpScope { slot: None };
        };
        if op.is_some() {
            return OpScope { slot: None };
        }
        *op = Some(crate::id::new_id());
        OpScope {
            slot: Some(&self.audit_op),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        action: &str,
        entity: &str,
        entity_id: &str,
        title: Option<String>,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
        session_id: Option<String>,
    ) -> Result<()> {
        self.store.add_audit(&AuditEntry {
            seq: 0,
            ts: crate::store::now(),
            action: action.to_string(),
            entity: entity.to_string(),
            entity_id: entity_id.to_string(),
            title,
            before,
            after,
            origin: self.audit_origin.origin.clone(),
            session_id: session_id.or_else(|| self.audit_origin.session_id.clone()),
            cwd: self.audit_cwd.clone(),
            pid: Some(self.audit_pid),
            version: Some(self.audit_version.clone()),
            op_id: self.audit_op.lock().ok().and_then(|op| op.clone()),
            undone_by: None,
        })
    }

    fn audit_node(&self, action: &str, before: Option<&Node>, after: Option<&Node>) -> Result<()> {
        let Some(subject) = after.or(before) else {
            return Ok(());
        };
        // The node's stored session_id names its creator, so it only
        // attributes "created" rows; every later action is whoever holds the
        // engine now (the audit origin), not the session that made the node.
        let actor_session = match action {
            "created" => subject.session_id.clone(),
            _ => None,
        };
        self.audit(
            action,
            "node",
            &subject.id,
            Some(subject.title.clone()),
            before.map(serde_json::to_value).transpose()?,
            after.map(serde_json::to_value).transpose()?,
            actor_session,
        )
    }

    fn audit_edge(&self, action: &str, before: Option<&Edge>, after: Option<&Edge>) -> Result<()> {
        let Some(subject) = after.or(before) else {
            return Ok(());
        };
        self.audit(
            action,
            "edge",
            &subject.id,
            Some(self.edge_label(subject)),
            before.map(serde_json::to_value).transpose()?,
            after.map(serde_json::to_value).transpose()?,
            None,
        )
    }

    /// Sentence-shaped display label for an edge's journal rows — endpoint
    /// titles are snapshotted so the row stays readable after deletions.
    fn edge_label(&self, e: &Edge) -> String {
        let title = |id: &str| {
            self.store
                .get_node(id)
                .ok()
                .flatten()
                .map(|n| n.title)
                .unwrap_or_else(|| id.to_string())
        };
        format!(
            "\"{}\" {} \"{}\"",
            title(&e.from_id),
            e.edge_type.as_str(),
            title(&e.to_id)
        )
    }

    /// Add a node and embed it (full-field composition) in one step. Trust is computed
    /// from timestamps at read time; user-authored nodes are approved by
    /// construction (the store stamps `approved_at`).
    pub fn add_node(&self, n: NewNode) -> Result<Node> {
        self.add_node_opts(n, true, None)
    }

    /// `enforce_required_fields: false` is reserved for engine-minted notes
    /// (the tombstone a hard delete leaves): a required custom field is a
    /// contract for authors, and must never be able to veto a user's delete.
    /// Provided values are still validated either way.
    fn add_node_opts(
        &self,
        mut n: NewNode,
        enforce_required_fields: bool,
        cache: Option<&EmbedCache>,
    ) -> Result<Node> {
        self.check_node_type(&n.node_type)?;
        let cfg = self.store.config();
        // Worklist-role types are live items from birth — the write boundary
        // owns the default so every surface (MCP, pane, raw HTTP) gets it.
        if n.status.is_none()
            && cfg
                .type_def(n.node_type.as_str())
                .is_some_and(|t| t.roles.worklist)
        {
            n.status = Some(NodeStatus::Open);
        }
        // Version tracking: auto-stamp the current working version on
        // version-bound types (explicit versions — digestion of historical
        // material — always win).
        if n.version.is_none()
            && cfg.versioning.enabled
            && cfg
                .type_def(n.node_type.as_str())
                .is_none_or(|t| t.roles.versioned)
        {
            n.version = self.store.current_version()?;
        }
        // Custom fields (0.9.0): an empty map is the same as none; the full
        // map is validated here so every surface gets one teaching error.
        if n.fields.as_ref().is_some_and(|f| f.is_empty()) {
            n.fields = None;
        }
        self.check_fields_ext(&n.node_type, n.fields.as_ref(), enforce_required_fields)?;
        let node = self.store.add_node(n)?;
        self.embed_node_with(&node, cache)?;
        self.audit_node("created", None, Some(&node))?;
        self.notify(ChangeEvent::NodeAdded(node.clone()));
        Ok(node)
    }

    /// The write boundary of ontology-as-data (PLAN §7D): a node type must
    /// exist in this graph's ontology. Shape was checked at parse; existence
    /// can only be checked here, where the graph's config is known.
    fn check_node_type(&self, t: &NodeType) -> Result<()> {
        let cfg = self.store.config();
        if cfg.type_def(t.as_str()).is_none() {
            return Err(crate::Error::Config(format!(
                "unknown node type {:?} — this graph's ontology defines: {}",
                t.as_str(),
                cfg.ontology
                    .types
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(())
    }

    /// The custom-fields write boundary (0.9.0): validates the FULL resolved
    /// map a node will carry — unknown names fail loudly, kinds must match,
    /// required fields must be present. The error is a teaching error: it
    /// names the graph's whole field roster and the exact call shape, so an
    /// agent that has never seen this graph can self-correct in one step.
    fn check_fields(
        &self,
        node_type: &NodeType,
        fields: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> Result<()> {
        self.check_fields_ext(node_type, fields, true)
    }

    fn check_fields_ext(
        &self,
        node_type: &NodeType,
        fields: Option<&serde_json::Map<String, serde_json::Value>>,
        enforce_required: bool,
    ) -> Result<()> {
        let cfg = self.store.config();
        let empty = serde_json::Map::new();
        let fields = fields.unwrap_or(&empty);
        if cfg.fields.is_empty() {
            if fields.is_empty() {
                return Ok(());
            }
            return Err(crate::Error::Config(
                "this graph declares no custom fields — drop the `fields` object \
                 (definitions are a user gesture: pane Settings or PUT /config)"
                    .into(),
            ));
        }
        let applicable = cfg.fields_for(node_type.as_str());
        let roster = || {
            let lines: Vec<String> = applicable
                .iter()
                .map(|f| {
                    format!(
                        "{} ({}{}{})",
                        f.name,
                        f.kind.as_str(),
                        if f.required { ", required" } else { "" },
                        if f.kind == crate::config::FieldKind::Enum {
                            format!(": {}", f.values.join(" | "))
                        } else {
                            String::new()
                        }
                    )
                })
                .collect();
            if lines.is_empty() {
                format!("no custom fields apply to {}", node_type.as_str())
            } else {
                format!(
                    "fields for {}: {} — pass as {{\"fields\": {{\"name\": value}}}}",
                    node_type.as_str(),
                    lines.join("; ")
                )
            }
        };
        for (name, value) in fields {
            let Some(def) = applicable.iter().find(|f| f.name == *name) else {
                return Err(crate::Error::Config(format!(
                    "unknown custom field {name:?} for type {} — {}",
                    node_type.as_str(),
                    roster()
                )));
            };
            if let Err(why) = field_value_ok(def, value) {
                return Err(crate::Error::Config(format!(
                    "custom field {name:?}: {why} — {}",
                    roster()
                )));
            }
        }
        let missing: Vec<&str> = applicable
            .iter()
            .filter(|f| enforce_required && f.required && !fields.contains_key(&f.name))
            .map(|f| f.name.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(crate::Error::Config(format!(
                "missing required custom field(s) {} for type {} — {}",
                missing.join(", "),
                node_type.as_str(),
                roster()
            )));
        }
        Ok(())
    }

    /// Resolve a patch's MERGE intent against the stored map: present keys
    /// overwrite, `null` deletes, absent keys survive. Returns the full
    /// replacement map the store gets (empty = clear all).
    fn resolve_fields_patch(
        before: Option<&serde_json::Map<String, serde_json::Value>>,
        patch: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut out = before.cloned().unwrap_or_default();
        for (k, v) in patch {
            if v.is_null() {
                out.remove(k);
            } else {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    /// The bitemporal-search boundary (0.9.0): a `date_field` clock must
    /// name declared date-kind custom fields, and must travel with a window
    /// — an unaimed clock filters nothing and would silently return the
    /// default result while looking like an event-time answer.
    fn check_clock(&self, filter: &SearchFilter) -> Result<()> {
        use crate::timespec::TimeClock;
        let names: Vec<&str> = match &filter.clock {
            TimeClock::CreatedAt => return Ok(()),
            TimeClock::Field(name) => vec![name.as_str()],
            TimeClock::FieldSpan(from, to) => vec![from.as_str(), to.as_str()],
        };
        let cfg = self.store.config();
        for name in names {
            let ok = cfg
                .field_def(name)
                .is_some_and(|f| f.kind == crate::config::FieldKind::Date);
            if !ok {
                let dates: Vec<&str> = cfg
                    .fields
                    .iter()
                    .filter(|f| f.kind == crate::config::FieldKind::Date)
                    .map(|f| f.name.as_str())
                    .collect();
                return Err(crate::Error::Config(format!(
                    "date_field {name:?} is not a declared date-kind custom field — {}",
                    if dates.is_empty() {
                        "this graph declares none (definitions: pane Settings / PUT /config)"
                            .to_string()
                    } else {
                        format!("this graph's date fields: {}", dates.join(", "))
                    }
                )));
            }
        }
        if filter.window.is_open() {
            return Err(crate::Error::Config(
                "date_field needs a window — pass after and/or before to aim it".into(),
            ));
        }
        Ok(())
    }

    /// Same boundary for edge verbs: a triple can only use a verb this
    /// graph's ontology declares.
    fn check_edge_type(&self, t: &EdgeType) -> Result<()> {
        let cfg = self.store.config();
        if cfg.verb_def(t.as_str()).is_none() {
            return Err(crate::Error::Config(format!(
                "unknown edge verb {:?} — this graph's ontology defines: {}",
                t.as_str(),
                cfg.ontology
                    .verbs
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(())
    }

    /// Patch a node and re-embed if any embedded field changed (title, body,
    /// tags, code_refs). Any update refreshes `last_seen` (the store stamps
    /// it): edited knowledge is in-use knowledge.
    pub fn update_node(&self, id: &str, patch: NodePatch) -> Result<Node> {
        self.update_node_with(id, patch, None)
    }

    /// [`Engine::update_node`] taking vectors computed before the lock
    /// (0.9.11) wherever their texts match.
    fn update_node_with(
        &self,
        id: &str,
        mut patch: NodePatch,
        cache: Option<&EmbedCache>,
    ) -> Result<Node> {
        if let Some(t) = &patch.node_type {
            self.check_node_type(t)?;
        }
        let touches_text = patch.title.is_some()
            || patch.body.is_some()
            || patch.tags.is_some()
            || patch.code_refs.is_some()
            || patch.fields.is_some();
        let before = self.store.get_node(id)?;
        // Custom fields (0.9.0): the patch is MERGE intent (null deletes a
        // key). Resolve it against the stored node, validate the result —
        // against the patched-to type on a retype — and hand the store the
        // full replacement map. A retype alone re-validates too: the new
        // type's required fields must already be satisfied.
        if patch.fields.is_some() || patch.node_type.is_some() {
            let stored = before.as_ref().and_then(|b| b.fields.as_ref());
            let resolved = match &patch.fields {
                Some(p) => Self::resolve_fields_patch(stored, p),
                None => stored.cloned().unwrap_or_default(),
            };
            let target_type = patch
                .node_type
                .clone()
                .or_else(|| before.as_ref().map(|b| b.node_type.clone()));
            if let Some(t) = &target_type {
                self.check_fields(t, Some(&resolved))?;
            }
            if patch.fields.is_some() {
                patch.fields = Some(resolved);
            }
        }
        let node = self.store.update_node(id, patch)?;
        if touches_text {
            self.embed_node_with(&node, cache)?;
        }
        // Setting valid_until is the supersede flow (replaces verdict), not an
        // edit — journal it under its real name.
        let action = match &before {
            Some(b) if b.valid_until.is_none() && node.valid_until.is_some() => "archived",
            _ => "updated",
        };
        self.audit_node(action, before.as_ref(), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
    }

    /// Confirm a node still true without changing its content: stamps
    /// `confirmed_at` (restarting trust on the confirmed curve) and clears
    /// any evidence demotion. A deliberate act — the pane's "Confirm still
    /// true" — unlike retrieval, which never refreshes trust (PLAN §6A).
    pub fn reconfirm(&self, id: &str) -> Result<Node> {
        self.update_node(id, NodePatch::default())
    }

    /// Explicit approval: trust restarts at its ceiling — and on stable
    /// knowledge holds there until contradicting evidence lands. User action
    /// in the pane, or the assistant **only on explicit user demand /
    /// verbatim verification** (enforced by skill policy).
    pub fn approve(&self, id: &str) -> Result<Node> {
        let before = self.store.get_node(id)?;
        let node = self.store.approve(id)?;
        self.audit_node("approved", before.as_ref(), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
    }

    /// Withdraw an approval (and any pin): trust falls back to the
    /// confirmed/created anchor. User-only, like the endorsements it undoes.
    pub fn revoke_approval(&self, id: &str) -> Result<Node> {
        let before = self.store.get_node(id)?;
        let node = self.store.revoke_approval(id)?;
        // Journal what was actually withdrawn — the pane offers this action
        // on pinned-but-never-approved nodes too.
        let action = match &before {
            Some(b) if b.approved_at.is_some() => "unapproved",
            Some(b) if b.trust_override.is_some() => "unpinned",
            _ => return Ok(node), // nothing to withdraw — no-op, no row
        };
        self.audit_node(action, before.as_ref(), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
    }

    /// Set or clear the constant-trust pin (PLAN §6A trust v2). Pin = 1.0;
    /// any 0..=1 value is allowed; `None` unpins. Pinned nodes never decay,
    /// never auto-archive, and evidence events skip them — user-only, the
    /// durable-memory counterpart of hard delete.
    pub fn set_trust_override(&self, id: &str, value: Option<f64>) -> Result<Node> {
        let before = self.store.get_node(id)?;
        let node = self.store.set_trust_override(id, value)?;
        let action = if value.is_some() {
            "pinned"
        } else {
            "unpinned"
        };
        self.audit_node(action, before.as_ref(), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
    }

    pub fn delete_node(&self, id: &str) -> Result<bool> {
        let _op = self.op_scope();
        let before = self.store.get_node(id)?;
        // The store cascades the node's edges; journal them too (0.9.10), so
        // undoing the delete can bring the links back. They are journaled
        // first, so an undo — newest first — restores the node before them.
        let mut cascaded = self.store.edges_out(id)?;
        for e in self.store.edges_in(id)? {
            if !cascaded.iter().any(|c| c.id == e.id) {
                cascaded.push(e);
            }
        }
        let removed = self.store.delete_node(id)?;
        if removed {
            for e in &cascaded {
                self.audit_edge("deleted", Some(e), None)?;
            }
            self.audit_node("deleted", before.as_ref(), None)?;
            self.notify(ChangeEvent::NodeDeleted(id.to_string()));
        }
        Ok(removed)
    }

    /// Hard delete that leaves a trace: mints a tombstone-role note recording
    /// what was removed and why, then cascades the victim (0.9.0 — before
    /// this, deletion was the one memory operation that left nothing behind,
    /// so an agent could re-learn a deliberately killed fact). User-only,
    /// like delete itself. When the ontology declares no tombstone type this
    /// degrades to a plain delete — the config decides, never the caller.
    ///
    /// `keep_text` (0.9.2, the pane's default) carries the victim's body,
    /// tags, and code_refs into the tombstone. A 0.9.0 mint embedded only
    /// the victim's title plus the reason, so a paraphrase of the buried
    /// CONTENT landed nowhere near it and the `tombstoned` write warning had
    /// nothing to fire on — the marker guarded the headline, not the
    /// knowledge. Off is the purge shape: identity and reason only, for a
    /// delete whose point is that the text goes away (the audit journal
    /// keeps the pre-image either way, as for any delete).
    ///
    /// The tombstone takes over the victim's live edges to anchor-role
    /// nodes (`about` a code subject): the removal stays attached to the
    /// place it happened, so a traversal from the anchor meets the tombstone
    /// where it used to meet the victim. Every other edge cascades with the
    /// victim — "Removed: X because P" is not a sentence, and edges never
    /// dangle. The victim's identity lives in the tombstone's body.
    pub fn delete_node_with_tombstone(
        &self,
        id: &str,
        reason: Option<&str>,
        keep_text: bool,
    ) -> Result<(bool, Option<Node>)> {
        // One operation in the journal: undo reverts all of it (0.9.10).
        let _op = self.op_scope();
        let Some(victim) = self.store.get_node(id)? else {
            return Ok((false, None));
        };
        let cfg = self.store.config();
        let Some(ts_type) = cfg.tombstone_type().map(str::to_string) else {
            return Ok((self.delete_node(id)?, None));
        };
        let ts_def = cfg.type_def(&ts_type).expect("tombstone_type is declared");
        let mut body = format!(
            "Deleted {} {:?} (id {}).",
            victim.node_type.as_str(),
            victim.title,
            victim.id
        );
        match reason {
            Some(r) if !r.trim().is_empty() => {
                body.push_str("\n\n**Why:** ");
                body.push_str(r.trim());
            }
            _ => {}
        }
        let (tags, code_refs) = if keep_text {
            if let Some(text) = victim
                .body
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
            {
                body.push_str("\n\n**Removed text:** ");
                body.push_str(text);
            }
            (victim.tags.clone(), victim.code_refs.clone())
        } else {
            (Vec::new(), Vec::new())
        };
        // The mint skips required-field enforcement: deletion isn't
        // authoring, and field config must never veto a user's delete.
        let tombstone = self.add_node_opts(
            NewNode {
                node_type: NodeType::parse(&ts_type)?,
                title: format!("Removed: {}", victim.title),
                body: Some(body),
                created_at: None,
                durability: ts_def.durability,
                source: Source::User,
                session_id: None,
                status: None,
                code_refs,
                tags,
                version: None,
                props: None,
                fields: None,
            },
            false,
            None,
        )?;
        // Rehome the victim's live anchor edges before the cascade takes
        // them. Same contract as merge's rehoming: the edge keeps its id and
        // timestamps — the connection moved, it didn't recur.
        for edge in self.store.edges_out(&victim.id)? {
            if edge.valid_until.is_some() {
                continue;
            }
            let Some(target) = self.store.get_node(&edge.to_id)? else {
                continue;
            };
            if !is_anchor(&cfg, &target) {
                continue;
            }
            let before = edge.clone();
            let mut moved = edge;
            moved.from_id = tombstone.id.clone();
            self.store.upsert_edge(&moved)?;
            self.audit_edge("updated", Some(&before), Some(&moved))?;
            self.notify(ChangeEvent::EdgeUpdated(moved));
        }
        let removed = self.delete_node(id)?;
        Ok((removed, Some(tombstone)))
    }

    pub fn get_node(&self, id: &str) -> Result<Option<Node>> {
        self.store.get_node(id)
    }

    pub fn add_edge(&self, e: NewEdge) -> Result<Edge> {
        self.check_edge_type(&e.edge_type)?;
        let edge = self.store.add_edge(e)?;
        self.audit_edge("created", None, Some(&edge))?;
        self.notify(ChangeEvent::EdgeAdded(edge.clone()));
        self.reconcile_conflict_demotion(&edge)?;
        // Before the retirement: a pending list drops archived pairs.
        self.settle_linked_suspects(&edge)?;
        self.retire_superseded(&edge)?;
        self.propagate_from_edge(&edge)?;
        Ok(edge)
    }

    /// A live link says how two notes relate, so a pending suspect between
    /// them is settled by it (0.11.0): `replaces` / `conflicts-with` confirm
    /// it, any other verb dismisses it. A sleep's state-of note raises
    /// suspects against the notes it summarises the moment it is written;
    /// its `builds-on` links clear them.
    fn settle_linked_suspects(&self, edge: &Edge) -> Result<()> {
        if matches!(
            edge.status,
            Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
        ) {
            return Ok(());
        }
        let cfg = self.store.config();
        let verb = edge.edge_type.as_str();
        let status = if verb == cfg.supersession_verb() || verb == cfg.contradiction_verb() {
            SuspectStatus::Confirmed
        } else {
            SuspectStatus::Dismissed
        };
        let pair = |a: &str, b: &str| {
            (a == edge.from_id && b == edge.to_id) || (a == edge.to_id && b == edge.from_id)
        };
        let mut settled = false;
        for s in self.store.suspects_pending()? {
            if pair(&s.a.id, &s.b.id) {
                self.store.set_suspect_status(&s.id, status)?;
                settled = true;
            }
        }
        if settled {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(())
    }

    /// Live edges per node id (resolved/dismissed edges don't count) — the
    /// connectedness `canon_order: connected` weighs.
    fn live_degrees(&self) -> std::collections::HashMap<String, usize> {
        let mut out = std::collections::HashMap::new();
        for e in self.store.all_edges().unwrap_or_default() {
            if matches!(e.status, Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)) {
                continue;
            }
            *out.entry(e.from_id).or_insert(0) += 1;
            *out.entry(e.to_id).or_insert(0) += 1;
        }
        out
    }

    /// A judged contradiction or supersession reaches whatever depended on
    /// its target (0.9.9): the edge's source now disagrees with, or replaces,
    /// the target, so every note that `inherits` from the target (builds on
    /// it, needs it, holds because of it) is queued against the source. A
    /// tombstone's `replaces` doesn't propagate — burying knowledge is the
    /// user's gesture, not a new claim to hold dependents against.
    fn propagate_from_edge(&self, edge: &Edge) -> Result<()> {
        let cfg = self.store.config();
        let verb = edge.edge_type.as_str();
        if (verb != cfg.supersession_verb() && verb != cfg.contradiction_verb())
            || matches!(
                edge.status,
                Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
            )
        {
            return Ok(());
        }
        let Some(source) = self.store.get_node(&edge.from_id)? else {
            return Ok(());
        };
        if is_tombstone(&cfg, &source) {
            return Ok(());
        }
        let score = edge.confidence.unwrap_or(1.0);
        if self.propagate_inherited(&source, &edge.to_id, score)? > 0 {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(())
    }

    /// Queue `(newer, dependent)` for every live note that `inherits` from
    /// `upstream` (an incoming edge whose verb carries the role) — the
    /// transitive contradiction a pairwise judge cannot see, because the two
    /// notes name different subjects and only the edge says one holds what
    /// the other holds. The hint label is `inherited`; its score is the
    /// strength of the upstream contradiction. Returns how many were queued.
    fn propagate_inherited(&self, newer: &Node, upstream: &str, score: f64) -> Result<usize> {
        let cfg = self.store.config();
        let newer_vec = self.store.embedding_of(&newer.id)?;
        let mut added = 0;
        for e in self.store.edges_in(upstream)? {
            if e.from_id == newer.id
                || !cfg.verb_inherits(e.edge_type.as_str())
                || matches!(e.status, Some(EdgeStatus::Resolved | EdgeStatus::Dismissed))
            {
                continue;
            }
            let Some(dependent) = self.store.get_node(&e.from_id)? else {
                continue;
            };
            if dependent.valid_until.is_some()
                || is_anchor(&cfg, &dependent)
                || is_tombstone(&cfg, &dependent)
                || self.store.pair_linked(&newer.id, &dependent.id)?
                || self.store.suspect_between(&newer.id, &dependent.id)?
            {
                continue;
            }
            let similarity = match (&newer_vec, self.store.embedding_of(&dependent.id)?) {
                (Some(a), Some(b)) => cosine(a, &b),
                _ => 0.0,
            };
            let (a, b) = if newer.created_at >= dependent.created_at {
                (&newer.id, &dependent.id)
            } else {
                (&dependent.id, &newer.id)
            };
            self.store
                .add_suspect(a, b, similarity, Some(("inherited", score, None)))?;
            added += 1;
        }
        Ok(added)
    }

    /// Supersession is retirement: wherever a live `replaces` edge lands — a
    /// suspect verdict, an assistant `link`, a retype in the pane — the node
    /// it replaces leaves the canon. It is archived, so retrieval, the brief
    /// and the pane stop offering it, and it survives in the `replaces` chain
    /// (`timeline`) where superseded knowledge belongs.
    ///
    /// Two deliberate asymmetries: pinned nodes are exempt (a pin is the
    /// user's "never fade" — the pane's replaces verdict overrides it
    /// explicitly, see [`Engine::resolve_suspect`]), and withdrawing the edge
    /// never un-archives. `valid_until` is also set by decay and by the user,
    /// so restoring it here would resurrect nodes nothing asked to see again.
    fn retire_superseded(&self, edge: &Edge) -> Result<()> {
        if edge.edge_type.as_str() != self.store.config().supersession_verb()
            || matches!(
                edge.status,
                Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
            )
        {
            return Ok(());
        }
        self.archive_superseded(&edge.to_id)?;
        Ok(())
    }

    /// Archive one superseded node; `false` when there was nothing to do
    /// (already archived, pinned, or gone).
    fn archive_superseded(&self, id: &str) -> Result<bool> {
        let Some(node) = self.store.get_node(id)? else {
            return Ok(false);
        };
        if node.valid_until.is_some() || node.trust_override.is_some() {
            return Ok(false);
        }
        self.update_node(
            id,
            NodePatch {
                valid_until: Some(crate::store::now()),
                ..NodePatch::default()
            },
        )?;
        Ok(true)
    }

    /// Heal graphs written before supersession retired its target: archive
    /// every still-current node sitting under a live `replaces` edge. Runs
    /// with the session-boundary validation and is idempotent — after the
    /// first pass it finds nothing.
    fn retire_superseded_sweep(&self) -> Result<usize> {
        let verb = self.store.config().supersession_verb().to_string();
        let mut retired = 0;
        for edge in self.store.all_edges()? {
            if edge.edge_type.as_str() != verb
                || matches!(
                    edge.status,
                    Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
                )
            {
                continue;
            }
            if self.archive_superseded(&edge.to_id)? {
                retired += 1;
            }
        }
        Ok(retired)
    }

    /// Keep endpoint demotions in lockstep with the edge's conflict state:
    /// a live `conflicts-with` is the evidence event that starts decay on the
    /// older claim — stable knowledge loses trust to evidence, never to time
    /// — and evidence that is withdrawn (edge resolved, dismissed, retyped,
    /// deleted) must take its demotion with it, or an innocent node keeps
    /// decaying after the contradiction is gone. (Pinned nodes are skipped
    /// inside demote.)
    fn reconcile_conflict_demotion(&self, edge: &Edge) -> Result<()> {
        let live = edge.edge_type.as_str() == self.store.config().contradiction_verb()
            && !matches!(
                edge.status,
                Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
            );
        if live {
            if let (Some(a), Some(b)) = (
                self.store.get_node(&edge.from_id)?,
                self.store.get_node(&edge.to_id)?,
            ) {
                let older = if a.created_at <= b.created_at { a } else { b };
                self.demote_node(&older, crate::store::now())?;
            }
        } else {
            for id in [&edge.from_id, &edge.to_id] {
                self.undemote_if_unconflicted(id)?;
            }
        }
        Ok(())
    }

    /// Stamp contradicting evidence on a node, with the journal row and SSE
    /// update a trust change deserves. No-op when already demoted or pinned.
    fn demote_node(&self, before: &Node, ts: i64) -> Result<()> {
        if self.store.demote(&before.id, ts)?
            && let Some(node) = self.store.get_node(&before.id)?
        {
            self.audit_node("demoted", Some(before), Some(&node))?;
            self.notify(ChangeEvent::NodeUpdated(node));
        }
        Ok(())
    }

    /// Clear a node's demotion once no live `conflicts-with` edge touches it.
    fn undemote_if_unconflicted(&self, id: &str) -> Result<()> {
        let Some(before) = self.store.get_node(id)? else {
            return Ok(());
        };
        if before.demoted_at.is_none() || self.store.has_active_conflict(id)? {
            return Ok(());
        }
        let node = self.store.clear_demotion(id)?;
        self.audit_node("undemoted", Some(&before), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node));
        Ok(())
    }

    pub fn update_edge(&self, id: &str, p: EdgePatch) -> Result<Edge> {
        if let Some(t) = &p.edge_type {
            self.check_edge_type(t)?;
        }
        let before = self.store.get_edge(id)?;
        let edge = self.store.update_edge(id, p)?;
        self.audit_edge("updated", before.as_ref(), Some(&edge))?;
        self.notify(ChangeEvent::EdgeUpdated(edge.clone()));
        // Retyping to conflicts-with is evidence arriving; resolving,
        // dismissing, or retyping away is evidence withdrawn.
        self.reconcile_conflict_demotion(&edge)?;
        // Retyping INTO the supersession verb retires the target too.
        self.retire_superseded(&edge)?;
        Ok(edge)
    }

    /// Remove one edge. Unlike node deletion this is open to Claude too —
    /// repairing a mislink must not require the pane.
    pub fn delete_edge(&self, id: &str) -> Result<bool> {
        let before = self.store.get_edge(id)?;
        let removed = self.store.delete_edge(id)?;
        if removed {
            self.audit_edge("deleted", before.as_ref(), None)?;
            self.notify(ChangeEvent::EdgeDeleted(id.to_string()));
            if let Some(b) = &before
                && b.edge_type.as_str() == self.store.config().contradiction_verb()
            {
                for endpoint in [&b.from_id, &b.to_id] {
                    self.undemote_if_unconflicted(endpoint)?;
                }
            }
        }
        Ok(removed)
    }

    pub fn edges_out(&self, id: &str) -> Result<Vec<Edge>> {
        self.store.edges_out(id)
    }

    pub fn edges_in(&self, id: &str) -> Result<Vec<Edge>> {
        self.store.edges_in(id)
    }

    pub fn list_open(&self, types: &[NodeType]) -> Result<Vec<Node>> {
        self.store.list_open(types)
    }

    /// The worklist: open Problems/Intents, plus (when `include_conflicts`)
    /// nodes sitting on an active `conflicts-with` edge — deduped by id.
    pub fn worklist(&self, types: &[NodeType], include_conflicts: bool) -> Result<Vec<Node>> {
        let mut nodes = self.store.list_open(types)?;
        if include_conflicts {
            let seen: std::collections::HashSet<String> =
                nodes.iter().map(|n| n.id.clone()).collect();
            for n in self.store.nodes_in_active_conflicts()? {
                if !seen.contains(&n.id) {
                    nodes.push(n);
                }
            }
        }
        Ok(nodes)
    }

    pub fn traverse(
        &self,
        from: &str,
        edge_types: &[EdgeType],
        depth: usize,
    ) -> Result<(Vec<Node>, Vec<Edge>)> {
        self.store.traverse(from, edge_types, depth)
    }

    /// The whole graph, for the pane's full-graph render (PLAN §8).
    pub fn graph(&self) -> Result<(Vec<Node>, Vec<Edge>)> {
        Ok((self.store.all_nodes()?, self.store.all_edges()?))
    }

    /// Export the whole graph as a portable, diffable snapshot. Nodes and edges
    /// are sorted (created_at, id), and the computed trust fields are zeroed —
    /// they're a function of "now", and a time-dependent export would never
    /// produce stable git diffs. Importers recompute trust from the timestamps.
    pub fn export(&self) -> Result<ExportGraph> {
        let mut nodes = self.store.all_nodes()?;
        let mut edges = self.store.all_edges()?;
        let key_n = |n: &Node| (n.created_at, n.id.clone());
        let key_e = |e: &Edge| (e.created_at, e.id.clone());
        nodes.sort_by_key(key_n);
        edges.sort_by_key(key_e);
        for n in &mut nodes {
            n.trust = 0.0;
            n.stale = false;
        }
        Ok(ExportGraph {
            version: EXPORT_VERSION,
            nodes,
            edges,
            // Exports embed their ontology (PLAN §7D): a customized graph's
            // dump must re-import as the same graph. Uncustomized stays bare
            // — an old dump and a new default dump mean the same thing.
            config: self.stored_graph_config(),
            tasks: {
                let mut tasks = self.store.all_tasks()?;
                tasks.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
                tasks
            },
        })
    }

    /// The stored per-graph configuration, or `None` when the graph runs on
    /// defaults. A corrupt document reads as `None` (defaults) — config must
    /// never be able to brick a store open.
    fn stored_graph_config(&self) -> Option<crate::config::GraphConfig> {
        // Corrupt documents already warn at open (GraphConfig::from_stored);
        // here only the stored-vs-defaults distinction matters (exports).
        self.store
            .graph_config()
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
    }

    /// The live configuration this graph runs on (PLAN §7D): the store's
    /// cached parse, shared — the cheap accessor every read path should use.
    pub fn config(&self) -> std::sync::Arc<crate::config::GraphConfig> {
        self.store.config()
    }

    /// Owned clone of the live configuration — only for callers that
    /// serialize or mutate it (GET /config, the rename ops).
    pub fn graph_config(&self) -> crate::config::GraphConfig {
        (*self.store.config()).clone()
    }

    /// Resolve a caller's optional brief budget: explicit wins, otherwise
    /// the graph's configured `brief.total_chars` — the one rule every
    /// surface (HTTP, MCP, CLI) shares.
    pub fn brief_chars(&self, requested: Option<usize>) -> usize {
        requested.unwrap_or_else(|| self.store.config().brief.total_chars)
    }

    /// The ontology's default durability for a node type when the caller
    /// doesn't specify one (each TypeDef carries its default; unknown types
    /// are caught by the write-boundary check, episodic here is moot).
    pub fn default_durability(&self, t: &NodeType) -> Durability {
        self.store
            .config()
            .type_def(t.as_str())
            .map(|d| d.durability)
            .unwrap_or(Durability::Episodic)
    }

    /// Replace the graph's configuration — validated against the hard
    /// invariants first; a violation is a 400, never a partial write. A
    /// config change is a user gesture (pane/HTTP only) and journals like
    /// any other mutation.
    pub fn set_graph_config(&self, cfg: &crate::config::GraphConfig) -> Result<()> {
        cfg.validate()?;
        // In-use guard: a PUT can't strand stored knowledge. Dropping a type
        // that still has nodes (or a verb that still has edges) is refused —
        // rename (bulk retype) or retype first; renames must go through
        // `rename_type`/`rename_verb`, which move the stored rows along.
        let current = self.store.config();
        let dropped_types: Vec<&str> = current
            .ontology
            .types
            .iter()
            .filter(|t| cfg.type_def(&t.name).is_none())
            .map(|t| t.name.as_str())
            .collect();
        if !dropped_types.is_empty() {
            let mut counts: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            let nodes = self.store.all_nodes()?;
            for node in &nodes {
                if let Some(name) = dropped_types
                    .iter()
                    .find(|t| **t == node.node_type.as_str())
                {
                    *counts.entry(name).or_default() += 1;
                }
            }
            if let Some((name, n)) = counts.into_iter().next() {
                return Err(crate::Error::Config(format!(
                    "type {name:?} still has {n} node(s) — rename it (bulk retype) or retype them first"
                )));
            }
        }
        let dropped_verbs: Vec<&str> = current
            .ontology
            .verbs
            .iter()
            .filter(|v| cfg.verb_def(&v.name).is_none())
            .map(|v| v.name.as_str())
            .collect();
        if !dropped_verbs.is_empty() {
            let mut counts: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            let edges = self.store.all_edges()?;
            for edge in &edges {
                if let Some(name) = dropped_verbs
                    .iter()
                    .find(|v| **v == edge.edge_type.as_str())
                {
                    *counts.entry(name).or_default() += 1;
                }
            }
            if let Some((name, n)) = counts.into_iter().next() {
                return Err(crate::Error::Config(format!(
                    "verb {name:?} still has {n} edge(s) — rename it or retype them first"
                )));
            }
        }
        let before = self.stored_graph_config().map(|c| serde_json::json!(c));
        self.store.set_graph_config(&serde_json::to_string(cfg)?)?;
        self.audit(
            "config_updated",
            "graph",
            "",
            Some(format!(
                "{} types / {} verbs, preset {}",
                cfg.ontology.types.len(),
                cfg.ontology.verbs.len(),
                cfg.ontology.preset
            )),
            before,
            Some(serde_json::json!(cfg)),
            None,
        )?;
        self.notify(ChangeEvent::ConfigChanged);
        self.sync_history_store();
        // A changed indexed-field set rewrites + re-embeds affected storage
        // (cheap no-op when the fingerprint is unchanged).
        self.ensure_field_index()?;
        Ok(())
    }

    /// Apply a shipped ontology preset (0.9.12): the preset's ONTOLOGY
    /// replaces this graph's; everything else — policy (auto-tuned lines
    /// included), brief, versioning, history and custom field definitions —
    /// is kept. Every shipped preset carries the default policy, so replacing
    /// the whole document only ever erased per-graph calibration (Caution
    /// 00e3547lopga). The same in-use guards as [`Engine::set_graph_config`]
    /// apply, and the audit row is that method's `config_updated`.
    pub fn apply_preset(&self, id: &str) -> Result<crate::config::GraphConfig> {
        let preset = crate::config::presets()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| {
                let known: Vec<String> =
                    crate::config::presets().into_iter().map(|p| p.id).collect();
                crate::Error::Config(format!(
                    "unknown preset {id:?} — known: {}",
                    known.join(", ")
                ))
            })?;
        let mut cfg = self.graph_config();
        cfg.ontology = preset.config.ontology;
        self.set_graph_config(&cfg)?;
        Ok(cfg)
    }

    /// Keep the search index in line with the graph's INDEXED custom fields
    /// (0.9.0), returning how many nodes were reprocessed. The store meta
    /// carries a fingerprint of the indexed-field set; when it drifts, every
    /// node is rewritten (re-hoisting the tepin keyword fields) and
    /// re-embedded (the composition gains/loses the `name: value` lines).
    /// Skipped under a fake embedder over a non-empty graph, like the other
    /// re-embed guards — the fingerprint stays unstamped, so the next real
    /// open finishes the job.
    pub fn ensure_field_index(&self) -> Result<usize> {
        const FP_KEY: &str = "indexed_fields_fp";
        let fp = self.store.config().indexed_field_names().join(",");
        if self.store.kv_get(FP_KEY)?.unwrap_or_default() == fp {
            return Ok(0);
        }
        let nodes = self.store.all_nodes()?;
        if self.embedder.is_fake() && !nodes.is_empty() {
            return Ok(0);
        }
        for n in &nodes {
            self.store.upsert_node(n)?;
            self.embed_node(n)?;
        }
        self.store.kv_set(FP_KEY, &fp)?;
        Ok(nodes.len())
    }

    /// Rename a custom field AND move every stored value with it — the
    /// field-migration gesture, mirroring [`Engine::rename_type`]. Returns
    /// how many nodes carried the field.
    pub fn rename_field(&self, from: &str, to: &str) -> Result<u64> {
        if from == to {
            return Err(crate::Error::Config("rename needs a new name".into()));
        }
        let mut cfg = (*self.store.config()).clone();
        let def = cfg
            .fields
            .iter_mut()
            .find(|f| f.name == from)
            .ok_or_else(|| crate::Error::Config(format!("unknown field {from:?}")))?;
        def.name = to.to_string();
        cfg.validate()?;
        // Move the values first (idempotent on re-run), then persist the
        // config — same ordering rationale as rename_type.
        let mut moved = 0u64;
        for node in self.store.all_nodes()? {
            let Some(fields) = &node.fields else { continue };
            if !fields.contains_key(from) {
                continue;
            }
            let mut next = fields.clone();
            if let Some(v) = next.remove(from) {
                next.insert(to.to_string(), v);
            }
            let mut node = node;
            node.fields = Some(next);
            self.store.upsert_node(&node)?;
            moved += 1;
        }
        self.set_graph_config(&cfg)?;
        self.audit(
            "field_renamed",
            "graph",
            "",
            Some(format!("{from} → {to} ({moved} nodes carried the field)")),
            None,
            None,
            None,
        )?;
        Ok(moved)
    }

    /// Rename a node type AND bulk-retype every stored node of it — the
    /// ontology-migration gesture (PLAN §7D). Roles, hue, brief section and
    /// durability ride along unchanged; only the name moves. Returns how
    /// many nodes followed.
    pub fn rename_type(&self, from: &str, to: &str) -> Result<u64> {
        if from == to {
            return Err(crate::Error::Config("rename needs a new name".into()));
        }
        let mut cfg = (*self.store.config()).clone();
        let def = cfg
            .ontology
            .types
            .iter_mut()
            .find(|t| t.name == from)
            .ok_or_else(|| crate::Error::Config(format!("unknown type {from:?}")))?;
        def.name = to.to_string();
        cfg.validate()?;
        // Retype first, then persist: if the config write fails the retype
        // is legal either way (a re-run is idempotent), while the reverse
        // order could strand rows under a name the config no longer knows.
        let renamed = self.store.retype_nodes(from, to)?;
        self.set_graph_config(&cfg)?;
        self.audit(
            "type_renamed",
            "graph",
            "",
            Some(format!("{from} → {to} ({renamed} nodes retyped)")),
            None,
            None,
            None,
        )?;
        Ok(renamed)
    }

    /// Rename an edge verb AND bulk-retype every stored edge of it. Role
    /// flags (supersession/contradiction/…) ride along unchanged.
    pub fn rename_verb(&self, from: &str, to: &str) -> Result<u64> {
        if from == to {
            return Err(crate::Error::Config("rename needs a new name".into()));
        }
        let mut cfg = (*self.store.config()).clone();
        let def = cfg
            .ontology
            .verbs
            .iter_mut()
            .find(|v| v.name == from)
            .ok_or_else(|| crate::Error::Config(format!("unknown verb {from:?}")))?;
        def.name = to.to_string();
        cfg.validate()?;
        let renamed = self.store.retype_edges(from, to)?;
        self.set_graph_config(&cfg)?;
        self.audit(
            "verb_renamed",
            "graph",
            "",
            Some(format!("{from} → {to} ({renamed} edges retyped)")),
            None,
            None,
            None,
        )?;
        Ok(renamed)
    }

    /// Import a snapshot: upsert nodes+edges by id in one transaction, then
    /// regenerate embeddings. Idempotent — re-importing the same graph is a
    /// no-op beyond refreshing fields. Unknown future versions are rejected.
    pub fn import(&self, graph: ExportGraph) -> Result<ImportSummary> {
        if graph.version > EXPORT_VERSION {
            return Err(crate::Error::Parse {
                kind: "export version",
                value: graph.version.to_string(),
            });
        }
        // Pre-trust-v2 exports carry last_seen but no confirmed_at; restore
        // the same backfill the schema migration applies, or every imported
        // node's trust anchor collapses to created_at and a healthy backup
        // comes back stale (and decay-eligible).
        let mut nodes = graph.nodes;
        for n in &mut nodes {
            n.confirmed_at = n.confirmed_at.or(n.last_seen);
        }
        let graph = ExportGraph { nodes, ..graph };
        self.store.import_raw(&graph.nodes, &graph.edges)?;
        for n in &graph.nodes {
            self.embed_node(n)?;
        }
        // A dump that embeds its ontology restores it — validated like any
        // config write; an invalid one fails the whole import loudly rather
        // than silently restoring a graph whose types have no definitions.
        if let Some(cfg) = &graph.config {
            self.set_graph_config(cfg)?;
        }
        for t in &graph.tasks {
            self.store.upsert_task(t)?;
        }
        let (nodes, edges) = (graph.nodes.len(), graph.edges.len());
        // One summary row: per-entity rows for a bulk restore would drown the
        // journal, and the snapshot file itself is the before/after record.
        self.audit(
            "imported",
            "graph",
            "",
            Some(format!("{nodes} nodes / {edges} edges")),
            None,
            Some(serde_json::json!({ "nodes": nodes, "edges": edges })),
            None,
        )?;
        Ok(ImportSummary { nodes, edges })
    }

    /// Hybrid retrieval: embed the query, fuse keyword + vector hits, run the
    /// precision layer when present (over-fetch candidates, cross-encode them
    /// against the query, re-order), then attach each hit's 1-hop neighbors
    /// (conflicts/supersessions first) so contradictions surface passively
    /// with the match (PLAN §6A / §7A).
    pub fn search(&self, query: &str, types: &[NodeType], limit: usize) -> Result<Vec<SearchHit>> {
        self.search_filtered(query, types, limit, &SearchFilter::default())
    }

    /// [`Engine::search`] with a time window and an explicit ordering (0.8.7).
    ///
    /// The window prunes candidates BEFORE the reranker and before both
    /// calibrated cuts, so the delivery floor, the knee trim and the verdict
    /// all describe the scoped set — a "strong" verdict means the best answer
    /// *inside the window* cleared the line, not that something outside it
    /// did.
    ///
    /// The ordering is applied LAST, after every cut. Re-sorting a delivered
    /// set cannot change what cleared the line, so a chronological read still
    /// carries the same verdict its relevance-ordered twin would.
    pub fn search_filtered(
        &self,
        query: &str,
        types: &[NodeType],
        limit: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<SearchHit>> {
        for t in types {
            self.check_node_type(t)?;
        }
        self.check_clock(filter)?;
        let qv = self.embedder.embed_one(query)?;
        let fetch = match &self.reranker {
            Some(_) => (limit * 3).clamp(12, 50),
            None => limit,
        };
        let mut hits = self.store.search_hybrid(
            query,
            Some(&qv),
            types,
            fetch,
            filter.window,
            &filter.clock,
        )?;
        if let Some(reranker) = &self.reranker
            && !hits.is_empty()
        {
            // Even a single hit goes through: the delivery floor and the
            // search verdict both read the cross-encoder's calibrated scale,
            // so an unreranked one-hit result would be judged on the wrong
            // ruler.
            self.rerank(reranker.as_ref(), query, &mut hits);
            // Calibrated delivery, trim face: hits under the floor are tail
            // by score even when the vote ranked them mid-list — measured
            // free of recall cost (policy::DELIVERY_FLOOR), and every token
            // they would have carried is attention the answer keeps.
            let floor = self.store.config().policy.delivery_floor;
            hits.retain(|h| h.score >= floor);
        }
        // Session-diverse delivery: when the floor-surviving pool is deeper
        // than the limit, a session already holding a delivered slot pays a
        // rank demotion for each further one, so restatements from one
        // session stop crowding out complementary evidence from others. With
        // the knob at 0 this IS `truncate(limit)`.
        session_diverse_select(
            &mut hits,
            limit,
            self.store.config().policy.session_diversity_demote,
            |h| h.session_id.as_deref(),
        );
        // Calibrated delivery, knee face: cut at the largest relative drop
        // in the delivered score curve — the cliff between the relevance
        // head and the noise tail (policy::KNEE_MIN_CLIFF). Measured free of
        // recall cost at every size, and unlike the fixed floor the cliff
        // sharpens as the graph grows. Reranker-gated like the floor: the
        // fused hybrid score is a different scale.
        if self.reranker.is_some()
            && let Some(cliff) = self.store.config().policy.knee_cliff
        {
            let mut curve: Vec<f64> = hits.iter().map(|h| h.score).collect();
            curve.sort_by(|a, b| b.total_cmp(a));
            if let Some(k) = knee_floor(&curve, cliff) {
                hits.retain(|h| h.score >= k);
            }
        }
        if let Some(band) = self.store.config().policy.twin_trust_order {
            self.order_twins(&mut hits, band)?;
        }
        for hit in &mut hits {
            hit.neighbors = self.store.neighbors(&hit.id, NEIGHBOR_CAP)?;
        }
        // Role, not type name: a custom ontology can call its tombstone
        // anything, and a reader skimming for X must not take "Removed: X"
        // for a memory of X — the flag rides on every surface (0.9.4).
        let cfg = self.store.config();
        for hit in &mut hits {
            hit.tombstone = cfg
                .type_def(hit.node_type.as_str())
                .is_some_and(|t| t.roles.tombstone);
            hit.standing = Some(self.standing_of(&hit.id, hit.tombstone, hit.stale)?);
        }
        order_hits(&mut hits, filter.order);
        // Observability stamp on what was actually returned — never the
        // over-fetched candidates the reranker discarded. (Trust doesn't
        // read this either way; see policy.)
        let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        self.store.touch(&ids)?;
        Ok(hits)
    }

    /// A node's grounded-answer verdict (0.9.10) — the same `standing` a
    /// search hit carries, for surfaces that show one node (the pane's
    /// inspector). `None` when the node does not exist.
    pub fn node_standing(&self, id: &str) -> Result<Option<Standing>> {
        let Some(node) = self.store.get_node(id)? else {
            return Ok(None);
        };
        let tombstone = is_tombstone(&self.store.config(), &node);
        self.standing_of(id, tombstone, node.stale).map(Some)
    }

    /// Grounded answers (0.9.10): how far a hit can be relied on, composed
    /// from what the graph already records — the tombstone role, a live
    /// `replaces` onto it, an open judged conflict, staleness, and the trust
    /// ladder. The first that applies wins, worst first. Reads the full edge
    /// lists, not the capped neighbour sample, so no conflict hides behind
    /// the cap.
    fn standing_of(&self, id: &str, tombstone: bool, stale: bool) -> Result<Standing> {
        let standing = |verdict: &str, reason: String, other: Option<String>| Standing {
            verdict: verdict.to_string(),
            reason,
            other,
        };
        if tombstone {
            return Ok(standing(
                "tombstone",
                "a record that this knowledge was deliberately removed — don't act on it or re-add it"
                    .into(),
                None,
            ));
        }
        let cfg = self.store.config();
        let live =
            |e: &Edge| !matches!(e.status, Some(EdgeStatus::Resolved | EdgeStatus::Dismissed));
        let edges_in = self.store.edges_in(id)?;
        for e in edges_in.iter().filter(|e| live(e)) {
            if e.edge_type.as_str() == cfg.supersession_verb()
                && let Some(newer) = self.store.get_node(&e.from_id)?
                && newer.valid_until.is_none()
            {
                return Ok(standing(
                    "superseded",
                    format!("replaced by \"{}\" — read that instead", newer.title),
                    Some(newer.id),
                ));
            }
        }
        let edges_out = self.store.edges_out(id)?;
        for e in edges_in.iter().chain(&edges_out).filter(|e| live(e)) {
            if e.edge_type.as_str() != cfg.contradiction_verb() {
                continue;
            }
            let other_id = if e.from_id == id {
                &e.to_id
            } else {
                &e.from_id
            };
            if let Some(other) = self.store.get_node(other_id)?
                && other.valid_until.is_none()
            {
                return Ok(standing(
                    "contested",
                    format!(
                        "in an open judged conflict with \"{}\" — tell the user before relying on either",
                        other.title
                    ),
                    Some(other.id),
                ));
            }
        }
        if stale {
            return Ok(standing(
                "stale",
                "its trust fell below the stale line — verify against the code or the user first"
                    .into(),
                None,
            ));
        }
        let Some(node) = self.store.get_node(id)? else {
            return Ok(standing(
                "unverified",
                "not found in the store".into(),
                None,
            ));
        };
        Ok(if node.trust_override.is_some() {
            standing("canon", "pinned by the user".into(), None)
        } else if node.approved_at.is_some() {
            standing(
                "canon",
                if node.source == Source::User {
                    "written by the user".into()
                } else {
                    "approved by the user".into()
                },
                None,
            )
        } else if node.confirmed_at.is_some() {
            standing(
                "confirmed",
                "confirmed still true since it was written".into(),
                None,
            )
        } else {
            standing(
                "unverified",
                "written by an assistant and never confirmed — a lead, not a fact".into(),
                None,
            )
        })
    }

    /// Twin order (0.9.10): delivered hits whose vectors sit at or above
    /// `band` of each other are near-identical notes, and among them the one
    /// people endorsed goes first: pinned, then higher trust, then — between
    /// twins on the same rung that trust cannot tell apart — the one endorsed
    /// most recently (the endorsement clock). Each group of
    /// twins is re-sorted within the positions it already holds, so nothing
    /// that isn't a twin moves, and nothing enters or leaves the delivered
    /// set. The vote otherwise decides between two near-identical notes on
    /// capture stamps and type priors, which are the same size as the trust
    /// bonus (KnowledgeDrift authority family, 2026-09-14).
    fn order_twins(&self, hits: &mut [SearchHit], band: f64) -> Result<()> {
        let n = hits.len();
        if n < 2 {
            return Ok(());
        }
        let mut vecs = Vec::with_capacity(n);
        let mut nodes = Vec::with_capacity(n);
        for h in hits.iter() {
            vecs.push(self.store.embedding_of(&h.id)?);
            nodes.push(self.store.get_node(&h.id)?);
        }
        let rung = |i: usize| nodes[i].as_ref().map_or(Rung::None, Rung::of);
        // Union-find over the twin relation, so a chain of twins is one group.
        let mut group: Vec<usize> = (0..n).collect();
        fn root(g: &mut [usize], mut i: usize) -> usize {
            while g[i] != i {
                g[i] = g[g[i]];
                i = g[i];
            }
            i
        }
        for i in 0..n {
            for j in (i + 1)..n {
                if let (Some(a), Some(b)) = (&vecs[i], &vecs[j])
                    && cosine(a, b) >= band
                {
                    let (ri, rj) = (root(&mut group, i), root(&mut group, j));
                    group[rj] = ri;
                }
            }
        }
        let mut members: std::collections::HashMap<usize, Vec<usize>> =
            std::collections::HashMap::new();
        for i in 0..n {
            let r = root(&mut group, i);
            members.entry(r).or_default().push(i);
        }
        let original: Vec<SearchHit> = hits.to_vec();
        for slots in members.values().filter(|m| m.len() > 1) {
            let mut ranked = slots.clone();
            // The endorsement clock is read only for twins that share a rung
            // — one journal lookup each, and only in a twin group.
            let mut clock = std::collections::HashMap::new();
            for &i in slots {
                if let Some(node) = &nodes[i] {
                    clock.insert(i, self.endorsement_clock(node, rung(i))?);
                }
            }
            // Stable: equal standing keeps the vote's order.
            ranked.sort_by(|a, b| {
                let pinned = |i: usize| rung(i) == Rung::Pinned;
                pinned(*b)
                    .cmp(&pinned(*a))
                    .then(original[*b].trust.total_cmp(&original[*a].trust))
                    .then_with(|| {
                        if rung(*a) != rung(*b) {
                            return std::cmp::Ordering::Equal;
                        }
                        clock.get(b).cmp(&clock.get(a))
                    })
            });
            for (slot, from) in slots.iter().zip(ranked) {
                hits[*slot] = original[from].clone();
            }
        }
        Ok(())
    }

    /// When `node` last received the endorsement its rung names, as
    /// (stamp, journal seq): the seq breaks same-second ties. Pins keep no
    /// stamp on the node, so theirs is the newest `pinned` journal row —
    /// which also dates pins made before this existed. Approvals and
    /// confirmations read their node stamp, seq from the matching row
    /// (`approved`; `updated`, which is what stamps `confirmed_at`).
    fn endorsement_clock(&self, node: &Node, rung: Rung) -> Result<(i64, i64)> {
        let (stamp, action) = match rung {
            Rung::None => return Ok((0, 0)),
            Rung::Pinned => (None, "pinned"),
            Rung::Approved => (node.approved_at, "approved"),
            Rung::Confirmed => (node.confirmed_at, "updated"),
        };
        let newest = self
            .store
            .audit_rows(&AuditQuery {
                entity_id: Some(node.id.clone()),
                action: Some(action.to_string()),
                ..Default::default()
            })?
            .pop();
        let seq = newest.as_ref().map_or(0, |r| r.seq);
        let stamp = stamp.or(newest.map(|r| r.ts)).unwrap_or(0);
        Ok((stamp, seq))
    }

    /// The half-open window a named working version was current for, read from
    /// the audit journal's `version_switched` rows (`set_current_version`
    /// journals every switch under entity_id "version").
    ///
    /// The window OPENS when the version was switched to and CLOSES when it
    /// was switched away from — the still-current version has no end. A
    /// version that was set more than once (a cycle reopened, a bump reverted)
    /// spans from its first switch-in to the last switch-away, which is the
    /// reading that keeps "during 0.8.4" meaning the same thing as the
    /// release's story.
    ///
    /// `None` when the journal never mentions the version: the caller must
    /// say so rather than silently searching all of time.
    pub fn version_window(&self, version: &str) -> Result<Option<crate::timespec::TimeWindow>> {
        let version = version.trim();
        if version.is_empty() {
            return Ok(None);
        }
        // The journal is paged newest-first; version switches are rare enough
        // that one deep page holds a project's whole history.
        let page = self.store.audit_page(None, Some("version"), 4096)?;
        let (mut after, mut before) = (None::<i64>, None::<i64>);
        for row in &page.entries {
            if row.action != "version_switched" {
                continue;
            }
            // The row's title is "<previous> → <next>".
            let Some((prev, next)) = row.title.as_deref().and_then(|t| t.split_once('→')) else {
                continue;
            };
            let (prev, next) = (prev.trim(), next.trim());
            if next == version {
                after = Some(after.map_or(row.ts, |a: i64| a.min(row.ts)));
            }
            if prev == version {
                before = Some(before.map_or(row.ts, |b: i64| b.max(row.ts)));
            }
        }
        // Switched away from but never switched to: the graph was already on
        // that version before journaling began. Everything up to the switch
        // away is the honest window.
        if after.is_none() && before.is_none() {
            return Ok(None);
        }
        Ok(Some(crate::timespec::TimeWindow { after, before }))
    }

    /// Resolve the temporal arguments a caller passed into one filter,
    /// anchored at the current clock. `during_version` contributes the
    /// release's window; explicit `after`/`before` INTERSECT it rather than
    /// replacing it, so "during 0.8.4, after the 10th" narrows as it reads.
    pub fn time_filter(
        &self,
        after: Option<&str>,
        before: Option<&str>,
        during_version: Option<&str>,
        order: Option<&str>,
    ) -> Result<SearchFilter> {
        self.time_filter_clocked(after, before, during_version, order, None)
    }

    /// [`Engine::time_filter`] with the bitemporal clock selector (0.9.0):
    /// `date_field` re-aims the window at a date-kind custom field, or a
    /// `from..to` pair with interval-overlap semantics. Validated here so an
    /// unreadable selector is an ERROR, never a dropped filter.
    pub fn time_filter_clocked(
        &self,
        after: Option<&str>,
        before: Option<&str>,
        during_version: Option<&str>,
        order: Option<&str>,
        date_field: Option<&str>,
    ) -> Result<SearchFilter> {
        let mut window = crate::timespec::window(after, before, crate::store::now())?;
        if let Some(v) = during_version.map(str::trim).filter(|v| !v.is_empty()) {
            let Some(release) = self.version_window(v)? else {
                return Err(crate::Error::Config(format!(
                    "no recorded switch to or from version {v:?} — `set_version` \
                     journals each switch, so only versions this graph was \
                     actually worked under can be searched by name"
                )));
            };
            window = window.intersect(release);
            if window.is_empty() {
                return Err(crate::Error::Config(format!(
                    "the window is empty: the given after/before falls outside \
                     the time version {v:?} was current"
                )));
            }
        }
        let order = match order.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => crate::timespec::SearchOrder::parse(s).ok_or_else(|| {
                crate::Error::Config(format!(
                    "order {s:?} must be relevance, chronological or recent"
                ))
            })?,
            None => crate::timespec::SearchOrder::default(),
        };
        let clock = match date_field.map(str::trim).filter(|s| !s.is_empty()) {
            Some(sel) => crate::timespec::TimeClock::parse_selector(sel).ok_or_else(|| {
                crate::Error::Config(format!(
                    "date_field {sel:?} must be a field name or a from..to pair"
                ))
            })?,
            None => crate::timespec::TimeClock::CreatedAt,
        };
        Ok(SearchFilter {
            window,
            order,
            clock,
        })
    }

    /// Calibrated delivery, verdict face: how the assistant should hold a
    /// search result. `"none"` — the graph is silent, say so instead of
    /// inventing. `"weak"` — delivered, but nothing cleared the calibrated
    /// confidence line (`policy::WEAK_EVIDENCE_TOP`): as likely topical
    /// adjacency as an answer, verify before relying. `"strong"` — the top
    /// hit cleared the line. Returns `None` for a non-empty result when no
    /// reranker is loaded: the fused hybrid score is a different scale, and
    /// a verdict from an uncalibrated ruler would be noise wearing a badge.
    /// A label rather than a harder floor by measurement: hard abstention
    /// costs oblique recall (see `policy::DELIVERY_FLOOR`), a label costs
    /// nothing and keeps the assistant aligned.
    pub fn search_confidence(&self, hits: &[SearchHit]) -> Option<&'static str> {
        if hits.is_empty() {
            return Some("none");
        }
        self.reranker.as_ref()?;
        let top = hits.iter().map(|h| h.score).fold(f64::MIN, f64::max);
        Some(if top < self.store.config().policy.weak_evidence_top {
            "weak"
        } else {
            "strong"
        })
    }

    /// Re-score candidates with the cross-encoder: relevance comes from the
    /// reranker logit (sigmoid-squashed), trust modulates it the same way it
    /// modulates the hybrid blend — relevance dominates, trust breaks ties
    /// (PLAN §6A). A reranker failure keeps hybrid order: precision is an
    /// upgrade, never a dependency.
    ///
    /// With `rerank_vote_k` set (the default), the cross-encoder does not get
    /// the final word: its ordering is combined with the retrieval ordering by
    /// a reciprocal-rank vote, so a hit two independent channels ranked highly
    /// cannot be buried by one confident cross-encoder mistake.
    ///
    /// The *reported* score stays the cross-encoder's calibrated relevance
    /// either way. The vote decides order only — reciprocal-rank sums encode
    /// rank, not confidence, and `score` is surfaced to assistants over MCP and
    /// to the pane, where a number that no longer means "how good is this
    /// match" would quietly mislead both.
    fn rerank(&self, reranker: &dyn Reranker, query: &str, hits: &mut [SearchHit]) {
        // `rerank_full_note` scores `title + whole body` instead of `title +
        // keyword-window snippet`: notes fit the cross-encoder's window whole,
        // and on an oblique query the evidence sentence often shares no
        // keyword with the query — it never enters the window at all. The
        // snippet stays the delivered text either way; this changes only what
        // the judge reads.
        let full_note = self.config().policy.rerank_full_note;
        let docs: Vec<String> = hits
            .iter()
            .map(|h| {
                if full_note
                    && let Ok(Some(node)) = self.store.get_node(&h.id)
                    && let Some(body) = node.body.as_deref().filter(|b| !b.is_empty())
                {
                    return format!("{}\n{}", h.title, body);
                }
                let snippet = h.snippet.replace(
                    [crate::store::SNIPPET_OPEN, crate::store::SNIPPET_CLOSE],
                    "",
                );
                format!("{}\n{}", h.title, snippet)
            })
            .collect();
        let Ok(scores) = reranker.rank(query, &docs) else {
            return;
        };
        if scores.len() != hits.len() {
            return;
        }
        let config = self.config();
        let trust_weight = config.policy.rerank_trust_weight;

        // `hits` arrives in retrieval order, so a hit's index IS its incoming
        // rank — captured before anything re-scores or re-sorts.
        let retrieval_rank: Vec<usize> = (1..=hits.len()).collect();
        for (hit, logit) in hits.iter_mut().zip(&scores) {
            let relevance = 1.0 / (1.0 + (-*logit as f64).exp());
            hit.score = relevance * (1.0 + trust_weight * hit.trust);
        }

        let Some(k) = config.policy.rerank_vote_k else {
            hits.sort_by(|a, b| b.score.total_cmp(&a.score));
            return;
        };

        let mut by_rerank: Vec<usize> = (0..hits.len()).collect();
        by_rerank.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
        let mut vote = vec![0.0_f64; hits.len()];
        for (rank, idx) in by_rerank.into_iter().enumerate() {
            vote[idx] = 1.0 / (k + retrieval_rank[idx] as f64) + 1.0 / (k + (rank + 1) as f64);
        }
        let mut order: Vec<usize> = (0..hits.len()).collect();
        order.sort_by(|a, b| vote[*b].total_cmp(&vote[*a]).then(a.cmp(b)));
        let reordered: Vec<SearchHit> = order.into_iter().map(|i| hits[i].clone()).collect();
        hits.clone_from_slice(&reordered);
    }

    /// Claude-side note write with the PLAN §6A safety net: if a same-type,
    /// still-current node sits at/above the duplicate-similarity threshold,
    /// return it instead of creating — the caller merges via `update_node`.
    /// Created notes carry warnings when they land near contradicted or
    /// superseded knowledge (see `write_warnings`).
    ///
    /// Front-ends sharing the engine behind a mutex call
    /// [`Engine::add_node_checked_shared`] instead — same verdict, judged
    /// with the lock released.
    pub fn add_node_checked(&self, n: NewNode) -> Result<WriteOutcome> {
        let nli = self.nli.clone();
        match self.plan_add(n, None)? {
            WritePlan::Matched(m) => Ok(m.judge(nli.as_deref())),
            WritePlan::Created(p) => {
                let judged = p.judge(nli.as_deref(), &self.config());
                self.finish_created(judged)
            }
        }
    }

    /// [`Engine::add_node_checked`] over a shared engine (the MCP tools): the
    /// write and the pairs it must judge are gathered under one lock, the
    /// logic layer judges them with the lock RELEASED, and a second short
    /// lock queues what it nominated. A write judges up to ~14 pairs
    /// (duplicate, suspects, canon); with a heavy judge — Laya, or any NLI
    /// on a low-tier machine — that is seconds, and holding the lock for it
    /// parked every other request on the graph (0.9.10; the sweep got the
    /// same fix in 0.9.9). `origin` is re-stamped under each lock: another
    /// front-end may stamp its own in between.
    pub fn add_node_checked_shared(
        engine: &std::sync::Mutex<Engine>,
        origin: AuditOrigin,
        n: NewNode,
    ) -> Result<WriteOutcome> {
        // Embedding off the lock (0.9.11): a moment under it to learn the
        // model and the graph's config, the vectors computed outside it.
        let cache = {
            let (embedder, model, cfg) = {
                let e = lock_engine(engine)?;
                (e.embedder.clone(), e.embed_model_id(), e.config())
            };
            EmbedCache::for_add(embedder.as_ref(), model, &cfg, &n)
        };
        let (nli, cfg, plan) = {
            let mut e = lock_engine(engine)?;
            e.set_audit_origin(origin.clone());
            (e.nli.clone(), e.config(), e.plan_add(n, cache.as_ref())?)
        };
        match plan {
            WritePlan::Matched(m) => Ok(m.judge(nli.as_deref())),
            WritePlan::Created(p) => {
                let judged = p.judge(nli.as_deref(), &cfg);
                let mut e = lock_engine(engine)?;
                e.set_audit_origin(origin);
                e.finish_created(judged)
            }
        }
    }

    /// The locked first half of a checked write: embed (or take the vectors
    /// `cache` computed before the lock), short-circuit to a near-duplicate,
    /// or create and gather everything the judge will read.
    fn plan_add(&self, n: NewNode, cache: Option<&EmbedCache>) -> Result<WritePlan> {
        let scrubbed_title = crate::redact::scrub(&n.title);
        let scrubbed_body = n.body.as_deref().map(crate::redact::scrub);
        let vec = self.embed_one_with(
            &embed_text(
                &scrubbed_title,
                scrubbed_body.as_deref(),
                &n.tags,
                &n.code_refs,
            ),
            cache,
        )?;

        let duplicate_similarity = self.store.config().policy.duplicate_similarity;
        for (id, distance) in self.store.search_vec(&vec, WRITE_CHECK_K)? {
            let similarity = 1.0 - distance;
            if similarity < duplicate_similarity {
                break; // results are distance-ordered; nothing closer follows
            }
            if let Some(node) = self.store.get_node(&id)?
                && node.node_type == n.node_type
                && node.valid_until.is_none()
                // Two titles that demonstrably name different things are
                // not a duplicate whatever the vectors say (0.9.6): on a
                // corpus whose bodies are mostly shared boilerplate, a
                // Caution about one component matched a Caution about
                // another at 0.90 and swallowed a resurrection the
                // tombstone channel was there to catch (KnowledgeDrift v2,
                // 6 of 42 released victims re-added as "duplicates" of
                // unrelated notes). Undecidable titles keep matching.
                && !subjects_differ(&scrubbed_title, &node.title)
            {
                let text = match &scrubbed_body {
                    Some(b) => format!("{scrubbed_title}. {b}"),
                    None => scrubbed_title.clone(),
                };
                return Ok(WritePlan::Matched(Box::new(MatchedPlan {
                    node,
                    similarity,
                    excerpt: text.chars().take(400).collect(),
                })));
            }
        }

        let missing_refs = self.missing_refs(&n.code_refs);
        let node = self.add_node_opts(n, true, cache)?;
        Ok(WritePlan::Created(Box::new(self.plan_created(
            node,
            vec,
            missing_refs,
        )?)))
    }

    /// What a fresh text's checks need from the store, read under the lock:
    /// the warnings (store-only, final here) and the pairs to judge.
    fn plan_created(
        &self,
        node: Node,
        vec: Vec<f32>,
        missing_refs: Vec<String>,
    ) -> Result<CreatedPlan> {
        let near = self.nearby(&vec, &node.id)?;
        let warnings = self.write_warnings(&near, &node.id, &node.title)?;
        let suspects = self.suspect_candidates(&node, &near)?;
        let canon = self.canon_candidates(&near)?;
        Ok(CreatedPlan {
            node,
            warnings,
            missing_refs,
            suspects,
            canon,
        })
    }

    /// The locked last half of a checked write: queue what the judge
    /// nominated and assemble the verdict.
    fn finish_created(&self, j: JudgedPlan) -> Result<WriteOutcome> {
        let suspects = self.queue_judged(&j.node, j.suspects)?;
        Ok(WriteOutcome::Created {
            node: j.node,
            warnings: j.warnings,
            suspects,
            missing_refs: j.missing_refs,
            canon: j.canon,
        })
    }

    /// Queue judged suspects near `node`; returns their judgeable views.
    fn queue_judged(&self, node: &Node, judged: Vec<JudgedSuspect>) -> Result<Vec<SuspectView>> {
        if self.queue_suspects(node, judged)? == 0 {
            return Ok(Vec::new());
        }
        self.notify(ChangeEvent::SuspectsChanged);
        self.suspects_involving(&node.id)
    }

    /// The nearest current neighbourhood of a fresh text, read once for
    /// every check that walks it (0.9.10): the warnings, the suspect
    /// candidates and the canon candidates each used to run the same
    /// `WRITE_CHECK_K` search and re-read the same nodes under the lock.
    /// Distance-ordered, `exclude_id` (the written note itself) left out, a
    /// neighbour whose node is gone skipped — what each check did on its
    /// own.
    fn nearby(&self, vec: &[f32], exclude_id: &str) -> Result<Vec<(Node, f64)>> {
        let mut out = Vec::new();
        for (id, distance) in self.store.search_vec(vec, WRITE_CHECK_K)? {
            if id == exclude_id {
                continue;
            }
            if let Some(node) = self.store.get_node(&id)? {
                out.push((node, 1.0 - distance));
            }
        }
        Ok(out)
    }

    /// The write-time canon check's candidates (PLAN §7A): the nearest
    /// current knowledge above the warn line, capped — judged by
    /// [`judge_canon`]. Empty without the logic layer.
    fn canon_candidates(&self, near: &[(Node, f64)]) -> Result<Vec<(Node, f64)>> {
        if self.nli.is_none() {
            return Ok(Vec::new());
        }
        let cfg = self.store.config();
        let mut out = Vec::new();
        for (node, similarity) in near {
            let (node, similarity) = (node.clone(), *similarity);
            if similarity < cfg.policy.warn_similarity {
                break; // distance-ordered: nothing closer follows
            }
            if out.len() >= CANON_CHECK_CAP {
                break;
            }
            // Tombstones are skipped here on purpose: "Removed: X" against
            // "X" is not a pair the NLI reads reliably, and the write already
            // carries a `tombstoned` warning for the same neighbor — one
            // channel per fact, no double report.
            if node.valid_until.is_some() || is_anchor(&cfg, &node) || is_tombstone(&cfg, &node) {
                continue;
            }
            out.push((node, similarity));
        }
        Ok(out)
    }

    /// `update_node` plus conflict warnings and freshly-queued suspects when
    /// any embedded field changed. Shared front-ends call
    /// [`Engine::update_node_checked_shared`].
    pub fn update_node_checked(&self, id: &str, patch: NodePatch) -> Result<CheckedUpdate> {
        let nli = self.nli.clone();
        let judged = self
            .plan_update(id, patch, None)?
            .judge(nli.as_deref(), &self.config());
        self.finish_update(judged)
    }

    /// [`Engine::update_node_checked`] judged with the lock released — see
    /// [`Engine::add_node_checked_shared`].
    pub fn update_node_checked_shared(
        engine: &std::sync::Mutex<Engine>,
        origin: AuditOrigin,
        id: &str,
        patch: NodePatch,
    ) -> Result<CheckedUpdate> {
        // Embedding off the lock, as on add: the texts are predicted from
        // the stored node as the patch will leave it; if the node changes
        // in between, the misses are embedded under the lock.
        let cache = {
            let (embedder, model, cfg, before) = {
                let e = lock_engine(engine)?;
                (
                    e.embedder.clone(),
                    e.embed_model_id(),
                    e.config(),
                    e.store.get_node(id)?,
                )
            };
            before.and_then(|b| EmbedCache::for_update(embedder.as_ref(), model, &cfg, &b, &patch))
        };
        let (nli, cfg, plan) = {
            let mut e = lock_engine(engine)?;
            e.set_audit_origin(origin.clone());
            (
                e.nli.clone(),
                e.config(),
                e.plan_update(id, patch, cache.as_ref())?,
            )
        };
        let judged = plan.judge(nli.as_deref(), &cfg);
        let mut e = lock_engine(engine)?;
        e.set_audit_origin(origin);
        e.finish_update(judged)
    }

    /// The locked first half of a checked update. An update that touches no
    /// embedded field has nothing to check: its plan carries no pairs.
    fn plan_update(
        &self,
        id: &str,
        patch: NodePatch,
        cache: Option<&EmbedCache>,
    ) -> Result<CreatedPlan> {
        let touches_text = patch.title.is_some()
            || patch.body.is_some()
            || patch.tags.is_some()
            || patch.code_refs.is_some();
        let node = self.update_node_with(id, patch, cache)?;
        let missing_refs = self.missing_refs(&node.code_refs);
        if !touches_text {
            return Ok(CreatedPlan {
                node,
                warnings: Vec::new(),
                missing_refs,
                suspects: Vec::new(),
                canon: Vec::new(),
            });
        }
        let vec = self.embed_one_with(
            &embed_text(
                &node.title,
                node.body.as_deref(),
                &node.tags,
                &node.code_refs,
            ),
            cache,
        )?;
        self.plan_created(node, vec, missing_refs)
    }

    fn finish_update(&self, j: JudgedPlan) -> Result<CheckedUpdate> {
        let suspects = self.queue_judged(&j.node, j.suspects)?;
        Ok(CheckedUpdate {
            node: j.node,
            warnings: j.warnings,
            suspects,
            missing_refs: j.missing_refs,
            canon: j.canon,
        })
    }

    /// Merge duplicate nodes into one survivor — the "merge via
    /// `update_node`" guidance made first-class. Tags and code_refs union
    /// onto the survivor, the victims' live edges rehome onto it, and each
    /// victim is superseded behind a `replaces` edge so its generation stays
    /// traversable. A merge is convergence of records that say the same
    /// thing — nothing is judged wrong and nothing is deleted.
    ///
    /// Rehoming rules: dead edges and edges internal to the merged set stay
    /// on their victim (they are its story); an edge whose (verb, far
    /// endpoint, direction) already lives on the survivor is skipped rather
    /// than duplicated; incoming supersession edges never move (something
    /// replaced the *victim's* generation, not the survivor's). A rehomed
    /// edge keeps its id and timestamps — the connection moved, it didn't
    /// recur. A rehomed contradiction re-runs demotion reconciliation: the
    /// survivor genuinely inherits a live conflict.
    ///
    /// Pinned victims are refused for the assistant (surface to the user)
    /// and archived explicitly for a user-sourced merge — the same contract
    /// as [`Engine::resolve_suspect`]. Not atomic: every step journals
    /// individually, and a failure part-way leaves a smaller, still-valid
    /// merge.
    pub fn merge_nodes(
        &self,
        survivor_id: &str,
        victims: &[String],
        title: Option<String>,
        body: Option<String>,
        source: Source,
    ) -> Result<MergeOutcome> {
        // One operation in the journal: undo reverts all of it (0.9.10).
        let _op = self.op_scope();
        let survivor = self
            .store
            .get_node(survivor_id)?
            .ok_or_else(|| crate::Error::NotFound(survivor_id.to_string()))?;
        if survivor.valid_until.is_some() {
            return Err(crate::Error::Parse {
                kind: "merge",
                value: format!(
                    "survivor {survivor_id} is archived — merge into the live generation"
                ),
            });
        }
        let mut victim_nodes: Vec<Node> = Vec::new();
        for id in victims {
            if id == survivor_id || victim_nodes.iter().any(|v| &v.id == id) {
                continue;
            }
            let node = self
                .store
                .get_node(id)?
                .ok_or_else(|| crate::Error::NotFound(id.clone()))?;
            if source == Source::Agent && node.trust_override.is_some() {
                return Err(crate::Error::Pinned(format!(
                    "\"{}\" ({}) is user-pinned; merging would archive it — \
                     tell the user and let them merge this pair in the pane",
                    node.title, node.id
                )));
            }
            victim_nodes.push(node);
        }
        if victim_nodes.is_empty() {
            return Err(crate::Error::Parse {
                kind: "merge",
                value: "no victims to merge (the survivor itself doesn't count)".into(),
            });
        }
        let victim_ids: Vec<String> = victim_nodes.iter().map(|v| v.id.clone()).collect();

        // The union the survivor will carry.
        let mut tags = survivor.tags.clone();
        let mut code_refs = survivor.code_refs.clone();
        for v in &victim_nodes {
            for t in &v.tags {
                if !tags.contains(t) {
                    tags.push(t.clone());
                }
            }
            for r in &v.code_refs {
                if !code_refs.contains(r) {
                    code_refs.push(r.clone());
                }
            }
        }

        // The survivor's live (verb, far-endpoint) pairs, per direction —
        // grown as edges rehome so two victims can't both move the same link.
        let live = |e: &Edge| e.valid_until.is_none();
        let mut out_keys: std::collections::HashSet<(String, String)> = self
            .store
            .edges_out(survivor_id)?
            .iter()
            .filter(|e| live(e))
            .map(|e| (e.edge_type.as_str().to_string(), e.to_id.clone()))
            .collect();
        let mut in_keys: std::collections::HashSet<(String, String)> = self
            .store
            .edges_in(survivor_id)?
            .iter()
            .filter(|e| live(e))
            .map(|e| (e.edge_type.as_str().to_string(), e.from_id.clone()))
            .collect();

        let supersession = self.store.config().supersession_verb().to_string();
        let mut merged = Vec::new();
        for victim in &victim_nodes {
            let mut rehomed_edges = 0usize;
            let mut skipped_edges = 0usize;
            for edge in self.store.edges_out(&victim.id)? {
                if !live(&edge) {
                    continue;
                }
                let key = (edge.edge_type.as_str().to_string(), edge.to_id.clone());
                if edge.to_id == survivor_id
                    || victim_ids.contains(&edge.to_id)
                    || out_keys.contains(&key)
                {
                    skipped_edges += 1;
                    continue;
                }
                let before = edge.clone();
                let mut moved = edge;
                moved.from_id = survivor_id.to_string();
                self.store.upsert_edge(&moved)?;
                self.audit_edge("updated", Some(&before), Some(&moved))?;
                self.notify(ChangeEvent::EdgeUpdated(moved.clone()));
                self.reconcile_conflict_demotion(&moved)?;
                self.retire_superseded(&moved)?;
                out_keys.insert(key);
                rehomed_edges += 1;
            }
            for edge in self.store.edges_in(&victim.id)? {
                if !live(&edge) {
                    continue;
                }
                let key = (edge.edge_type.as_str().to_string(), edge.from_id.clone());
                if edge.edge_type.as_str() == supersession
                    || edge.from_id == survivor_id
                    || victim_ids.contains(&edge.from_id)
                    || in_keys.contains(&key)
                {
                    skipped_edges += 1;
                    continue;
                }
                let before = edge.clone();
                let mut moved = edge;
                moved.to_id = survivor_id.to_string();
                self.store.upsert_edge(&moved)?;
                self.audit_edge("updated", Some(&before), Some(&moved))?;
                self.notify(ChangeEvent::EdgeUpdated(moved.clone()));
                self.reconcile_conflict_demotion(&moved)?;
                in_keys.insert(key);
                rehomed_edges += 1;
            }
            // The story edge — skipped when a previous merge already wrote it.
            let replaces_key = (supersession.clone(), victim.id.clone());
            if !out_keys.contains(&replaces_key) {
                self.add_edge(NewEdge {
                    edge_type: self.store.config().supersession_edge(),
                    from_id: survivor_id.to_string(),
                    to_id: victim.id.clone(),
                    source,
                    note: Some("merged".into()),
                    confidence: None,
                    strength: None,
                    status: None,
                })?;
                out_keys.insert(replaces_key);
            }
            // add_edge retired the victim unless it is pinned; a USER merge
            // overrides the pin (the assistant case errored above).
            if let Some(current) = self.store.get_node(&victim.id)?
                && current.valid_until.is_none()
            {
                self.update_node(
                    &victim.id,
                    NodePatch {
                        valid_until: Some(crate::store::now()),
                        ..NodePatch::default()
                    },
                )?;
            }
            let after = self.store.get_node(&victim.id)?;
            self.audit_node("merged", Some(victim), after.as_ref())?;
            merged.push(MergedVictim {
                id: victim.id.clone(),
                title: victim.title.clone(),
                rehomed_edges,
                skipped_edges,
            });
        }

        // The survivor's convergence is a deliberate update: it re-embeds,
        // stamps confirmed_at, and earns the same-turn verdict set.
        let checked = self.update_node_checked(
            survivor_id,
            NodePatch {
                title,
                body,
                tags: Some(tags),
                code_refs: Some(code_refs),
                ..NodePatch::default()
            },
        )?;
        let mut warnings = checked.warnings;
        warnings.retain(|w| !victim_ids.contains(&w.id));
        Ok(MergeOutcome {
            survivor: checked.node,
            merged,
            warnings,
            suspects: checked.suspects,
            missing_refs: checked.missing_refs,
            canon: checked.canon,
        })
    }

    /// Pending suspects that involve this node — the judgeable form of what a
    /// write just queued.
    fn suspects_involving(&self, node_id: &str) -> Result<Vec<SuspectView>> {
        Ok(self
            .store
            .suspects_pending()?
            .into_iter()
            .filter(|s| s.a.id == node_id || s.b.id == node_id)
            .collect())
    }

    /// Nearby nodes that are contradicted (active `conflicts-with`),
    /// superseded, or tombstoned — returned with writes so the writing
    /// assistant notices it may be re-treading contested, stale, or
    /// deliberately killed ground (PLAN §7, pull-based).
    ///
    /// `tombstoned` (0.9.2) is the write-path half of the tombstone role:
    /// before it, a tombstone was findable but guarded nothing — it is
    /// active (so never `superseded`), sits out the conflict scan (so never
    /// `in-active-conflict`), and the near-duplicate match is same-type, so
    /// a Decision re-deriving a tombstoned Decision sailed through. The
    /// check is role-based and cross-type on purpose: the shape of the
    /// resurrection doesn't matter, the content does. Superseded says "a
    /// successor exists — follow it"; tombstoned says "killed without one —
    /// re-adding is the error".
    ///
    /// Two channels for the tombstone role (0.9.6): the vector neighbourhood
    /// and an exact match between the new title and a marker's victim title
    /// — re-adding a note word for word is the resurrection the role exists
    /// to catch, and it must not depend on where the marker's vector landed
    /// (KnowledgeDrift v2 at 500: `resurrection_warned` 0.81 on the vector
    /// channel alone, 1.00 with the title channel). The vector scan stays
    /// eight deep on purpose: read 24 deep it warned 38% of rewrites of
    /// PURGED notes about other subjects' markers (knob probe, 2026-09-16).
    fn write_warnings(
        &self,
        near: &[(Node, f64)],
        exclude_id: &str,
        title: &str,
    ) -> Result<Vec<WriteWarning>> {
        let mut warnings = Vec::new();
        let cfg = self.store.config();
        let warn_similarity = cfg.policy.warn_similarity;
        for (node, similarity) in near {
            let (node, similarity) = (node.clone(), *similarity);
            if similarity < warn_similarity {
                break;
            }
            let (reason, note) = if node.valid_until.is_some() {
                ("superseded", None)
            } else if is_tombstone(&cfg, &node) {
                ("tombstoned", tombstone_note(&node))
            } else if self.store.has_active_conflict(&node.id)? {
                ("in-active-conflict", None)
            } else {
                continue;
            };
            warnings.push(WriteWarning {
                id: node.id,
                title: node.title,
                reason: reason.to_string(),
                similarity,
                note,
            });
        }
        if cfg.tombstone_type().is_some() && !warnings.iter().any(|w| w.reason == "tombstoned") {
            let wanted = normalized_title(title);
            if !wanted.is_empty() {
                for node in self.store.all_nodes()? {
                    if node.id == exclude_id
                        || node.valid_until.is_some()
                        || !is_tombstone(&cfg, &node)
                        || normalized_title(tombstone_victim_title(&node.title)) != wanted
                    {
                        continue;
                    }
                    warnings.push(WriteWarning {
                        note: tombstone_note(&node),
                        id: node.id,
                        title: node.title,
                        reason: "tombstoned".to_string(),
                        similarity: 1.0,
                    });
                    break;
                }
            }
        }
        Ok(warnings)
    }

    /// The session-start brief: a token-budgeted markdown digest of the graph's
    /// canon — unresolved conflicts, suspects to judge, what changed recently,
    /// the open worklist, then the per-type canon sections. Composition —
    /// which sections, their caps and excerpt lengths, the total budget —
    /// comes from the graph's config (PLAN §7D); the shipped defaults render
    /// the classic principles/decisions/cautions shape. Every record uses
    /// one line shape and carries its node id. Every included node's decay
    /// clock is refreshed: being briefed counts as reuse.
    pub fn brief(&self, max_chars: usize) -> Result<String> {
        let cfg = self.store.config();
        let bc = &cfg.brief;
        let mut out = String::from("# Engram brief\n");
        let mut included: Vec<String> = Vec::new();
        // `brief.bodies` off = titles only, whatever each section's excerpt.
        let ex = |excerpt: usize| if bc.bodies { excerpt } else { 0 };
        let mut seen = std::collections::HashSet::new();

        let push_line = |out: &mut String, line: &str| -> bool {
            if out.len() + line.len() + 1 > max_chars {
                return false;
            }
            out.push_str(line);
            out.push('\n');
            true
        };

        'assemble: {
            // Teach the ontology up front when configured (custom ontologies
            // the assistant's skill can't know; off in the shipped preset —
            // `describe_ontology` serves the same content on demand).
            if bc.ontology.show {
                for line in cfg.describe_ontology().lines() {
                    if !push_line(&mut out, line) {
                        break 'assemble;
                    }
                }
            }

            // Version tracking: the current working version leads the brief
            // (every version-bound note is stamped with it; set_version
            // moves it when the project does).
            let working_version = if cfg.versioning.enabled {
                self.store.current_version()?
            } else {
                None
            };
            if cfg.versioning.enabled {
                let line = match working_version.clone() {
                    Some(v) => format!(
                        "Current working version: {v} — new notes are stamped with it, and brief lines show a version only when it is an older one; call `set_version` when the project moves on."
                    ),
                    None => "Current working version: not set — call `set_version` once you know it (a release tag, a date, anything).".to_string(),
                };
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
            }

            // Handoff notes ([`crate::config::HANDOFF_TAG`]): what the LAST
            // session left for THIS one — guaranteed top placement, never
            // sampled away. Resolve one once it is acted on; volatile decay
            // burns forgotten leftovers. The open worklist is fetched once
            // here and partitioned; the open-work section below reuses it.
            let (handoff, worklist): (Vec<Node>, Vec<Node>) = self
                .store
                .list_open(&[])?
                .into_iter()
                .partition(|n| n.tags.iter().any(|t| t == crate::config::HANDOFF_TAG));
            // The current cycle (0.9.9): open worklist notes stamped with the
            // working version lead, right under the version line — what this
            // cycle set out to do and still owes. Handoff notes keep their own
            // section; everything else stays in the open section below.
            let (cycle, worklist): (Vec<Node>, Vec<Node>) = match (&working_version, bc.cycle.show)
            {
                (Some(v), true) => worklist
                    .into_iter()
                    .partition(|n| n.version.as_deref() == Some(v.as_str())),
                _ => (Vec::new(), worklist),
            };
            if let Some(v) = working_version.as_deref().filter(|_| !cycle.is_empty()) {
                let heading = format!(
                    "\n## Current cycle\nOpen work stamped with the working version: what this cycle set out to do and still owes. `search` with `during_version: \"{v}\"` reads everything captured in it."
                );
                if !push_line(&mut out, &heading) {
                    break 'assemble;
                }
                for n in cycle.iter().take(bc.cycle.cap) {
                    let line = node_line_cfg(
                        n,
                        ex(bc.cycle.excerpt),
                        Some(&cfg),
                        working_version.as_deref(),
                    );
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                    seen.insert(n.id.clone());
                    included.push(n.id.clone());
                }
                if cycle.len() > bc.cycle.cap {
                    let line = format!(
                        "- …and {} more of this cycle — `list_open` has the full worklist.",
                        cycle.len() - bc.cycle.cap
                    );
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                }
            }
            // Tasks (0.10.0): the working list beside the graph — what the
            // assistant was doing, titles only. Rules settle on the way in.
            for line in self.brief_task_lines()? {
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
            }
            if bc.handoff.show {
                if !handoff.is_empty()
                    && !push_line(
                        &mut out,
                        "\n## Handoff — left for this session, read first\nAct on each, then mark it resolved (`update_node` status resolved).",
                    )
                {
                    break 'assemble;
                }
                for n in handoff.iter().take(bc.handoff.cap) {
                    let line = node_line_cfg(
                        n,
                        ex(bc.handoff.excerpt),
                        Some(&cfg),
                        working_version.as_deref(),
                    );
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                    seen.insert(n.id.clone());
                    included.push(n.id.clone());
                }
            }

            // The live tag vocabulary, up front: one cheap line the writing
            // assistant must see (a budget-cut tail section never surfaces on
            // a mature graph). A genuinely new tag is fine — created on write.
            let tags = if bc.tags.show {
                self.store.tag_stats(bc.tags.cap)?
            } else {
                Vec::new()
            };
            if !tags.is_empty() {
                let list = tags
                    .iter()
                    .map(|t| t.tag.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let line = format!("Recent tags (reuse before inventing new ones): {list}");
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
            }

            let conflicts = if bc.conflicts.show {
                self.store.active_conflict_edges()?
            } else {
                Vec::new()
            };
            if !conflicts.is_empty() && !push_line(&mut out, "\n## Unresolved conflicts") {
                break 'assemble;
            }
            for e in conflicts {
                let (Some(a), Some(b)) = (
                    self.store.get_node(&e.from_id)?,
                    self.store.get_node(&e.to_id)?,
                ) else {
                    continue;
                };
                let line = format!(
                    "- \"{}\" [{} {}] conflicts with \"{}\" [{} {}]",
                    a.title,
                    a.node_type.as_str(),
                    a.id,
                    b.title,
                    b.node_type.as_str(),
                    b.id,
                );
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
                for n in [a, b] {
                    if seen.insert(n.id.clone()) {
                        included.push(n.id);
                    }
                }
            }

            let mut suspects = if bc.suspects.show {
                self.store.suspects_pending()?
            } else {
                Vec::new()
            };
            // NLI-hinted contradictions first (the pairs most worth the
            // judge's attention), then strongest similarity; capped — the
            // brief is a digest, the full queue lives in list_suspects/pane.
            suspects.sort_by(|x, y| {
                let contra = |s: &SuspectView| s.nli_label.as_deref() == Some("contradiction");
                contra(y)
                    .cmp(&contra(x))
                    .then(y.similarity.total_cmp(&x.similarity))
            });
            let overflow = suspects.len().saturating_sub(bc.suspects.cap);
            suspects.truncate(bc.suspects.cap);
            if !suspects.is_empty() {
                let heading = "\n## Suspected conflicts — judge these\nThe local scan flagged \
                     unlinked look-alike pairs. For each: `resolve_suspect(id, verdict)` with \
                     `conflict` (they contradict), `replaces` (the newer supersedes — archives \
                     the older), or `dismiss` (unrelated/fine together). A hint of `inherited` \
                     means the newer note contradicts or replaces something the other one builds \
                     on, needs, or holds because of — follow its edges before judging.";
                if !push_line(&mut out, heading) {
                    break 'assemble;
                }
                for s in suspects {
                    let hint = match (&s.nli_label, s.nli_score) {
                        (Some(label), Some(score)) => {
                            let side = match s.nli_direction.as_deref() {
                                Some(side) => format!(", negation likely on the {side} side"),
                                None => String::new(),
                            };
                            format!("; hint: {label} {:.0}%{side}", score * 100.0)
                        }
                        _ => String::new(),
                    };
                    let line = format!(
                        "- {}: \"{}\" [{} {}] vs \"{}\" [{} {}] ({:.0}% similar{hint})",
                        s.id,
                        s.a.title,
                        s.a.node_type.as_str(),
                        s.a.id,
                        s.b.title,
                        s.b.node_type.as_str(),
                        s.b.id,
                        s.similarity * 100.0,
                    );
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                }
                if overflow > 0
                    && !push_line(
                        &mut out,
                        &format!("- …and {overflow} more — `list_suspects` has the full queue."),
                    )
                {
                    break 'assemble;
                }
            }

            // What changed lately, right after the judgment queue: recency is
            // the context the assistant continues from, so it must never fall
            // into the budget-cut tail. A node shown here is claimed — later
            // sections skip it rather than repeat it.
            let recent: Vec<Node> = if bc.recent.show {
                self.store
                    .recent_nodes(bc.recent.cap)?
                    .into_iter()
                    .filter(|n| !seen.contains(&n.id))
                    .collect()
            } else {
                Vec::new()
            };
            if !recent.is_empty() && !push_line(&mut out, "\n## Recently added") {
                break 'assemble;
            }
            for n in recent {
                let line = node_line_cfg(
                    &n,
                    ex(bc.recent.excerpt),
                    Some(&cfg),
                    working_version.as_deref(),
                );
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
                seen.insert(n.id.clone());
                included.push(n.id);
            }

            // Newest first (list_open's order), capped: the brief samples the
            // worklist, it doesn't mirror it — uncapped, a dogfood-sized
            // worklist ate a third of the budget and starved every later
            // section. The overflow line keeps the full count honest.
            let open: Vec<Node> = if bc.open.show {
                worklist
                    .into_iter()
                    .filter(|n| !seen.contains(&n.id))
                    .collect()
            } else {
                Vec::new()
            };
            // "## Open problems & intents" in the shipped set — the heading
            // names whatever types carry the worklist role here.
            let open_heading = format!(
                "\n## Open {}",
                cfg.worklist_types()
                    .iter()
                    .map(|t| format!("{}s", t.to_lowercase()))
                    .collect::<Vec<_>>()
                    .join(" & ")
            );
            if !open.is_empty() && !push_line(&mut out, &open_heading) {
                break 'assemble;
            }
            for n in open.iter().take(bc.open.cap) {
                let line = node_line_cfg(
                    n,
                    ex(bc.open.excerpt),
                    Some(&cfg),
                    working_version.as_deref(),
                );
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
                seen.insert(n.id.clone());
                included.push(n.id.clone());
            }
            if open.len() > bc.open.cap {
                let line = format!(
                    "- …and {} more — `list_open` has the full worklist.",
                    open.len() - bc.open.cap
                );
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
            }

            // Live-edge counts for `canon_order: connected`, computed once
            // on first use.
            let mut degree: Option<std::collections::HashMap<String, usize>> = None;
            // The per-type canon sections, in ontology order (the shipped set
            // shows Principles, then Decisions with a shorter excerpt —
            // their titles are already declarative — then Cautions).
            for t in cfg.ontology.types.iter().filter(|t| t.brief.show) {
                let heading = format!("\n## {}s", t.name);
                let node_type = NodeType::parse(&t.name)?;
                let (cap, excerpt) = (t.brief.cap, t.brief.excerpt);
                // Fetch the full active set: nodes already claimed by an
                // earlier section (conflicts, recent) must not starve this
                // one, and `elsewhere` must count every such node — a capped
                // window misses seen nodes ranked below it and the overflow
                // line then double-counts them as "more".
                let total = self.store.count_by_type_active(&node_type)? as usize;
                let mut fetched = self.store.nodes_by_type_active(&node_type, total)?;
                if bc.canon_order == "connected" {
                    let degree = degree.get_or_insert_with(|| self.live_degrees());
                    sort_connected(&mut fetched, degree);
                }
                let elsewhere = fetched.iter().filter(|n| seen.contains(&n.id)).count();
                let nodes: Vec<Node> = fetched
                    .into_iter()
                    .filter(|n| !seen.contains(&n.id))
                    .take(cap)
                    .collect();
                if !nodes.is_empty() && !push_line(&mut out, &heading) {
                    break 'assemble;
                }
                let shown = nodes.len();
                for n in nodes {
                    let line =
                        node_line_cfg(&n, ex(excerpt), Some(&cfg), working_version.as_deref());
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                    seen.insert(n.id.clone());
                    included.push(n.id);
                }
                // The cap hides real canon; say how much, so the assistant
                // knows the section is a sample, not the whole set.
                if total > shown + elsewhere {
                    let line = format!(
                        "- …{} more {}s — `search` reaches them.",
                        total - shown - elsewhere,
                        node_type.as_str()
                    );
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                }
            }
        }

        // Cold start (PLAN §11 / the day-one problem): an empty brief teaches
        // the assistant to offer seeding instead of reporting nothing.
        if included.is_empty() && self.store.all_nodes()?.is_empty() {
            out.push_str(COLD_START_BRIEF);
            // A cold graph is exactly where the writer has never seen the
            // ontology (issue #9: a model with no loaded skill guessed at
            // types until it grepped the schema) — teach this graph's actual
            // set in the same breath, unless the brief already led with it.
            if !bc.ontology.show {
                out.push('\n');
                out.push_str(&cfg.describe_ontology());
            }
        }

        self.store.touch(&included)?;
        // Activity journal: a served brief is the trace of a session starting
        // work (whoever asked — hook, MCP tool, pane lens, CLI).
        self.audit_activity(
            "brief_served",
            Some(format!(
                "{} chars, {} nodes included",
                out.len(),
                included.len()
            )),
        )?;
        Ok(out)
    }

    // ---- conflict scan (PLAN §7): detection is local and automatic; judgment
    // stays with Claude in-session or the user in the pane. The daemon never
    // calls an LLM.

    /// The loaded logic layer, or the uniform sweeps-need-NLI error.
    fn require_nli(&self) -> Result<&dyn Nli> {
        self.nli.as_deref().ok_or_else(|| {
            crate::Error::Embedding(
                "the NLI model is not loaded — audit sweeps need the local logic layer".into(),
            )
        })
    }

    /// Sweep the whole graph for unlinked look-alike pairs (the pane's
    /// "Scan now" and the daemon's periodic pass). Returns how many new
    /// suspects were queued.
    pub fn scan_conflicts(&self) -> Result<usize> {
        let mut added = 0;
        for id in self.scan_ids()? {
            added += self.scan_one(&id)?;
        }
        if added > 0 {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(added)
    }

    /// The nodes a conflict scan visits, in scan order.
    pub fn scan_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .store
            .scannable_nodes()?
            .into_iter()
            .map(|n| n.id)
            .collect())
    }

    /// Scan one node for suspects; 0 when it is gone, archived or has no
    /// vector (it may have changed since [`Engine::scan_ids`] listed it).
    pub fn scan_one(&self, id: &str) -> Result<usize> {
        let Some(node) = self.store.get_node(id)? else {
            return Ok(0);
        };
        if node.valid_until.is_some() {
            return Ok(0);
        }
        let Some(vec) = self.store.embedding_of(id)? else {
            return Ok(0);
        };
        self.suspects_near(&node, &vec)
    }

    // ---- local cortex, logic layer (PLAN §7A). All read-only nominations:
    // sweeps queue suspects for judgment, claim checks annotate — no trust
    // field moves here.

    /// Verify a claim against the canon: retrieve the nearest nodes, judge
    /// each (node claim as premise, input as hypothesis), and bucket into
    /// supports / contradicts / silent. NLI beats a similarity list here
    /// because "the canon disagrees" and "the canon doesn't know" are
    /// different answers — one is a conflict, the other a gap worth capturing.
    pub fn check_claim(&self, text: &str, limit: usize) -> Result<ClaimReport> {
        let Some(nli) = &self.nli else {
            return Err(crate::Error::Embedding(
                "the NLI model is not loaded — claim checks need the local logic layer".into(),
            ));
        };
        let qv = self.embedder.embed_one(text)?;
        // Unwindowed on purpose: a claim is checked against the whole canon,
        // because knowledge that contradicts it does not stop counting for
        // being old.
        let hits = self.store.search_hybrid(
            text,
            Some(&qv),
            &[],
            limit.clamp(4, 16),
            Default::default(),
            &Default::default(),
        )?;
        let cfg = self.config();
        let mut report = ClaimReport {
            claim: text.to_string(),
            supports: Vec::new(),
            contradicts: Vec::new(),
            retracted: Vec::new(),
            silent: Vec::new(),
        };
        let mut nodes = Vec::new();
        for h in &hits {
            let Some(n) = self.store.get_node(&h.id)? else {
                continue;
            };
            // A tombstone-role hit is sorted out by ROLE, before the model
            // sees it (0.9.2): the claim lands on knowledge a person
            // deliberately removed, and that is a verdict of its own —
            // "retracted" — not an entailment question. The retrieval score
            // rides in `entailment` so the bucket still sorts strongest
            // first; the NLI columns are zero because none ran.
            if is_tombstone(&cfg, &n) {
                report.retracted.push(ClaimVerdict {
                    id: n.id,
                    node_type: n.node_type,
                    title: n.title,
                    trust: n.trust,
                    stale: n.stale,
                    entailment: h.score as f32,
                    neutral: 0.0,
                    contradiction: 0.0,
                    project: None,
                });
                continue;
            }
            nodes.push(n);
        }
        let pairs: Vec<(String, String)> =
            nodes.iter().map(|n| (claim(n), text.to_string())).collect();
        let judgments = nli.judge(&pairs)?;

        for (node, j) in nodes.into_iter().zip(judgments) {
            let verdict = ClaimVerdict {
                id: node.id,
                node_type: node.node_type,
                title: node.title,
                trust: node.trust,
                stale: node.stale,
                entailment: j.entailment,
                neutral: j.neutral,
                contradiction: j.contradiction,
                project: None,
            };
            // A contradiction the model is not confident about is reported as
            // silence, not as a conflict. Every assertion here costs a human
            // judgment, and the benchmark in `eval/CONTRADICTIONS.md` measured
            // what the ungated layer was spending: the model calls most
            // UNRELATED claims contradictions, and the gate is what separates
            // the populations. The raw probabilities ride along on the verdict
            // either way, so nothing is hidden — only unasserted.
            //
            // Deliberately contradiction-only: false `supports` has never been
            // measured, and gating it on the same number would be guessing.
            let floor = self.config().policy.claim_contradiction_min_confidence;
            match j.label() {
                "entailment" => report.supports.push(verdict),
                "contradiction" if f64::from(verdict.contradiction) >= floor => {
                    report.contradicts.push(verdict)
                }
                _ => report.silent.push(verdict),
            }
        }
        report
            .contradicts
            .sort_by(|a, b| b.contradiction.total_cmp(&a.contradiction));
        // Strongest near-miss first. Roughly a fifth of claims that restate a
        // stored note verbatim are judged neutral rather than entailment
        // (`eval/CONTRADICTIONS.md`), and a gated contradiction lands here too
        // — so this bucket is where the layer's declined-to-assert cases
        // collect, and reading it top-down is reading them in order of how
        // close the model came.
        report.silent.sort_by(|a, b| {
            b.entailment
                .max(b.contradiction)
                .total_cmp(&a.entailment.max(a.contradiction))
        });
        report
            .supports
            .sort_by(|a, b| b.entailment.total_cmp(&a.entailment));
        report
            .retracted
            .sort_by(|a, b| b.entailment.total_cmp(&a.entailment));
        Ok(report)
    }

    /// Conflict sweep (the Checkup panel's "Find hidden conflicts"): rescan
    /// at the standing similarity threshold, queueing only pairs the NLI
    /// layer marks as contradictions. Above the floor the claim text is
    /// judged; MNLI-class models presuppose co-reference, and below that band
    /// unrelated same-shaped claims read as confident contradictions (the
    /// dogfood finding of 2026-07-13 — 140 junk pairs at a 0.8 gate). Below
    /// it (0.9.11) the sweep admits exactly what the write path admits since
    /// 0.9.4: titles that name one subject and share a claim word, read as a
    /// contradiction at `policy.conflict_nli_gate` — so the button reaches
    /// what the background scan reaches, and further down the neighbour list.
    pub fn audit_conflicts(&self) -> Result<AuditSweep> {
        self.audit_sweep(
            "contradiction",
            self.store.config().policy.conflict_suspect_similarity,
            AUDIT_NLI_PAIR_BUDGET,
        )
    }

    /// Duplicate sweep (the Audit panel's "Find duplicates"): mutual
    /// entailment above a 0.80 similarity floor — two nodes stating the same
    /// thing. Queued as suspects; the judge's `replaces` verdict is the merge.
    pub fn audit_duplicates(&self) -> Result<AuditSweep> {
        self.audit_sweep("entailment", 0.80, AUDIT_NLI_PAIR_BUDGET)
    }

    /// Shared sweep: nominate unlinked, unraised look-alike pairs whose NLI
    /// hint matches `target`. NLI pair budget capped — an audit that takes a
    /// minute under the engine lock is worse than one that says "truncated,
    /// run me again". Since 0.9.11 "again" makes progress: each unordered
    /// pair is judged once per run, and a pair judged without queueing is
    /// remembered ([`SweepMemory`]) so the next run spends its budget on
    /// pairs no run has read yet.
    pub(crate) fn audit_sweep(
        &self,
        target: &'static str,
        floor: f64,
        budget: usize,
    ) -> Result<AuditSweep> {
        let cfg = self.store.config();
        let nli = self.require_nli()?;
        // Below the floor only the conflict sweep reads, and only through the
        // title gate — the duplicate sweep's floor is already its band.
        let title_gate = cfg
            .policy
            .conflict_nli_gate
            .filter(|_| target == "contradiction");
        let low = match title_gate {
            Some(_) => crate::policy::CONFLICT_NLI_FLOOR.min(floor),
            None => floor,
        };
        let mut memory = SweepMemory::load(
            self.store.as_ref(),
            target,
            &format!(
                "{}|{:?}|{}",
                nli.model_id(),
                title_gate,
                cfg.policy.nli_sweep_min_confidence
            ),
        )?;
        let mut sweep = AuditSweep::default();
        // The scan meets every close pair from both sides; whatever happened
        // to it the first time (judged, queued, skipped) holds for the second.
        let mut seen = std::collections::HashSet::new();
        'nodes: for node in self.store.scannable_nodes()? {
            let Some(vec) = self.store.embedding_of(&node.id)? else {
                continue;
            };
            for (id, distance) in self.store.search_vec(&vec, 12)? {
                let similarity = 1.0 - distance;
                if id == node.id {
                    continue;
                }
                if similarity < low {
                    break; // distance-ordered
                }
                let key = if node.id < id {
                    (node.id.clone(), id.clone())
                } else {
                    (id.clone(), node.id.clone())
                };
                if !seen.insert(key) {
                    continue;
                }
                let Some(other) = self.store.get_node(&id)? else {
                    continue;
                };
                // Below the floor a pair exists for this sweep only if the
                // title guard admits it; the skip counters describe the close
                // pairs above it.
                let path = if similarity >= floor {
                    None
                } else {
                    match title_pair_admission(&node.title, &other.title) {
                        Some(path) => Some(path),
                        None => continue,
                    }
                };
                let passed_over = |bucket: &mut usize| {
                    if path.is_none() {
                        *bucket += 1;
                    }
                };
                if is_anchor(&cfg, &other)
                    || is_tombstone(&cfg, &other)
                    || other.valid_until.is_some()
                {
                    passed_over(&mut sweep.inactive);
                    continue;
                }
                if self.store.pair_linked(&node.id, &other.id)? {
                    passed_over(&mut sweep.already_linked);
                    continue;
                }
                if self.store.suspect_between(&node.id, &other.id)? {
                    passed_over(&mut sweep.already_raised);
                    continue;
                }
                let memo = pair_memo(&node, &other, path.unwrap_or("claim"));
                if memory.contains(memo) {
                    sweep.already_judged += 1;
                    continue;
                }
                if sweep.examined >= budget {
                    sweep.truncated = true;
                    break 'nodes;
                }
                sweep.examined += 1;
                let hint = match path {
                    None => self.nli_hint(&node, &other),
                    Some(path) => self.nli_title_hint_for(path, &node, &other),
                };
                let Some((label, score, direction)) = hint else {
                    continue; // the judge failed: nothing was read, nothing to remember
                };
                let gate = match path {
                    None => cfg.policy.nli_sweep_min_confidence,
                    Some(_) => title_gate.unwrap_or(f64::INFINITY),
                };
                if label != target || score < gate {
                    memory.insert(memo);
                    continue;
                }
                let (newer, older) = if node.created_at >= other.created_at {
                    (&node.id, &other.id)
                } else {
                    (&other.id, &node.id)
                };
                self.store.add_suspect(
                    newer,
                    older,
                    similarity,
                    Some((label, score, direction)),
                )?;
                sweep.queued += 1;
            }
        }
        memory.save(self.store.as_ref())?;
        if sweep.queued > 0 {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(sweep)
    }

    /// "Check open problems": does any current node entail an answer to an
    /// open Problem/Intent? Returns nominations — the human (or assistant)
    /// still links `answers` and resolves. Pairs already linked with the
    /// answer-role verb are dropped (nothing to suggest); pairs linked some
    /// OTHER way keep their nomination but rank under a penalty, carrying
    /// the existing verb — "these are connected, but maybe the answer link
    /// is the one that's missing".
    pub fn audit_answered(&self) -> Result<Vec<AnsweredHint>> {
        const NLI_PAIR_BUDGET: usize = 150;
        let nli = self.require_nli()?;
        let cfg = self.store.config();
        let answer_verb = cfg.ontology.verbs.iter().find(|v| v.roles.answer);
        let mut hints = Vec::new();
        let mut examined = 0;
        for problem in self.store.list_open(&[])? {
            let Some(vec) = self.store.embedding_of(&problem.id)? else {
                continue;
            };
            // The problem's incident edges, fetched once for all candidates.
            let mut incident = self.store.edges_out(&problem.id)?;
            incident.extend(self.store.edges_in(&problem.id)?);
            for (id, distance) in self.store.search_vec(&vec, 8)? {
                if id == problem.id || 1.0 - distance < 0.6 {
                    continue;
                }
                let Some(candidate) = self.store.get_node(&id)? else {
                    continue;
                };
                // Answer candidates by role: any non-worklist, non-anchor,
                // non-tombstone type can settle an open item (Resolution/
                // Decision/Insight and the canon types in the shipped set —
                // a removal marker never answers open work).
                let can_answer = cfg
                    .type_def(candidate.node_type.as_str())
                    .is_some_and(|t| !t.roles.worklist && !t.roles.anchor && !t.roles.tombstone);
                if candidate.valid_until.is_some() || !can_answer {
                    continue;
                }
                if examined >= NLI_PAIR_BUDGET {
                    return Ok(hints);
                }
                examined += 1;
                let Ok(j) = nli.judge(&[(claim(&candidate), claim(&problem))]) else {
                    continue;
                };
                let entailment = j[0].entailment;
                if entailment >= 0.6 {
                    // Already linked? With the answer verb: nothing left to
                    // suggest. With another verb: keep the nomination at a
                    // penalty — the connection exists, but the answer link
                    // may be the missing one.
                    let existing: Vec<String> = incident
                        .iter()
                        .filter(|e| e.to_id == candidate.id || e.from_id == candidate.id)
                        .map(|e| e.edge_type.as_str().to_string())
                        .collect();
                    if let Some(av) = answer_verb
                        && existing.contains(&av.name)
                    {
                        continue;
                    }
                    hints.push(AnsweredHint {
                        problem: SuspectEndpoint {
                            id: problem.id.clone(),
                            node_type: problem.node_type.clone(),
                            title: problem.title.clone(),
                        },
                        candidate: SuspectEndpoint {
                            id: candidate.id,
                            node_type: candidate.node_type,
                            title: candidate.title,
                        },
                        entailment: entailment as f64,
                        existing_link: existing.into_iter().next(),
                    });
                }
            }
        }
        // Fresh pairs first: an existing (non-answer) link halves the rank.
        let rank =
            |h: &AnsweredHint| h.entailment * if h.existing_link.is_some() { 0.5 } else { 1.0 };
        hints.sort_by(|a, b| rank(b).total_cmp(&rank(a)));
        Ok(hints)
    }

    /// "Triage stale notes": judge each stale node against its nearest live
    /// canon and say what the evidence suggests — `reconfirm` (a current node
    /// still entails it: confirm-still-true restores its trust),
    /// `contradicted` (a current node disputes it — judge as a conflict;
    /// gated on the suspect similarity band because MNLI presupposes
    /// co-reference), or `isolated` (nothing current speaks to it — an
    /// archive candidate). Nominations only; nothing self-applies.
    pub fn audit_stale_triage(&self) -> Result<Vec<StaleTriage>> {
        const NLI_PAIR_BUDGET: usize = 150;
        const TRIAGE_ENTAILMENT: f32 = 0.60;
        let nli = self.require_nli()?;
        let cfg = self.store.config();
        let mut out = Vec::new();
        let mut examined = 0;
        'stale: for node in self.store.recent_nodes(usize::MAX)? {
            if !node.stale || node.valid_until.is_some() || node.trust_override.is_some() {
                continue;
            }
            let endpoint = SuspectEndpoint {
                id: node.id.clone(),
                node_type: node.node_type.clone(),
                title: node.title.clone(),
            };
            let Some(vec) = self.store.embedding_of(&node.id)? else {
                continue;
            };
            let mut spoke = false;
            for (id, distance) in self.store.search_vec(&vec, 6)? {
                let similarity = 1.0 - distance;
                if id == node.id || similarity < 0.6 {
                    continue;
                }
                let Some(other) = self.store.get_node(&id)? else {
                    continue;
                };
                if other.valid_until.is_some() || other.stale {
                    continue;
                }
                if examined >= NLI_PAIR_BUDGET {
                    break 'stale;
                }
                examined += 1;
                let Ok(j) = nli.judge_pair(&claim(&other), &claim(&node)) else {
                    continue;
                };
                let evidence = SuspectEndpoint {
                    id: other.id,
                    node_type: other.node_type,
                    title: other.title,
                };
                if j.contradiction() >= TRIAGE_ENTAILMENT
                    && similarity >= cfg.policy.conflict_suspect_similarity
                {
                    out.push(StaleTriage {
                        node: endpoint.clone(),
                        trust: node.trust,
                        verdict: "contradicted".into(),
                        evidence: Some(evidence),
                        score: j.contradiction() as f64,
                    });
                    continue 'stale;
                }
                if j.forward.entailment >= TRIAGE_ENTAILMENT {
                    out.push(StaleTriage {
                        node: endpoint.clone(),
                        trust: node.trust,
                        verdict: "reconfirm".into(),
                        evidence: Some(evidence),
                        score: j.forward.entailment as f64,
                    });
                    continue 'stale;
                }
                spoke = true;
            }
            if !spoke {
                out.push(StaleTriage {
                    node: endpoint,
                    trust: node.trust,
                    verdict: "isolated".into(),
                    evidence: None,
                    score: 0.0,
                });
            }
        }
        Ok(out)
    }

    /// Timeline (PLAN §10): the chronological story of one piece of
    /// knowledge — every generation connected to `id` through `replaces`
    /// edges, oldest first. A node that was never part of a supersession
    /// yields a single-entry timeline. Each superseded generation carries the
    /// note of the `replaces` edge that retired it (the why of the change).
    pub fn timeline(&self, id: &str) -> Result<Vec<TimelineEntry>> {
        let cfg = self.store.config();
        let supersession = cfg.supersession_verb();
        let Some(start) = self.store.get_node(id)? else {
            return Err(crate::Error::NotFound(format!("node {id}")));
        };
        let mut seen = std::collections::HashSet::from([start.id.clone()]);
        let mut queue = vec![start.id.clone()];
        let mut nodes = vec![start];
        let mut replaced_note = std::collections::HashMap::new();
        // (newer, older) pairs — the chain's own topology orders generations;
        // created_at only breaks ties (same-second writes sort randomly).
        let mut pairs = std::collections::HashSet::new();
        while let Some(cur) = queue.pop() {
            let mut edges = self.store.edges_out(&cur)?;
            edges.extend(self.store.edges_in(&cur)?);
            for e in edges {
                if e.edge_type.as_str() != supersession {
                    continue;
                }
                // The edge reads "from replaces to": its note explains why
                // the `to` generation was retired.
                replaced_note.insert(e.to_id.clone(), e.note);
                pairs.insert((e.from_id.clone(), e.to_id.clone()));
                for next in [e.from_id, e.to_id] {
                    if seen.insert(next.clone())
                        && let Some(n) = self.store.get_node(&next)?
                    {
                        nodes.push(n);
                        queue.push(next);
                    }
                }
            }
        }
        // Generation = longest replaces-path down to an original (0). Sorting
        // by it puts every node after everything it (transitively) replaced.
        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        let mut adj: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
        for (newer, older) in &pairs {
            adj.entry(newer.as_str()).or_default().push(older.as_str());
        }
        let mut memo = std::collections::HashMap::new();
        for id in &ids {
            generation(id.as_str(), &adj, &mut memo);
        }
        nodes.sort_by(|a, b| {
            memo[a.id.as_str()]
                .cmp(&memo[b.id.as_str()])
                .then(a.created_at.cmp(&b.created_at))
                .then(a.id.cmp(&b.id))
        });
        Ok(nodes
            .into_iter()
            .map(|n| TimelineEntry {
                replaced_note: replaced_note.get(&n.id).cloned().flatten(),
                id: n.id,
                node_type: n.node_type,
                title: n.title,
                created_at: n.created_at,
                valid_until: n.valid_until,
            })
            .collect())
    }

    /// Verified code refs (PLAN §10): current nodes whose path-shaped
    /// code_refs no longer exist under `root` have drifted — the code moved
    /// or was deleted and the memory didn't follow. A contradiction between
    /// the graph and reality, surfaced for review like a conflict. Reporting
    /// only — drift deliberately does NOT demote: the scan runs on every pane
    /// load against an environment-dependent root (a wrong cwd or a feature
    /// branch with files temporarily gone would mass-stamp sticky demotions
    /// across the graph). Judged conflicts are the demotion trigger; drift is
    /// a review queue. Free-text responsibility labels (anything with
    /// whitespace) are not checkable and never drift.
    pub fn scan_code_refs(&self, root: &std::path::Path) -> Result<Vec<Drift>> {
        let mut out = Vec::new();
        for node in self.store.all_nodes()? {
            if node.valid_until.is_some() || node.code_refs.is_empty() {
                continue;
            }
            let missing: Vec<String> = node
                .code_refs
                .iter()
                .filter(|r| ref_is_path(r) && !root.join(r.as_str()).exists())
                .cloned()
                .collect();
            if !missing.is_empty() {
                out.push(Drift {
                    id: node.id,
                    node_type: node.node_type,
                    title: node.title,
                    missing,
                });
            }
        }
        Ok(out)
    }

    /// Shared candidate logic: nearest neighbors above the suspect threshold,
    /// both active and non-anchor, not already linked by any edge, pair never
    /// raised before. Stored newer-first so `replaces` verdicts read forward.
    fn suspects_near(&self, node: &Node, vec: &[f32]) -> Result<usize> {
        let candidates = self.suspect_candidates(node, &self.nearby(vec, &node.id)?)?;
        let judged = judge_suspects(self.nli.as_deref(), &self.config(), node, candidates);
        self.queue_suspects(node, judged)
    }

    /// The pairs [`Engine::suspects_near`] judges — store reads only, so a
    /// shared write gathers them under the lock and judges them off it.
    fn suspect_candidates(
        &self,
        node: &Node,
        near: &[(Node, f64)],
    ) -> Result<Vec<SuspectCandidate>> {
        let cfg = self.store.config();
        // Tombstones sit out BOTH sides here, as they do in the sweep's
        // scannable set: a tombstone resembles its victim by design, and a
        // suspect between them invites a `replaces` verdict that would
        // archive the marker. The write path speaks about tombstones through
        // the `tombstoned` warning instead (0.9.2).
        if is_anchor(&cfg, node) || is_tombstone(&cfg, node) || node.valid_until.is_some() {
            return Ok(Vec::new());
        }
        // Two ways in (0.9.4). Above the similarity floor a pair is a
        // look-alike and queues on similarity alone, hinted by the claim
        // text. Below it — down to the NLI floor — the logic layer reads the
        // two bare TITLES, and a confident contradiction between titles that
        // plausibly name the same subject queues too. Similarity alone
        // cannot tell a contradiction from an agreeing restatement (MemStrata
        // AUROC 0.59; the KnowledgeDrift gate probe reproduced it), and a
        // note's first body sentence only dilutes what the titles say.
        let nli_gate = cfg.policy.conflict_nli_gate.filter(|_| self.nli.is_some());
        let floor = match nli_gate {
            Some(_) => crate::policy::CONFLICT_NLI_FLOOR,
            None => cfg.policy.conflict_suspect_similarity,
        };
        let mut out = Vec::new();
        for (other, similarity) in near {
            let (other, similarity) = (other.clone(), *similarity);
            if similarity < floor {
                break; // distance-ordered: nothing closer follows
            }
            if self.pair_blocked(node, &other)? {
                continue;
            }
            let title_path = if similarity >= cfg.policy.conflict_suspect_similarity {
                None
            } else {
                if nli_gate.is_none() {
                    continue;
                }
                let Some(path) = title_pair_admission(&node.title, &other.title) else {
                    continue;
                };
                Some(path)
            };
            out.push(SuspectCandidate {
                other,
                similarity,
                title_path,
            });
        }
        Ok(out)
    }

    /// Whether a pair may not queue: the other side is an anchor, a tombstone
    /// or archived, or the two are already linked or already suspected.
    fn pair_blocked(&self, node: &Node, other: &Node) -> Result<bool> {
        let cfg = self.store.config();
        Ok(is_anchor(&cfg, other)
            || is_tombstone(&cfg, other)
            || other.valid_until.is_some()
            || self.store.pair_linked(&node.id, &other.id)?
            || self.store.suspect_between(&node.id, &other.id)?)
    }

    /// Queue judged pairs, each re-checked first: on the shared path the lock
    /// was free while the judge ran, so either side may have been archived,
    /// deleted, linked or queued since.
    fn queue_suspects(&self, node: &Node, judged: Vec<JudgedSuspect>) -> Result<usize> {
        if judged.is_empty() {
            return Ok(0);
        }
        let Some(node) = self.store.get_node(&node.id)? else {
            return Ok(0);
        };
        if node.valid_until.is_some() {
            return Ok(0);
        }
        let cfg = self.store.config();
        let mut added = 0;
        for (candidate, hint) in judged {
            let Some(other) = self.store.get_node(&candidate.other.id)? else {
                continue;
            };
            if self.pair_blocked(&node, &other)? {
                continue;
            }
            let (newer, older): (&Node, &Node) = if node.created_at >= other.created_at {
                (&node, &other)
            } else {
                (&other, &node)
            };
            self.store
                .add_suspect(&newer.id, &older.id, candidate.similarity, hint)?;
            added += 1;
            // A confident contradiction of the older note reaches whatever
            // inherits from it (0.9.9, the transitive shape): the judge read
            // one same-subject pair, the edges carry the rest.
            let inherit_gate = cfg
                .policy
                .conflict_nli_gate
                .unwrap_or(cfg.policy.nli_sweep_min_confidence);
            if let Some(("contradiction", score, _)) = hint
                && score >= inherit_gate
            {
                added += self.propagate_inherited(newer, &older.id, score)?;
            }
        }
        Ok(added)
    }

    /// The logic layer's triage hint for a candidate pair — a nomination for
    /// the judge, never a verdict (PLAN §7A: models don't validate). `None`
    /// when the NLI model isn't loaded or judgment fails (hints are
    /// best-effort). For contradiction hints the third element says which
    /// SIDE the model reads as carrying the negation, already mapped to
    /// `"newer"`/`"older"` by the nodes' own timestamps — the side that,
    /// judged as the hypothesis, contradicts hardest. Absent under a 0.15
    /// asymmetry margin — near-symmetric contradictions carry no direction
    /// worth showing.
    ///
    /// Public so the eval harness can score the suspect queue's judgment on a
    /// real graph's judged pairs. Calibrating that gate against a
    /// reimplementation of this composition would measure the
    /// reimplementation.
    pub fn nli_hint(&self, a: &Node, b: &Node) -> Option<Hint> {
        claim_hint(self.nli.as_deref()?, a, b)
    }

    /// The same hint read off the two bare TITLES (0.9.4) — the text of the
    /// claim itself, without the first body sentence [`claim`] appends. On
    /// the KnowledgeDrift gate probe the titles separated contradiction from
    /// restatement far more sharply than the claim text (negation 0.99 vs
    /// 0.61); this is what the below-floor nomination path reads.
    pub fn nli_title_hint(&self, a: &Node, b: &Node) -> Option<Hint> {
        title_hint(self.nli.as_deref()?, a, b)
    }

    /// [`nli_title_hint`] read twice when the titles carry a trailing clause
    /// (0.9.6): once whole, once cut to their first clauses
    /// ([`title_clause`]), the stronger contradiction winning. A title's
    /// tail ("— surfaced on the third occurrence", "; the earlier note was
    /// mistaken") is commentary the model weighs against the claim: on the
    /// KnowledgeDrift v2 gate probe the clause read lifted the `clause`
    /// shape from 0.04–0.24 to 0.58–0.98 and dropped two negatives (synonym
    /// 0.68→0.18, paraphrase 0.56→0.02).
    ///
    /// Only the UNNAMED admission path reads this way. On named pairs the
    /// same cut pushed seven same-subject-different-fact pairs over the gate
    /// across the two dogfood graphs (release notes, adapter write-ups —
    /// 2026-09-16 replays) where the whole-title read had let none through:
    /// a long real title's first clause is still a long sentence, and the
    /// tail it loses was what told the model the two facts differ.
    pub fn nli_title_hint_clause(&self, a: &Node, b: &Node) -> Option<Hint> {
        title_hint_clause(self.nli.as_deref()?, a, b)
    }

    /// The title read the sweep uses for a pair the guard admitted on
    /// `path` ("named" | "unnamed").
    pub fn nli_title_hint_for(&self, path: &str, a: &Node, b: &Node) -> Option<Hint> {
        title_hint_for(self.nli.as_deref()?, path, a, b)
    }

    /// The pending queue, ready for judgment.
    pub fn suspects(&self) -> Result<Vec<SuspectView>> {
        self.store.suspects_pending()
    }

    /// Score the "models nominate, people judge" loop: over every judged
    /// suspect pair, how often the local NLI hint (nli_label) matched the
    /// judge's verdict. Read-only — a report, never a calibration input
    /// (dial one reads similarity, not the hint).
    pub fn nli_agreement(&self) -> Result<NliAgreement> {
        let mut r = NliAgreement::default();
        for s in self.store.all_suspects()? {
            let confirmed = match s.status {
                SuspectStatus::Confirmed => true,
                SuspectStatus::Dismissed => false,
                SuspectStatus::Suspected => continue,
            };
            r.judged += 1;
            let Some(label) = s.nli_label.as_deref() else {
                continue;
            };
            r.with_hint += 1;
            match (label == "contradiction", confirmed) {
                (true, true) => r.hits += 1,
                (true, false) => r.false_alarms += 1,
                (false, true) => r.misses += 1,
                (false, false) => r.passes += 1,
            }
        }
        if r.with_hint > 0 {
            r.agreement = Some((r.hits + r.passes) as f64 / r.with_hint as f64);
        }
        Ok(r)
    }

    /// Tags in use, freshest first (the pane's dropdown; the brief's vocabulary).
    pub fn tags(&self, limit: usize) -> Result<Vec<TagStat>> {
        self.store.tag_stats(limit)
    }

    /// Judge a suspected pair. `conflict` records a `conflicts-with` edge;
    /// `replaces` records the edge *and* archives the older node (the
    /// supersede-not-delete flow, PLAN §6B); `dismiss` marks the pair judged
    /// so it is never re-raised. Already-judged suspects are a no-op.
    pub fn resolve_suspect(
        &self,
        id: &str,
        verdict: SuspectVerdict,
        source: Source,
    ) -> Result<Option<Edge>> {
        // One operation in the journal: undo reverts all of it (0.9.10).
        let _op = self.op_scope();
        let Some(suspect) = self.store.get_suspect(id)? else {
            return Err(crate::Error::NotFound(id.to_string()));
        };
        if suspect.status != SuspectStatus::Suspected {
            return Ok(None);
        }
        let edge = match verdict {
            SuspectVerdict::Dismiss => None,
            SuspectVerdict::Conflict => Some(self.add_edge(NewEdge {
                edge_type: self.store.config().contradiction_edge(),
                from_id: suspect.a_id.clone(),
                to_id: suspect.b_id.clone(),
                source,
                note: Some("confirmed from conflict scan".into()),
                confidence: Some(suspect.similarity),
                strength: None,
                status: None,
            })?),
            SuspectVerdict::Replaces => {
                // A pin is the user's "never fade" — an assistant verdict
                // must not archive it. Surface instead; the user can still
                // replace it from the pane (a user verdict proceeds).
                if source == Source::Agent
                    && let Some(older) = self.store.get_node(&suspect.b_id)?
                    && older.trust_override.is_some()
                {
                    return Err(crate::Error::Pinned(format!(
                        "\"{}\" ({}) is user-pinned; a replaces verdict would archive it — \
                         tell the user and let them judge this pair in the pane",
                        older.title, older.id
                    )));
                }
                let edge = self.add_edge(NewEdge {
                    edge_type: self.store.config().supersession_edge(),
                    from_id: suspect.a_id.clone(),
                    to_id: suspect.b_id.clone(),
                    source,
                    note: Some("confirmed from conflict scan".into()),
                    confidence: Some(suspect.similarity),
                    strength: None,
                    status: None,
                })?;
                // add_edge already retired the older node — unless it is
                // pinned, where the automatic path steps aside. A USER
                // verdict overrides the pin (the assistant case errored
                // above), so archive it here explicitly.
                if let Some(older) = self.store.get_node(&suspect.b_id)?
                    && older.valid_until.is_none()
                {
                    self.update_node(
                        &suspect.b_id,
                        NodePatch {
                            valid_until: Some(crate::store::now()),
                            ..NodePatch::default()
                        },
                    )?;
                }
                Some(edge)
            }
        };
        let status = match verdict {
            SuspectVerdict::Dismiss => SuspectStatus::Dismissed,
            _ => SuspectStatus::Confirmed,
        };
        self.store.set_suspect_status(id, status)?;
        self.notify(ChangeEvent::SuspectsChanged);
        Ok(edge)
    }

    /// The decay pass (PLAN §6B): archive Claude-authored, never-approved
    /// episodic/volatile nodes that have sat below the stale threshold for
    /// `ttl_days`. Dry-run reports without mutating.
    pub fn decay(&self, ttl_days: i64, dry_run: bool) -> Result<Vec<String>> {
        let now = crate::store::now();
        let candidates = self.store.decay_candidates(ttl_days * 24 * 60 * 60, now)?;
        let ids: Vec<String> = candidates.iter().map(|n| n.id.clone()).collect();
        if dry_run || ids.is_empty() {
            return Ok(ids);
        }
        self.store.archive_nodes(&ids, now)?;
        for candidate in &candidates {
            if let Some(node) = self.store.get_node(&candidate.id)? {
                self.audit_node("archived", Some(candidate), Some(&node))?;
                self.notify(ChangeEvent::NodeUpdated(node));
            }
        }
        Ok(ids)
    }

    fn embed_node(&self, node: &Node) -> Result<()> {
        self.embed_node_with(node, None)
    }

    /// [`Engine::embed_node`], taking vectors a write computed before the
    /// lock wherever their texts match (0.9.11).
    fn embed_node_with(&self, node: &Node, cache: Option<&EmbedCache>) -> Result<()> {
        let texts = node_texts(
            &self.store.config(),
            &node.title,
            node.body.as_deref(),
            &node.tags,
            &node.code_refs,
            node.fields.as_ref(),
        );
        let vectors = self.embed_texts(&texts, cache)?;
        self.store.upsert_embeddings(&node.id, &vectors)
    }

    /// Embed `texts`, taking each from `cache` when it holds exactly that
    /// text under the ACTIVE model — anything else (a model swapped since
    /// the cache was filled, a text the prediction missed) is embedded now.
    /// A cache can only make a write faster, never different.
    pub(crate) fn embed_texts(
        &self,
        texts: &[String],
        cache: Option<&EmbedCache>,
    ) -> Result<Vec<Vec<f32>>> {
        let Some(cache) = cache.filter(|c| c.model == self.embed_model_id()) else {
            return self.embedder.embed(texts);
        };
        let mut missing: Vec<String> = Vec::new();
        for t in texts {
            if !cache.vecs.contains_key(t) && !missing.contains(t) {
                missing.push(t.clone());
            }
        }
        let fresh: std::collections::HashMap<String, Vec<f32>> = if missing.is_empty() {
            Default::default()
        } else {
            let vecs = self.embedder.embed(&missing)?;
            missing.into_iter().zip(vecs).collect()
        };
        Ok(texts
            .iter()
            .map(|t| {
                cache
                    .vecs
                    .get(t)
                    .or_else(|| fresh.get(t))
                    .cloned()
                    .expect("every text was cached or embedded")
            })
            .collect())
    }

    fn embed_one_with(&self, text: &str, cache: Option<&EmbedCache>) -> Result<Vec<f32>> {
        Ok(self
            .embed_texts(&[text.to_string()], cache)?
            .pop()
            .expect("one vector for one text"))
    }

    /// Bring stored vectors in line with the ACTIVE embedding model (PLAN §7A
    /// model selection), returning how many nodes were re-embedded. A store
    /// records the identity its vectors were computed with; when the active
    /// model differs — different name or width — vector storage is rebuilt
    /// for the new width and the whole graph re-embeds. Skipped entirely
    /// under a fake embedder (fake vectors must never replace real ones), so
    /// a `--fake-embeddings` open can never mass-destroy a graph's vectors.
    pub fn ensure_embed_model(&self) -> Result<usize> {
        if self.embedder.is_fake() {
            return Ok(0);
        }
        let active = self.embed_model_id();
        let stored = self.store.embed_model()?;
        // Stores that predate model selection carry no identity: they are the
        // default model by construction — stamp, don't re-embed.
        let effective = stored.clone().unwrap_or(EmbedModelId {
            name: crate::rag::DEFAULT_EMBED_MODEL.to_string(),
            dim: crate::rag::EMBED_DIM,
        });
        if effective == active {
            if stored.is_none() {
                self.store.set_embed_model(&active)?;
            }
            // Same identity, but a swap that died mid-loop may have left
            // gaps — backfill any node without a vector so every open heals.
            let mut healed = 0;
            for n in self.store.all_nodes()? {
                if self.store.embedding_of(&n.id)?.is_none() {
                    self.embed_node(&n)?;
                    healed += 1;
                }
            }
            return Ok(healed);
        }
        self.store.reset_vectors(active.dim)?;
        // Record the new identity BEFORE the loop: the TepinDB backend stamps
        // each written vector with the store's recorded model, and the file
        // pins itself to whatever the first write says — stamping after the
        // loop would pin the file under the OLD name and poison every later
        // write with embedder_mismatch (bit us live on the first real swap).
        self.store.set_embed_model(&active)?;
        let nodes = self.store.all_nodes()?;
        for n in &nodes {
            self.embed_node(n)?;
        }
        // A full re-embed is by definition the current composition too.
        self.store.set_embed_version(EMBED_COMPOSITION)?;
        Ok(nodes.len())
    }

    /// Bring stored vectors up to the current [`EMBED_COMPOSITION`], returning
    /// how many nodes were re-embedded (0 = already current or skipped).
    /// Skipped with a fake embedder over a non-empty graph — fake vectors must
    /// never replace real ones, and the brief hook routinely opens real DBs
    /// with `--fake-embeddings`. Idempotent; stamps the version when done.
    pub fn ensure_embed_composition(&self) -> Result<usize> {
        if self.store.embed_version()? >= EMBED_COMPOSITION {
            return Ok(0);
        }
        let nodes = self.store.all_nodes()?;
        if self.embedder.is_fake() && !nodes.is_empty() {
            return Ok(0);
        }
        // The composition change also reshapes the vector layout (claim
        // chunks since v3) — clear storage once, then rebuild it whole.
        self.store.reset_vectors(self.embedder.dim())?;
        for n in &nodes {
            self.embed_node(n)?;
        }
        self.store.set_embed_version(EMBED_COMPOSITION)?;
        Ok(nodes.len())
    }
}

/// Drive one store from its recorded encryption state to the desired one
/// (0.9.0). `None` desired = hands off (library engines and tests never
/// migrate or touch the keyring). The mid-flight state persists BEFORE the
/// rewrite, so new writes already conform and a crash resumes cleanly.
fn reconcile_encryption(
    store: &dyn Store,
    desired: Option<bool>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<usize> {
    use crate::seal::EncryptionState as S;
    let target = match desired {
        Some(true) => S::Sealed,
        Some(false) => S::Plaintext,
        None => return Ok(0),
    };
    if store.encryption_state() == target {
        return Ok(0);
    }
    store.set_encryption_state(if target == S::Sealed {
        S::Sealing
    } else {
        S::Unsealing
    })?;
    let n = store.reseal_all(progress)?;
    store.set_encryption_state(target)?;
    Ok(n)
}

/// Appended to the brief when the graph is empty, so a cold start reads as an
/// actionable instruction to the assistant rather than an empty digest.
const COLD_START_BRIEF: &str = "\nThe graph is empty — this is a cold start.\n\n\
Offer the user a one-time seeding pass (ask first; this is the one capture \
that must not be silent): read the project's existing canon — README, \
design/plan docs, recent git history — and batch-capture the durable \
knowledge as provisional nodes: key Decisions with their reasons (`because` \
edges), stated Principles and conventions, known Cautions, and open Intents, \
attached to Anchors where several notes share a subject. Afterward, point the \
user at the pane to review what was captured. If the user declines, don't ask \
again — just capture knowledge as it emerges.\n\n\
Before seeding, review the settings with the user: `GET /guide` on the core \
(port in `.engram/daemon.json`) is the manual — does the shipped ontology fit, \
or would a preset, a custom set, or custom fields fit better? A customized \
graph wants `brief.ontology.show` on, so every session learns its vocabulary.\n";

/// Longest `replaces`-path from a timeline node down to an original (which is
/// generation 0). Memoized; a cycle (bad data) counts as 0 instead of hanging.
fn generation<'a>(
    id: &'a str,
    adj: &std::collections::HashMap<&'a str, Vec<&'a str>>,
    memo: &mut std::collections::HashMap<&'a str, usize>,
) -> usize {
    if let Some(&g) = memo.get(id) {
        return g;
    }
    memo.insert(id, 0); // cycle guard while this node is being computed
    let g = adj
        .get(id)
        .map(|olders| {
            olders
                .iter()
                .map(|o| generation(o, adj, memo) + 1)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    memo.insert(id, g);
    g
}

/// Sentence-sized claim texts for retrieval (v0.6.3 claim-level search):
/// the tokenizer splits the body on sentence boundaries and keeps units
/// substantial enough to mean something alone — each becomes its own vector
/// next to the node-level one, mirroring how the NLI layer already judges
/// claims instead of whole bodies. Title rides along with each claim so a
/// sentence keeps its subject.
pub(crate) fn claim_texts(title: &str, body: Option<&str>) -> Vec<String> {
    const MIN_CHARS: usize = 30;
    const MAX_CLAIMS: usize = 12;
    let Some(body) = body.filter(|b| b.trim().len() >= MIN_CHARS) else {
        return Vec::new();
    };
    let mut claims = Vec::new();
    let mut current = String::new();
    let flat = body.replace('\n', " ");
    let mut chars = flat.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        let boundary =
            matches!(c, '.' | '!' | '?' | ';') && chars.peek().is_none_or(|n| n.is_whitespace());
        if boundary || chars.peek().is_none() {
            let sentence = current.trim();
            if sentence.len() >= MIN_CHARS {
                claims.push(format!("{title}. {sentence}"));
                if claims.len() == MAX_CLAIMS {
                    break;
                }
            }
            current.clear();
        }
    }
    // A single claim adds nothing over the composition vector.
    if claims.len() <= 1 {
        Vec::new()
    } else {
        claims
    }
}

/// A node's canonical claim for NLI judgment (PLAN §7A): the declarative,
/// skill-enforced title, plus the body's first sentence when it adds context.
/// Claim-level on purpose — whole multi-claim bodies dilute a sentence-pair
/// model past usefulness, however large its context window.
/// Label, confidence and — for contradictions — which SIDE carries the
/// negation, mapped to `"newer"`/`"older"` by the nodes' own timestamps: the
/// side that, judged as the hypothesis, contradicts hardest. Absent under a
/// 0.15 asymmetry margin — near-symmetric contradictions carry no direction
/// worth showing.
fn hint_with_direction(
    sym: &crate::nli::SymmetricJudgment,
    a: &Node,
    b: &Node,
) -> (&'static str, f64, Option<&'static str>) {
    const DIRECTION_MARGIN: f32 = 0.15;
    let (label, score) = sym.hint();
    let direction = if label == "contradiction" {
        // forward = (a premise → b hypothesis): high forward contradiction
        // reads b as the negated claim.
        let carrier = if sym.forward.contradiction >= sym.backward.contradiction + DIRECTION_MARGIN
        {
            Some(b)
        } else if sym.backward.contradiction >= sym.forward.contradiction + DIRECTION_MARGIN {
            Some(a)
        } else {
            None
        };
        let a_is_newer = a.created_at >= b.created_at;
        carrier.map(|c| {
            if std::ptr::eq(c, a) == a_is_newer {
                "newer"
            } else {
                "older"
            }
        })
    } else {
        None
    };
    (label, score as f64, direction)
}

/// Sentence starters that are capitalised for being first, not for being a
/// name — a title's first word is a subject candidate only when it is not
/// one of these.
const SUBJECT_STARTERS: [&str; 40] = [
    "it", "in", "the", "until", "every", "before", "after", "when", "if", "a", "an", "on", "at",
    "for", "to", "not", "no", "never", "always", "there", "this", "that", "these", "those", "we",
    "our", "one", "all", "any", "some", "use", "do", "under", "since", "while", "with", "without",
    "as", "by", "from",
];

/// Capitalised tokens that could name what a title is about.
fn subject_tokens(title: &str) -> std::collections::HashSet<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .enumerate()
        .filter(|(i, w)| {
            w.chars().next().is_some_and(char::is_uppercase)
                && !(*i == 0 && SUBJECT_STARTERS.contains(&w.to_lowercase().as_str()))
        })
        .map(|(_, w)| w.to_lowercase())
        .collect()
}

/// Do two titles speak about the same thing? The co-reference guard for
/// NLI: MNLI-trained models presuppose the premise and hypothesis co-refer,
/// so "Kelnor broker uses 7" against "Vanor broker uses 19" reads as a
/// contradiction when it is two facts about two things (RefNLI).
///
/// Two ways to pass, and the tokens of the subject phrase come back so the
/// overlap count can leave them out:
///
/// - **named**: both titles carry capitalised name tokens and share one
///   (the 0.9.4 strict guard — the lenient "no name on either side, let the
///   model decide" form raised 56 false alarms on the dogfood graph);
/// - **unnamed** (0.9.6): neither side has a usable name, or only one does,
///   and the titles share a contiguous run of at least two content words
///   that no differing content word qualifies on either side — "hazel bayou
///   assay station …" against "wheat dale assay station …" share the
///   component, but `bayou` and `dale` in front of it name two things, and
///   the pair is refused. Lowercase subjects ("amber harbor lease broker")
///   are the register the 0.9.4 guard was blind to: KnowledgeDrift v2
///   dropped tier-1 contradiction recall from 0.94 to 0.04 on them.
///
/// `None` = not the same subject as far as the titles can tell: no NLI
/// nomination.
pub(crate) fn shared_subject(a: &str, b: &str) -> Option<std::collections::HashSet<String>> {
    // Names are read from the first clause only: the subject leads the
    // claim, and a name in a title's tail ("… — both signed JetBrains zips
    // published") is a detail, not what the title is about. Two release
    // notes that both mention the plugin in passing are not two claims
    // about the plugin (dogfood replay, 2026-09-16).
    let (sa, sb) = (
        subject_tokens(title_clause(a)),
        subject_tokens(title_clause(b)),
    );
    if !sa.is_empty() && !sb.is_empty() {
        return (!sa.is_disjoint(&sb)).then(|| sa.union(&sb).cloned().collect());
    }
    // The run is looked for in the first clauses too: a phrase two titles
    // share in their tails ("… — Release green with nine assets" / "…; all
    // nine assets") is a detail they have in common, not a subject.
    let (ta, tb) = (title_tokens(title_clause(a)), title_tokens(title_clause(b)));
    let is_content =
        |w: &str| w.len() >= 3 && w.chars().any(char::is_alphabetic) && !is_function_word(w);
    // Every maximal common run, earliest in `a` first: the subject phrase
    // leads a claim more often than it trails one.
    for i in 0..ta.len() {
        for j in 0..tb.len() {
            if ta[i] != tb[j] || (i > 0 && j > 0 && ta[i - 1] == tb[j - 1]) {
                continue; // not a run start, or not maximal
            }
            let len = ta[i..]
                .iter()
                .zip(&tb[j..])
                .take_while(|(x, y)| x == y)
                .count();
            if len < 2 || ta[i..i + len].iter().filter(|w| is_content(w)).count() < 2 {
                continue;
            }
            // A differing content word right before the run on either side
            // is a modifier that makes the subject a different one.
            let qualified = |t: &[String], k: usize| k > 0 && is_content(&t[k - 1]);
            if qualified(&ta, i) || qualified(&tb, j) {
                continue;
            }
            return Some(
                ta[i..i + len.min(SUBJECT_PHRASE_MAX)]
                    .iter()
                    .cloned()
                    .collect(),
            );
        }
    }
    None
}

/// Do two titles demonstrably name DIFFERENT things? The refusing half of
/// [`shared_subject`], for the duplicate match: named on both sides with no
/// name in common, or a shared component run that a differing content word
/// qualifies on either side ("hazel quarry edge cache …" against "wheat
/// geyser edge cache …"). `false` when the titles are undecidable — no
/// names, no shared run — because a rewritten title with the same body is
/// still the duplicate it looks like.
pub(crate) fn subjects_differ(a: &str, b: &str) -> bool {
    let (sa, sb) = (
        subject_tokens(title_clause(a)),
        subject_tokens(title_clause(b)),
    );
    if !sa.is_empty() && !sb.is_empty() {
        return sa.is_disjoint(&sb);
    }
    let (ta, tb) = (title_tokens(title_clause(a)), title_tokens(title_clause(b)));
    let is_content =
        |w: &str| w.len() >= 3 && w.chars().any(char::is_alphabetic) && !is_function_word(w);
    let mut qualified_run = false;
    for i in 0..ta.len() {
        for j in 0..tb.len() {
            if ta[i] != tb[j] || (i > 0 && j > 0 && ta[i - 1] == tb[j - 1]) {
                continue;
            }
            let len = ta[i..]
                .iter()
                .zip(&tb[j..])
                .take_while(|(x, y)| x == y)
                .count();
            if len < 2 || ta[i..i + len].iter().filter(|w| is_content(w)).count() < 2 {
                continue;
            }
            let qualified = |t: &[String], k: usize| k > 0 && is_content(&t[k - 1]);
            if qualified(&ta, i) || qualified(&tb, j) {
                qualified_run = true;
            } else {
                return false; // an unqualified shared run: the same subject
            }
        }
    }
    qualified_run
}

/// Longest subject phrase the unnamed guard keeps out of the overlap count:
/// four words covers "amber harbor lease broker"; past that a shared run is
/// the claim itself and its words must stay in the count.
const SUBJECT_PHRASE_MAX: usize = 4;

/// A title's first clause: what precedes an em dash or a semicolon. The
/// claim leads; what follows is commentary on it.
pub fn title_clause(t: &str) -> &str {
    let t = t.split(" — ").next().unwrap_or(t);
    t.split("; ").next().unwrap_or(t).trim()
}

/// Lowercase alphanumeric tokens of a title.
fn title_tokens(t: &str) -> Vec<String> {
    t.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn is_function_word(w: &str) -> bool {
    CONTENT_STOP.contains(&w) || SUBJECT_STARTERS.contains(&w)
}

/// May the NLI path read this pair of titles? Same subject
/// ([`shared_subject`]) and at least [`crate::policy::CONFLICT_NLI_OVERLAP`]
/// content words shared beyond it ([`content_overlap`]). Public so the
/// bench's gate probe measures the shipped rule rather than a copy of it.
pub fn title_pair_admissible(a: &str, b: &str) -> bool {
    title_pair_admission(a, b).is_some()
}

/// [`title_pair_admissible`] with the reason: `"named"` when both titles
/// carry a shared capitalised name, `"unnamed"` when the lowercase run
/// guard admitted them, `None` when refused. For probes and replays that
/// compare the two paths.
pub fn title_pair_admission(a: &str, b: &str) -> Option<&'static str> {
    let (sa, sb) = (
        subject_tokens(title_clause(a)),
        subject_tokens(title_clause(b)),
    );
    let named = !sa.is_empty() && !sb.is_empty();
    let subject = shared_subject(a, b)?;
    (content_overlap(a, b, &subject) >= crate::policy::CONFLICT_NLI_OVERLAP).then_some(if named {
        "named"
    } else {
        "unnamed"
    })
}

/// Function words that carry no claim, for the overlap count.
const CONTENT_STOP: [&str; 60] = [
    "the",
    "a",
    "an",
    "is",
    "are",
    "was",
    "were",
    "it",
    "its",
    "in",
    "on",
    "of",
    "for",
    "to",
    "and",
    "or",
    "that",
    "this",
    "what",
    "which",
    "who",
    "how",
    "did",
    "does",
    "do",
    "we",
    "our",
    "us",
    "you",
    "be",
    "been",
    "with",
    "from",
    "by",
    "at",
    "as",
    "if",
    "not",
    "no",
    "yes",
    "about",
    "into",
    "than",
    "then",
    "so",
    "up",
    "case",
    "every",
    "without",
    "exception",
    "any",
    "all",
    "one",
    "until",
    "after",
    "before",
    "has",
    "have",
    "had",
    "never",
];

/// Content words two titles share beyond their subject: the titles' common
/// leading words — at most three, the subject phrase — are stripped first
/// (a multi-word subject such as "Kelnor lease broker" is not a shared
/// claim), then lowercase alphanumeric runs
/// of three or more characters — function words and subject tokens out —
/// are compared on their first five characters so `attempt` and `attempts`
/// agree. A restated claim shares its parameter or predicate; two
/// unrelated facts about one subject share the subject and nothing else.
pub(crate) fn content_overlap(
    a: &str,
    b: &str,
    subject: &std::collections::HashSet<String>,
) -> usize {
    let (ta, tb) = (title_tokens(a), title_tokens(b));
    // The subject phrase is the common prefix, capped at three words: past
    // that a shared prefix is the claim itself with a clause appended, and
    // the claim's words must stay in the count.
    let prefix = ta
        .iter()
        .zip(&tb)
        .take_while(|(x, y)| x == y)
        .count()
        .min(3);
    let words = |t: &[String]| -> std::collections::HashSet<String> {
        t.iter()
            .filter(|w| {
                w.len() >= 3 && !CONTENT_STOP.contains(&w.as_str()) && !subject.contains(*w)
            })
            .map(|w| w.chars().take(5).collect())
            .collect()
    };
    words(&ta[prefix..])
        .intersection(&words(&tb[prefix..]))
        .count()
}

fn claim(node: &Node) -> String {
    let mut text = node.title.trim().to_string();
    if let Some(body) = node.body.as_deref() {
        let first = body
            .trim()
            .replace('\n', " ")
            .split(". ")
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if !first.is_empty() && !text.to_lowercase().contains(&first.to_lowercase()) {
            text.push_str(". ");
            text.push_str(&first);
        }
    }
    text.chars().take(400).collect()
}

/// A code_ref reads as a checkable path when it has no whitespace and looks
/// filesystem-shaped (a separator or an extension dot); "auth flow"-style
/// responsibility labels fail this and are skipped by the drift scan.
fn ref_is_path(r: &str) -> bool {
    !r.is_empty() && !r.contains(char::is_whitespace) && (r.contains('/') || r.contains('.'))
}

/// Which embedding composition stored vectors were computed with (kept in
/// `PRAGMA user_version`). Bump when [`embed_text`] changes what it includes;
/// [`Engine::ensure_embed_composition`] re-embeds databases that are behind.
/// 0 = legacy title+body; 2 = full fields (title, body, tags, code_refs);
/// 3 = claim-chunked (the node-level vector plus one vector per body
/// sentence, so a query matching one claim in a rich body finds the node).
pub const EMBED_COMPOSITION: i64 = 3;

/// Every text a node is embedded as, in storage order: the node-level
/// composition (with the graph's indexed custom fields, 0.9.0) first, then
/// one text per claim sentence.
fn node_texts(
    cfg: &crate::config::GraphConfig,
    title: &str,
    body: Option<&str>,
    tags: &[String],
    code_refs: &[String],
    fields: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Vec<String> {
    let mut composed = embed_text(title, body, tags, code_refs);
    let fields_text = fields_embed_text(cfg, fields);
    if !fields_text.is_empty() {
        composed.push('\n');
        composed.push_str(&fields_text);
    }
    let mut texts = vec![composed];
    texts.extend(claim_texts(title, body));
    texts
}

/// Vectors a checked write computed BEFORE taking the engine lock (0.9.11,
/// Insight 00dxfvuv1j1f: ~144 of the ~175 ms a write held the lock at 1500
/// notes was embedding). The write predicts every text the store and its
/// checks will embed — the same scrub and tag normalization the store
/// applies — and embeds them with the lock released; under the lock
/// [`Engine::embed_texts`] takes a vector only for a byte-identical text
/// under the same model, and embeds anything else on the spot. A wrong
/// prediction, a model swapped mid-write or a field index changed meanwhile
/// cost a little time, never a different vector.
pub(crate) struct EmbedCache {
    model: EmbedModelId,
    vecs: std::collections::HashMap<String, Vec<f32>>,
}

impl EmbedCache {
    /// Embed `texts` (duplicates once). `None` when embedding fails — the
    /// write then embeds under the lock and reports the error there.
    fn fill(embedder: &dyn Embedder, model: EmbedModelId, texts: Vec<String>) -> Option<Self> {
        let mut unique: Vec<String> = Vec::with_capacity(texts.len());
        for t in texts {
            if !unique.contains(&t) {
                unique.push(t);
            }
        }
        let vecs = embedder.embed(&unique).ok()?;
        (vecs.len() == unique.len()).then(|| Self {
            model,
            vecs: unique.into_iter().zip(vecs).collect(),
        })
    }

    /// What a checked add will embed: the duplicate check's text, then the
    /// stored node's texts as the store will shape the node.
    pub(crate) fn for_add(
        embedder: &dyn Embedder,
        model: EmbedModelId,
        cfg: &crate::config::GraphConfig,
        n: &NewNode,
    ) -> Option<Self> {
        let title = crate::redact::scrub(&n.title);
        let body = n.body.as_deref().map(crate::redact::scrub);
        let fields = n
            .fields
            .as_ref()
            .filter(|f| !f.is_empty())
            .map(crate::redact::scrub_fields);
        let mut texts = vec![embed_text(&title, body.as_deref(), &n.tags, &n.code_refs)];
        texts.extend(node_texts(
            cfg,
            &title,
            body.as_deref(),
            &crate::store::normalize_tags(&n.tags),
            &n.code_refs,
            fields.as_ref(),
        ));
        Self::fill(embedder, model, texts)
    }

    /// What a checked update will embed, read off the stored node as the
    /// patch will leave it. `None` when the patch touches no embedded text.
    pub(crate) fn for_update(
        embedder: &dyn Embedder,
        model: EmbedModelId,
        cfg: &crate::config::GraphConfig,
        before: &Node,
        patch: &NodePatch,
    ) -> Option<Self> {
        let touches_text = patch.title.is_some()
            || patch.body.is_some()
            || patch.tags.is_some()
            || patch.code_refs.is_some()
            || patch.fields.is_some();
        if !touches_text {
            return None;
        }
        let title = patch
            .title
            .as_deref()
            .map(crate::redact::scrub)
            .unwrap_or_else(|| before.title.clone());
        let body = match &patch.body {
            Some(b) => Some(crate::redact::scrub(b)),
            None => before.body.clone(),
        };
        let tags = match &patch.tags {
            Some(t) => crate::store::normalize_tags(t),
            None => before.tags.clone(),
        };
        let code_refs = patch.code_refs.as_ref().unwrap_or(&before.code_refs);
        let fields = match &patch.fields {
            Some(p) => {
                let resolved = Engine::resolve_fields_patch(before.fields.as_ref(), p);
                (!resolved.is_empty()).then(|| crate::redact::scrub_fields(&resolved))
            }
            None => before.fields.clone(),
        };
        let mut texts = vec![embed_text(&title, body.as_deref(), &tags, code_refs)];
        texts.extend(node_texts(
            cfg,
            &title,
            body.as_deref(),
            &tags,
            code_refs,
            fields.as_ref(),
        ));
        Self::fill(embedder, model, texts)
    }
}

/// The text a node is embedded as — kept in one place so write-time similarity
/// checks embed exactly what storage embeds. Tags and code_refs ride along so
/// "everything about policy.rs" works as a semantic query, not only a keyword
/// one; title+body still dominate the vector, so dupe detection is unaffected.
fn embed_text(title: &str, body: Option<&str>, tags: &[String], code_refs: &[String]) -> String {
    let mut text = title.to_string();
    if let Some(b) = body.filter(|b| !b.is_empty()) {
        text.push('\n');
        text.push_str(b);
    }
    if !tags.is_empty() {
        text.push('\n');
        text.push_str(&tags.join(" "));
    }
    if !code_refs.is_empty() {
        text.push('\n');
        text.push_str(&code_refs.join(" "));
    }
    text
}

/// The `name: value` lines a node's INDEXED custom fields contribute to its
/// embedding (0.9.0). Per-graph, so it rides beside [`embed_text`] rather
/// than inside it: the global [`EMBED_COMPOSITION`] stays honest for graphs
/// without indexed fields, and the per-graph fingerprint guard
/// ([`Engine::ensure_field_index`]) re-embeds when a graph's set changes.
fn fields_embed_text(
    cfg: &crate::config::GraphConfig,
    fields: Option<&serde_json::Map<String, serde_json::Value>>,
) -> String {
    let Some(fields) = fields else {
        return String::new();
    };
    let mut out = String::new();
    for f in cfg.fields.iter().filter(|f| f.indexed) {
        if let Some(v) = fields.get(&f.name) {
            let text = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("{}: {}", f.name, text));
        }
    }
    out
}

/// Does `value` satisfy a field definition's kind? Values are scalars only —
/// arrays/objects are refused for every kind, so redaction, hoisting, and
/// the pane can all treat them as displayable atoms.
fn field_value_ok(def: &crate::config::FieldDef, value: &serde_json::Value) -> Result2<(), String> {
    use crate::config::FieldKind::*;
    use serde_json::Value;
    match def.kind {
        Text => match value {
            Value::String(_) => Ok(()),
            _ => Err("expected a string".into()),
        },
        Url => match value {
            Value::String(s) if s.contains("://") => Ok(()),
            Value::String(_) => Err("expected a URL (scheme://…)".into()),
            _ => Err("expected a URL string".into()),
        },
        Number => match value {
            Value::Number(_) => Ok(()),
            _ => Err("expected a number".into()),
        },
        Bool => match value {
            Value::Bool(_) => Ok(()),
            _ => Err("expected true or false".into()),
        },
        Date => match value {
            Value::Number(n) if n.is_i64() => Ok(()),
            Value::String(s)
                if s.len() >= 10
                    && s.as_bytes()[..4].iter().all(u8::is_ascii_digit)
                    && crate::timespec::parse_instant(s, crate::store::now()).is_some() =>
            {
                Ok(())
            }
            Value::String(_) => {
                Err("expected \"YYYY-MM-DD\", an ISO instant, or unix seconds".into())
            }
            _ => Err("expected a date string or unix seconds".into()),
        },
        Enum => match value {
            Value::String(s) if def.values.iter().any(|v| v == s) => Ok(()),
            Value::String(s) => Err(format!("{s:?} is not one of: {}", def.values.join(" | "))),
            _ => Err("expected one of the declared enum values (a string)".into()),
        },
    }
}

/// Local alias so [`field_value_ok`] can return a plain-string error without
/// colliding with the crate-wide `Result`.
type Result2<T, E> = std::result::Result<T, E>;

/// Longest excerpt a brief line carries. Word-boundary cut, so lines read as
/// prose, not as a mid-token truncation. Tuned down from 240 on the dogfood
/// graph: at 240 the budget died mid-Cautions; ~140 still carries the leading
/// sentence and lets every section (and its overflow counts) surface —
/// breadth over depth, since the full node is one `search` away.
pub const EXCERPT_CHARS: usize = 140;

/// One brief line per node, one uniform shape everywhere:
/// `- Title [Type id status STALE] — excerpt`. Every record carries its id so
/// the assistant can act on it directly (`get_node`, `traverse`,
/// `update_node`) without a `search` round-trip.
pub fn node_line(n: &Node, excerpt_max: usize) -> String {
    node_line_cfg(n, excerpt_max, None, None)
}

/// [`node_line`] with the graph's config in hand: custom fields flagged
/// `show_in_brief` render as `{name: value; …}` before the excerpt (0.9.0).
/// A version stamp equal to `working` is left out (0.10.0): the brief's first
/// line already names the working version, so only older stamps say anything.
pub fn node_line_cfg(
    n: &Node,
    excerpt_max: usize,
    cfg: Option<&crate::config::GraphConfig>,
    working: Option<&str>,
) -> String {
    let mut line = format!("- {} [{} {}", n.title, n.node_type.as_str(), n.id);
    if let Some(version) = n.version.as_deref().filter(|v| Some(*v) != working) {
        line.push(' ');
        line.push_str(version);
    }
    if let Some(status) = n.status {
        line.push(' ');
        line.push_str(status.as_str());
    }
    if n.trust_override.is_some() {
        line.push_str(" PINNED");
    }
    if n.stale {
        line.push_str(" STALE");
    }
    line.push(']');
    if let Some(cfg) = cfg
        && let Some(fields) = &n.fields
    {
        let shown: Vec<String> = cfg
            .fields
            .iter()
            .filter(|f| f.show_in_brief)
            .filter_map(|f| {
                fields.get(&f.name).map(|v| {
                    let text = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    format!("{}: {}", f.name, text)
                })
            })
            .collect();
        if !shown.is_empty() {
            line.push_str(&format!(" {{{}}}", shown.join("; ")));
        }
    }
    if let Some(body) = n
        .body
        .as_deref()
        .filter(|b| !b.is_empty() && excerpt_max > 0)
    {
        line.push_str(" — ");
        line.push_str(&excerpt_words(&body.replace('\n', " "), excerpt_max));
    }
    line
}

/// A logic-layer triage hint: label, score, and — for contradictions — the
/// side carrying the negation (see [`Engine::nli_hint`]).
pub type Hint = (&'static str, f64, Option<&'static str>);

/// How many nearest neighbours the write-time canon check judges.
const CANON_CHECK_CAP: usize = 5;
const CANON_SUPPORT: f32 = 0.6;
const CANON_CONTRADICTION: f32 = 0.7;

/// A checked write's locked first half: a near-duplicate to hand back, or a
/// created note with the pairs its checks must judge.
enum WritePlan {
    Matched(Box<MatchedPlan>),
    Created(Box<CreatedPlan>),
}

struct MatchedPlan {
    node: Node,
    similarity: f64,
    /// The new text as the judge reads it against the match.
    excerpt: String,
}

impl MatchedPlan {
    /// At duplicate similarity co-reference holds, so an NLI contradiction
    /// is trustworthy — it flags the negated near-duplicate a cosine score
    /// can't see.
    fn judge(self, nli: Option<&dyn Nli>) -> WriteOutcome {
        let (nli_label, nli_score) =
            match nli.and_then(|n| n.judge_pair(&self.excerpt, &claim(&self.node)).ok()) {
                Some(sym) => {
                    let (l, s) = sym.hint();
                    (Some(l.to_string()), Some(s as f64))
                }
                None => (None, None),
            };
        WriteOutcome::Matched {
            node: self.node,
            similarity: self.similarity,
            nli_label,
            nli_score,
        }
    }
}

/// A written note (created or updated) plus everything its checks read from
/// the store — gathered under the engine lock, judged without it.
struct CreatedPlan {
    node: Node,
    warnings: Vec<WriteWarning>,
    missing_refs: Vec<String>,
    suspects: Vec<SuspectCandidate>,
    canon: Vec<(Node, f64)>,
}

impl CreatedPlan {
    /// The lock-free half: every logic-layer call a write makes.
    fn judge(self, nli: Option<&dyn Nli>, cfg: &crate::config::GraphConfig) -> JudgedPlan {
        let suspects = judge_suspects(nli, cfg, &self.node, self.suspects);
        let canon = judge_canon(
            nli,
            cfg.policy.conflict_suspect_similarity,
            &claim(&self.node),
            self.canon,
        );
        JudgedPlan {
            node: self.node,
            warnings: self.warnings,
            missing_refs: self.missing_refs,
            suspects,
            canon,
        }
    }
}

struct JudgedPlan {
    node: Node,
    warnings: Vec<WriteWarning>,
    missing_refs: Vec<String>,
    suspects: Vec<JudgedSuspect>,
    canon: Vec<CanonVerdict>,
}

/// One pair the conflict scan will judge. `title_path` is set for a
/// below-floor pair the subject guard admitted ("named" | "unnamed"): it
/// queues only on a confident title contradiction.
struct SuspectCandidate {
    other: Node,
    similarity: f64,
    title_path: Option<&'static str>,
}

/// A judged candidate that queues, with its hint.
type JudgedSuspect = (SuspectCandidate, Option<Hint>);

/// Judge suspect candidates — no engine, no lock. A look-alike always
/// queues, hinted when the judge is loaded; a below-floor pair queues only
/// on a title contradiction at or above the gate.
fn judge_suspects(
    nli: Option<&dyn Nli>,
    cfg: &crate::config::GraphConfig,
    node: &Node,
    candidates: Vec<SuspectCandidate>,
) -> Vec<JudgedSuspect> {
    let gate = cfg.policy.conflict_nli_gate;
    candidates
        .into_iter()
        .filter_map(|c| {
            let hint = match c.title_path {
                None => nli.and_then(|n| claim_hint(n, node, &c.other)),
                Some(path) => {
                    let (nli, gate) = (nli?, gate?);
                    match title_hint_for(nli, path, node, &c.other) {
                        Some(h) if h.0 == "contradiction" && h.1 >= gate => Some(h),
                        _ => return None,
                    }
                }
            };
            Some((c, hint))
        })
        .collect()
}

/// The write-time canon check (PLAN §7A): judge the fresh text against its
/// nearest existing knowledge. Entailment is directional and cheap to trust
/// — `supports` says the canon already backs this claim (link it, or wonder
/// why it needed rewriting). `contradicts` is only issued inside the suspect
/// similarity band, where the co-reference presupposition holds — below it
/// an MNLI verdict is noise.
fn judge_canon(
    nli: Option<&dyn Nli>,
    suspect_similarity: f64,
    text: &str,
    candidates: Vec<(Node, f64)>,
) -> Vec<CanonVerdict> {
    let Some(nli) = nli else {
        return Vec::new();
    };
    let excerpt: String = text.chars().take(400).collect();
    let mut out = Vec::new();
    for (node, similarity) in candidates {
        let Ok(j) = nli.judge_pair(&claim(&node), &excerpt) else {
            continue;
        };
        let verdict =
            if j.contradiction() >= CANON_CONTRADICTION && similarity >= suspect_similarity {
                Some(("contradicts", j.contradiction()))
            } else if j.forward.entailment >= CANON_SUPPORT {
                Some(("supports", j.forward.entailment))
            } else {
                None
            };
        if let Some((verdict, score)) = verdict {
            out.push(CanonVerdict {
                id: node.id,
                node_type: node.node_type,
                title: node.title,
                verdict: verdict.into(),
                score: score as f64,
                similarity,
            });
        }
    }
    // Contradictions first — they are the act-now verdicts.
    out.sort_by(|a, b| {
        (b.verdict == "contradicts")
            .cmp(&(a.verdict == "contradicts"))
            .then(b.score.total_cmp(&a.score))
    });
    out
}

/// [`Engine::nli_hint`] without the engine.
fn claim_hint(nli: &dyn Nli, a: &Node, b: &Node) -> Option<Hint> {
    let sym = nli.judge_pair(&claim(a), &claim(b)).ok()?;
    Some(hint_with_direction(&sym, a, b))
}

/// [`Engine::nli_title_hint`] without the engine.
fn title_hint(nli: &dyn Nli, a: &Node, b: &Node) -> Option<Hint> {
    let sym = nli.judge_pair(a.title.trim(), b.title.trim()).ok()?;
    Some(hint_with_direction(&sym, a, b))
}

/// [`Engine::nli_title_hint_clause`] without the engine.
fn title_hint_clause(nli: &dyn Nli, a: &Node, b: &Node) -> Option<Hint> {
    let (ta, tb) = (a.title.trim(), b.title.trim());
    let mut sym = nli.judge_pair(ta, tb).ok()?;
    let (ca, cb) = (title_clause(ta), title_clause(tb));
    if (ca, cb) != (ta, tb)
        && let Ok(clause) = nli.judge_pair(ca, cb)
        && clause.contradiction() > sym.contradiction()
    {
        sym = clause;
    }
    Some(hint_with_direction(&sym, a, b))
}

/// [`Engine::nli_title_hint_for`] without the engine.
fn title_hint_for(nli: &dyn Nli, path: &str, a: &Node, b: &Node) -> Option<Hint> {
    if path == "unnamed" {
        title_hint_clause(nli, a, b)
    } else {
        title_hint(nli, a, b)
    }
}

/// The journal action of an undo's marker row (entity `audit`, entity_id =
/// the undone row's seq).
const UNDONE: &str = "undone";
/// The journal action of a note or link put back from a snapshot.
const RESTORED: &str = "restored";

/// Can this journal row be undone? Node and edge mutations with a snapshot;
/// never session activity, imports or undo markers.
fn undoable(r: &AuditEntry) -> bool {
    matches!(r.entity.as_str(), "node" | "edge")
        && r.action != "imported"
        && (r.before.is_some() || r.after.is_some())
}

/// What undoing a row means, in the pane's words.
fn undo_effect(r: &AuditEntry) -> &'static str {
    match (r.entity.as_str(), r.before.is_some(), r.after.is_some()) {
        ("node", false, true) => "removes the note",
        ("node", true, false) => "brings the note back",
        ("node", true, true) => "restores the note as it was",
        ("edge", false, true) => "removes the link",
        ("edge", true, false) => "brings the link back",
        ("edge", true, true) => "restores the link as it was",
        _ => "nothing",
    }
}

/// Clears the engine's current operation id when the operation that set it
/// ends (see [`Engine::op_scope`]).
struct OpScope<'a> {
    slot: Option<&'a std::sync::Mutex<Option<String>>>,
}

impl Drop for OpScope<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot
            && let Ok(mut op) = slot.lock()
        {
            *op = None;
        }
    }
}

/// Lock a shared engine; a poisoned lock (a panic mid-write elsewhere) is an
/// error for the caller, not a second panic.
fn lock_engine(engine: &std::sync::Mutex<Engine>) -> Result<std::sync::MutexGuard<'_, Engine>> {
    engine
        .lock()
        .map_err(|_| crate::Error::Io("engine lock poisoned".into()))
}

/// `canon_order: connected` (0.9.9): pinned first, then trust × (1 +
/// ln(1 + live edges)) — the canon other knowledge leans on leads — with the
/// endorsed order (the input's) breaking ties.
fn sort_connected(nodes: &mut [Node], degree: &std::collections::HashMap<String, usize>) {
    let weight = |n: &Node| {
        let d = degree.get(&n.id).copied().unwrap_or(0) as f64;
        n.trust * (1.0 + (1.0 + d).ln())
    };
    nodes.sort_by(|a, b| {
        b.trust_override
            .is_some()
            .cmp(&a.trust_override.is_some())
            .then(weight(b).total_cmp(&weight(a)))
    });
}

/// Whether a node's type carries the `anchor` role under this graph's
/// ontology (code-subject labels: similar by nature, not by contradiction).
fn is_anchor(cfg: &crate::config::GraphConfig, n: &Node) -> bool {
    cfg.type_def(n.node_type.as_str())
        .is_some_and(|t| t.roles.anchor)
}

/// Whether a node's type carries the `tombstone` role under this graph's
/// ontology (a record of deliberately removed knowledge — findable, never
/// canon, never to be re-derived).
/// The endorsement ladder a twin sits on, highest first (twin order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rung {
    Pinned,
    Approved,
    Confirmed,
    None,
}

impl Rung {
    fn of(n: &Node) -> Self {
        if n.trust_override.is_some() {
            Rung::Pinned
        } else if n.approved_at.is_some() {
            Rung::Approved
        } else if n.confirmed_at.is_some() {
            Rung::Confirmed
        } else {
            Rung::None
        }
    }
}

/// NLI pairs one Checkup sweep run may judge. An audit that holds the
/// engine for a minute is worse than one that says "run me again" — and
/// since 0.9.11 running it again continues where the last run stopped.
const AUDIT_NLI_PAIR_BUDGET: usize = 300;

/// Remembered pairs per sweep kind; the oldest fall out past this (a pair
/// read again costs one NLI call, never a wrong answer).
const SWEEP_MEMORY_MAX: usize = 20_000;

/// The pairs a Checkup sweep judged without queueing (0.9.11, Problem
/// 00dxmtfmipga): before it, every run re-read the same first 300 pairs and
/// "run again to continue" never continued. Kept in the store's kv meta per
/// sweep kind, so it travels with `migrate` like the rest of meta. A pair is
/// remembered by [`pair_memo`] — both notes' ids and content plus the read
/// that judged it — so editing either note re-opens it, and the whole
/// memory is dropped when its stamp (judge model, gates) changes: a verdict
/// is only as reusable as the judge and the bar that produced it.
struct SweepMemory {
    key: String,
    stamp: String,
    order: Vec<u64>,
    set: std::collections::HashSet<u64>,
    dirty: bool,
}

impl SweepMemory {
    fn load(store: &dyn Store, target: &str, stamp: &str) -> Result<Self> {
        let key = format!("audit_sweep.passed.{target}");
        let mut memory = Self {
            key,
            stamp: stamp.to_string(),
            order: Vec::new(),
            set: Default::default(),
            dirty: false,
        };
        if let Some(raw) = store.kv_get(&memory.key)?
            && let Some((head, pairs)) = raw.split_once('\n')
            && head == memory.stamp
        {
            for memo in pairs
                .split(',')
                .filter_map(|h| u64::from_str_radix(h, 16).ok())
            {
                if memory.set.insert(memo) {
                    memory.order.push(memo);
                }
            }
        } else {
            // No memory yet, or one a different judge or bar wrote: start
            // over, and say so on the next save even if nothing is added.
            memory.dirty = true;
        }
        Ok(memory)
    }

    fn contains(&self, memo: u64) -> bool {
        self.set.contains(&memo)
    }

    fn insert(&mut self, memo: u64) {
        if self.set.insert(memo) {
            self.order.push(memo);
            self.dirty = true;
        }
    }

    fn save(&mut self, store: &dyn Store) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.order.len() > SWEEP_MEMORY_MAX {
            let drop = self.order.len() - SWEEP_MEMORY_MAX;
            for memo in self.order.drain(..drop) {
                self.set.remove(&memo);
            }
        }
        let pairs: Vec<String> = self.order.iter().map(|m| format!("{m:x}")).collect();
        store.kv_set(&self.key, &format!("{}\n{}", self.stamp, pairs.join(",")))?;
        self.dirty = false;
        Ok(())
    }
}

/// One unordered pair as a sweep read it: both ids with their content, in id
/// order, and the read (`"claim"`, or the title guard's path) — a pair that
/// crosses the similarity floor is read differently and so judged anew.
fn pair_memo(a: &Node, b: &Node, read: &str) -> u64 {
    let (a, b) = if a.id <= b.id { (a, b) } else { (b, a) };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &x in bytes.iter().chain(&[0u8]) {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for n in [a, b] {
        eat(n.id.as_bytes());
        eat(&content_hash(&n.title, n.body.as_deref()).to_le_bytes());
    }
    eat(read.as_bytes());
    h
}

/// FNV-1a over a note's title and body: a sampling order that depends only
/// on content — stable across runs, platforms and Rust versions (std's
/// hasher promises none of that).
fn content_hash(title: &str, body: Option<&str>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let bytes = title.bytes().chain([0u8]).chain(body.unwrap_or("").bytes());
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn is_tombstone(cfg: &crate::config::GraphConfig, n: &Node) -> bool {
    cfg.type_def(n.node_type.as_str())
        .is_some_and(|t| t.roles.tombstone)
}

/// The victim's title as a delete mint wrote it into the marker's title
/// ("Removed: <title>"); a marker written by hand comes back whole.
fn tombstone_victim_title(title: &str) -> &str {
    title
        .trim()
        .strip_prefix("Removed:")
        .map_or(title.trim(), str::trim)
}

/// Titles compared as the same claim: case-folded alphanumeric words.
fn normalized_title(t: &str) -> String {
    t.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The tombstone's account of the removal, for riding on a warning: the
/// body's opening (which the delete mint fills with the victim's identity
/// and the "**Why:**" line), word-cut so a carried-over victim body doesn't
/// turn the verdict into a transcript.
fn tombstone_note(n: &Node) -> Option<String> {
    const TOMBSTONE_NOTE_CHARS: usize = 280;
    let body = n.body.as_deref()?.trim();
    if body.is_empty() {
        return None;
    }
    Some(excerpt_words(
        &body.replace('\n', " "),
        TOMBSTONE_NOTE_CHARS,
    ))
}

/// Cut text at the last word boundary within `max` chars, appending `…` when
/// anything was dropped.
fn excerpt_words(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let trimmed = match cut.rfind(char::is_whitespace) {
        Some(i) if i > max / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", trimmed.trim_end())
}

/// The temporal arguments of one search, resolved (0.8.7). Default is the
/// whole timeline in relevance order — which is exactly what every caller
/// before this existed asked for, so [`Engine::search`] stays a thin wrapper
/// and no existing behavior moves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilter {
    pub window: crate::timespec::TimeWindow,
    pub order: crate::timespec::SearchOrder,
    /// Which clock the window reads (0.9.0): `created_at` by default, or a
    /// date-kind custom field / `from..to` field span — bitemporal search
    /// for graphs that declare an event clock (historic imports).
    pub clock: crate::timespec::TimeClock,
}

impl SearchFilter {
    /// Does this filter change anything? Used to keep the unfiltered path
    /// identical rather than merely equivalent. A non-default clock always
    /// travels with a window (validated at search time), so the window test
    /// covers it.
    pub fn is_default(&self) -> bool {
        self.window.is_open() && !self.order.is_temporal()
    }
}

/// Re-order a delivered result set. Ties break by id, which is time-sortable,
/// so two notes captured in the same second still come out in a stable and
/// truthful order rather than whatever the hash iteration produced.
fn order_hits(hits: &mut [SearchHit], order: crate::timespec::SearchOrder) {
    use crate::timespec::SearchOrder;
    match order {
        SearchOrder::Relevance => {}
        SearchOrder::Chronological => {
            hits.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        }
        SearchOrder::Recent => {
            hits.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        }
    }
}

/// Cosine similarity, computed with magnitudes rather than assuming unit
/// vectors — the embedder is swappable (models.json), and a normalization
/// assumption that holds for today's default would fail silently on the next.
fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Re-order history hits — the episodic twin of [`order_hits`]. Ties break by
/// message id (time-sortable), so messages sharing a second keep the order
/// they were said in.
fn order_history_hits(
    hits: &mut [crate::history::HistoryHit],
    order: crate::timespec::SearchOrder,
) {
    use crate::timespec::SearchOrder;
    match order {
        SearchOrder::Relevance => {}
        SearchOrder::Chronological => {
            hits.sort_by(|a, b| (a.timestamp, &a.message_id).cmp(&(b.timestamp, &b.message_id)));
        }
        SearchOrder::Recent => {
            hits.sort_by(|a, b| (b.timestamp, &b.message_id).cmp(&(a.timestamp, &a.message_id)));
        }
    }
}

/// Session-diverse delivery cut (0.8.10): pick `limit` hits from a ranked
/// candidate list, demoting each additional hit from an already-picked
/// session by `demote` RANK positions — a greedy MMR keyed on rank rather
/// than score, because rank is the one scale that transfers between graphs
/// (the 0.8.1 register lesson). With `demote` 0 the pick is byte-identical
/// to `truncate(limit)`; hits without a session are never demoted and never
/// count against one. Selection and order only — scores are untouched, so
/// the floor, the knee and the verdict all still read the cross-encoder's
/// calibrated scale.
pub(crate) fn session_diverse_select<T>(
    hits: &mut Vec<T>,
    limit: usize,
    demote: f64,
    session_of: impl Fn(&T) -> Option<&str>,
) {
    if hits.len() <= limit || demote <= 0.0 {
        hits.truncate(limit);
        return;
    }
    let n = hits.len();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut used = vec![false; n];
    let mut picked: Vec<usize> = Vec::with_capacity(limit);
    while picked.len() < limit {
        let mut best: Option<(f64, usize)> = None;
        for (i, taken) in used.iter().enumerate() {
            if *taken {
                continue;
            }
            let repeats = session_of(&hits[i])
                .and_then(|s| counts.get(s).copied())
                .unwrap_or(0);
            let effective = i as f64 + demote * repeats as f64;
            // Strict `<` keeps ties on the earlier (better-ranked) index.
            if best.is_none_or(|(b, _)| effective < b) {
                best = Some((effective, i));
            }
        }
        let Some((_, i)) = best else { break };
        used[i] = true;
        if let Some(s) = session_of(&hits[i]) {
            *counts.entry(s.to_string()).or_default() += 1;
        }
        picked.push(i);
    }
    let mut keep = std::mem::take(hits);
    let mut by_index: Vec<Option<T>> = keep.drain(..).map(Some).collect();
    *hits = picked
        .into_iter()
        .filter_map(|i| by_index[i].take())
        .collect();
}

/// The knee of a DESC-sorted score curve: the largest relative drop, when it
/// is at least `min_cliff` of the running score (Tail-Aware Adaptive-k,
/// arXiv:2606.11907, simplified to the knee without the EVT validation
/// pass — see [`crate::policy::KNEE_MIN_CLIFF`]). Returns the minimum score
/// to KEEP, or `None` when the curve has no cliff worth acting on.
fn knee_floor(sorted_desc: &[f64], min_cliff: f64) -> Option<f64> {
    if sorted_desc.len() <= 1 {
        return None;
    }
    let (mut best_at, mut best_drop) = (0usize, 0.0f64);
    for i in 1..sorted_desc.len() {
        let prev = sorted_desc[i - 1].max(1e-9);
        let drop = (sorted_desc[i - 1] - sorted_desc[i]) / prev;
        if drop > best_drop {
            best_drop = drop;
            best_at = i;
        }
    }
    (best_drop >= min_cliff).then(|| sorted_desc[best_at - 1])
}

/// Higher quantile of an ASC-sorted sample (the split-conformal convention:
/// round up, never interpolate — the threshold must be a score that was
/// actually reached).
fn quantile(sorted_asc: &[f64], q: f64) -> f64 {
    if sorted_asc.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_asc.len() - 1) as f64 * q).ceil() as usize;
    sorted_asc[idx.min(sorted_asc.len() - 1)]
}

/// Up to two long content words from a note title — the register the
/// weak-line probes borrow so they ask the way this graph is written.
fn probe_terms(title: &str) -> Option<String> {
    let words: Vec<&str> = title
        .split(|c: char| !c.is_alphanumeric() && !"-_.".contains(c))
        .filter(|w| w.len() >= 5)
        .take(2)
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// The i-th deterministic coinage: pronounceable, unmistakably not a word
/// this graph contains. Deterministic — the same graph calibrates against
/// the same probes, so the fitted line settles instead of wandering.
fn coined(i: usize) -> String {
    const ONSET: [&str; 8] = ["vor", "zel", "quam", "dro", "fex", "gril", "plom", "stru"];
    const MID: [&str; 8] = ["ni", "ba", "ro", "ka", "lu", "tri", "gos", "pem"];
    const CODA: [&str; 8] = ["dax", "vek", "morn", "lisk", "tor", "bran", "funt", "gel"];
    format!("{}{}{}", ONSET[i % 8], MID[(i / 8) % 8], CODA[(i / 64) % 8])
}

/// Mint the i-th template phantom probe for the weak-line fit: a
/// memory-shaped question about a coined subject that cannot exist in any
/// graph, phrased over vocabulary borrowed from the graph itself when it
/// has any.
fn phantom_probe(i: usize, terms: Option<&str>) -> String {
    const FALLBACK: [&str; 6] = [
        "subsystem",
        "migration",
        "deployment",
        "cache layer",
        "retry worker",
        "pipeline",
    ];
    const TEMPLATES: [&str; 6] = [
        "What did we decide about the {}?",
        "Why does the {} keep failing?",
        "What broke the last time the {} was deployed?",
        "Which constraint governs the {}?",
        "What is the policy for the {}?",
        "What did the {} replace and why?",
    ];
    let topic = format!(
        "{} {}",
        coined(i),
        terms.unwrap_or(FALLBACK[i % FALLBACK.len()])
    );
    TEMPLATES[i % TEMPLATES.len()].replace("{}", &topic)
}

/// Mint the i-th transplant probe: a REAL sentence from a real note with
/// its two most distinctive words swapped for coinages (ICT inverted — ACL
/// 2019 P19-1612, repurposed for calibration). Template probes are
/// lexically in register but not syntactically; a transplant is both — it
/// reads exactly like the oblique prose queries that score highest on
/// never-written subjects, which is the ceiling the line has to reach
/// (measured gap at 1500 notes: template q90 0.476 vs real-control q90
/// 0.768). The swap keeps it unanswerable; everything else is the graph's
/// own voice.
fn transplant_probe(i: usize, title: &str, body: Option<&str>) -> String {
    // The longest sentence in the body carries the most register; a short
    // or empty body falls back to the title.
    let sentence = body
        .unwrap_or("")
        .split(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|s| s.len() >= 40)
        .max_by_key(|s| s.len().min(220))
        .unwrap_or(title);
    let sentence: String = sentence.chars().take(220).collect();

    // Swap the two longest content words — the subject-bearing tokens in
    // almost any technical sentence — for coinages.
    let mut words: Vec<&str> = sentence
        .split(|c: char| !c.is_alphanumeric() && !"-_".contains(c))
        .filter(|w| w.len() >= 6)
        .collect();
    words.sort_by_key(|w| std::cmp::Reverse(w.len()));
    words.dedup();
    let mut out = sentence.clone();
    for (n, w) in words.into_iter().take(2).enumerate() {
        out = out.replacen(w, &coined(i + 37 * (n + 1)), 1);
    }
    out
}
