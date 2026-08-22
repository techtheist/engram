//! The integration point the HTTP and MCP layers talk to: a `Store` plus an
//! `Embedder`. It keeps embeddings in lockstep with node writes and exposes
//! the retrieval surface (hybrid search) so callers never touch vectors
//! directly.

use crate::nli::Nli;
use crate::rag::{Embedder, Reranker};
use crate::types::*;
use crate::{Result, Store};

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
    embedder: Box<dyn Embedder>,
    /// The precision layer (PLAN §7A): optional cross-encoder re-scoring of
    /// search candidates. Absent in tests, under `--fake-embeddings`, and
    /// when the model can't load — search then keeps plain hybrid order.
    reranker: Option<Box<dyn Reranker>>,
    /// The logic layer (PLAN §7A): optional local NLI. Nominations only —
    /// suspect hints, claim checks, audit sweeps; never touches trust.
    nli: Option<Box<dyn Nli>>,
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
    /// The at-rest sealing key, loaded once on first history write/read —
    /// but only when the daemon opted in (tests and library embedders must
    /// never touch the OS keychain).
    sealing_wanted: bool,
    history_key: std::sync::OnceLock<Option<crate::history::HistoryKey>>,
    listeners: Vec<Listener>,
    audit_origin: AuditOrigin,
    /// Binary-side context captured once per process — the enrichment every
    /// audit row carries (PLAN §10 audit journal).
    audit_cwd: Option<String>,
    audit_pid: i64,
    audit_version: String,
    /// When [`Engine::validate_graph`] last ran — the session-boundary
    /// trigger consults this so back-to-back connects don't re-sweep. Per
    /// graph, not per process: this engine IS the graph's process-side.
    last_validated: std::sync::atomic::AtomicI64,
}

impl Engine {
    pub fn new(store: impl Store + 'static, embedder: Box<dyn Embedder>) -> Self {
        Self::with_store(Box::new(store), embedder)
    }

    /// Backend-agnostic form for callers that went through [`crate::open_store`].
    pub fn with_store(store: Box<dyn Store>, embedder: Box<dyn Embedder>) -> Self {
        Self {
            store,
            embedder,
            reranker: None,
            nli: None,
            repo_root: None,
            history_path: None,
            history: std::sync::Mutex::new(None),
            provenance_lot: std::sync::Mutex::new(Vec::new()),
            sealing_wanted: false,
            history_key: std::sync::OnceLock::new(),
            listeners: Vec::new(),
            audit_origin: AuditOrigin::default(),
            audit_cwd: std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string()),
            audit_pid: std::process::id() as i64,
            audit_version: env!("CARGO_PKG_VERSION").to_string(),
            last_validated: std::sync::atomic::AtomicI64::new(0),
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
        self.nli = Some(nli);
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
        self.embedder = embedder;
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

    /// The machine's history sealing key, loaded once per process (keyring,
    /// then file fallback). `None` = sealing unavailable — writes stay
    /// plaintext rather than failing, and the backlog pass seals them the
    /// moment a key exists.
    /// Opt this engine into at-rest history sealing (serve does; tests and
    /// library embedders don't, so they never touch the OS keychain).
    pub fn enable_history_sealing(&mut self) {
        self.sealing_wanted = true;
    }

    fn history_key(&self) -> Option<&crate::history::HistoryKey> {
        if !self.sealing_wanted {
            return None;
        }
        self.history_key
            .get_or_init(crate::history::HistoryKey::load_or_create)
            .as_ref()
    }

    /// Decrypt one stored history string for a reader. Unsealed strings pass
    /// through (pre-seal rows, structural fields); an undecryptable blob
    /// renders as a placeholder, never as garbage.
    fn unseal_str(&self, s: &str) -> String {
        if !crate::history::is_sealed(s) {
            return s.to_string();
        }
        self.history_key()
            .and_then(|k| k.unseal(s))
            .unwrap_or_else(|| "[sealed — history key unavailable]".to_string())
    }

    /// Write one node into the history layer and embed it there — the
    /// harvester's write path. Deliberately NOT [`Engine::add_node`]: no
    /// audit row, no change event, no dupe verdicts, no version stamp —
    /// history is records, not knowledge.
    ///
    /// Order is load-bearing: scrub the plaintext (the store's own scrub
    /// can't see through a seal), embed the SCRUBBED PLAINTEXT (vectors stay
    /// deliberately open — documented inversion risk), then seal title+body
    /// and store. `Ok(None)` when the layer is disabled or closed.
    pub fn add_history_node(&self, mut n: NewNode) -> Result<Option<Node>> {
        n.title = crate::redact::scrub(&n.title);
        n.body = n.body.as_deref().map(crate::redact::scrub);
        let plain_title = n.title.clone();
        let plain_body = n.body.clone();
        if let Some(key) = self.history_key() {
            n.title = key.seal(&n.title);
            n.body = n.body.as_deref().map(|b| key.seal(b));
        }
        let Some(node) = self.with_history(|s| s.add_node(n)).transpose()? else {
            return Ok(None);
        };
        let mut texts = vec![embed_text(
            &plain_title,
            plain_body.as_deref(),
            &node.tags,
            &node.code_refs,
        )];
        texts.extend(claim_texts(&plain_title, plain_body.as_deref()));
        let vectors = self.embedder.embed(&texts)?;
        self.with_history(|s| s.upsert_embeddings(&node.id, &vectors))
            .transpose()?;
        Ok(Some(node))
    }

    /// Seal any plaintext rows a pre-seal daemon (or a keyless period) left
    /// behind — the re-seal pass the "encryption last" build order requires.
    /// Vectors are untouched: they were computed from the same plaintext and
    /// stay open by design. Returns how many nodes were sealed.
    pub fn seal_history_backlog(&self) -> Result<usize> {
        let Some(_) = self.history_key() else {
            return Ok(0);
        };
        let nodes = self
            .with_history(|s| s.all_nodes())
            .transpose()?
            .unwrap_or_default();
        let mut sealed = 0usize;
        for mut node in nodes {
            let title_open = !crate::history::is_sealed(&node.title);
            let body_open = node
                .body
                .as_deref()
                .is_some_and(|b| !crate::history::is_sealed(b));
            if !title_open && !body_open {
                continue;
            }
            let key = self.history_key().expect("checked above");
            if title_open {
                node.title = key.seal(&node.title);
            }
            if body_open && let Some(b) = &node.body {
                node.body = Some(key.seal(b));
            }
            self.with_history(|s| s.upsert_node(&node)).transpose()?;
            sealed += 1;
        }
        Ok(sealed)
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
            if node.source != crate::types::Source::Claude
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
                source: crate::types::Source::Claude,
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

    /// Search the history layer: vector-first (no FTS exists over history —
    /// caution 00bgftfbusll), then the cross-encoder re-scores the candidate
    /// texts at query time. Scores live on the reranker's scale but are
    /// NEVER blended with curated scores (the 0.8.1 register lesson) — the
    /// caller renders these as their own labeled section. Empty when the
    /// layer is off or `search_fallthrough` gates it at the call site.
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
                    let p = |k: &str| n.props.as_ref().and_then(|m| m.get(k).cloned());
                    let session = n.session_id.clone().unwrap_or_default();
                    out.push(crate::history::HistoryHit {
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
                        score: (1.0 - dist).clamp(0.0, 1.0),
                        prior: Vec::new(),
                    });
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
        hits.truncate(limit);
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
            .with_history(|s| -> Result<Vec<crate::types::Node>> {
                Ok(s.all_nodes()?
                    .into_iter()
                    .filter(|n| {
                        n.node_type.as_str() == crate::history::MESSAGE_TYPE
                            && n.session_id.as_deref() == Some(session)
                    })
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
            .with_history(|s| {
                s.nodes_by_type_active(
                    &crate::types::NodeType::parse(crate::history::SESSION_TYPE)?,
                    usize::MAX,
                )
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
                let nodes: Vec<Node> = s
                    .all_nodes()?
                    .into_iter()
                    .filter(|n| n.session_id.as_deref() == Some(session))
                    .collect();
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
                for n in s.all_nodes()? {
                    if n.node_type.as_str() != crate::history::MESSAGE_TYPE
                        || n.session_id.as_deref() != Some(session)
                    {
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
    /// title upgrades live in `Session.props`). Fields arrive plaintext (a
    /// retitle) or still sealed (a props-only flush) — plaintext gets
    /// sealed, sealed passes through. Re-embeds only when asked, always
    /// from plaintext. Returns whether the layer was open to take the write.
    pub fn upsert_history_node(&self, node: &Node, re_embed: bool) -> Result<bool> {
        let mut stored = node.clone();
        let plain_title = self.unseal_str(&stored.title);
        let plain_body = stored.body.as_deref().map(|b| self.unseal_str(b));
        if let Some(key) = self.history_key() {
            if !crate::history::is_sealed(&stored.title) {
                stored.title = key.seal(&stored.title);
            }
            if let Some(b) = &stored.body
                && !crate::history::is_sealed(b)
            {
                stored.body = Some(key.seal(b));
            }
        }
        let wrote = self
            .with_history(|s| {
                s.upsert_node(&stored)?;
                Ok::<_, crate::Error>(())
            })
            .transpose()?;
        if wrote.is_some() && re_embed {
            let mut texts = vec![embed_text(
                &plain_title,
                plain_body.as_deref(),
                &stored.tags,
                &stored.code_refs,
            )];
            texts.extend(claim_texts(&plain_title, plain_body.as_deref()));
            let vectors = self.embedder.embed(&texts)?;
            self.with_history(|s| s.upsert_embeddings(&stored.id, &vectors))
                .transpose()?;
        }
        Ok(wrote.is_some())
    }

    /// Reconcile the open handle with `config().history.enabled` — runs on
    /// path install and after every config write, so the pane's toggle takes
    /// effect without a daemon restart. Disabling drops the handle; the file
    /// stays on disk (delete is a user gesture).
    fn sync_history_store(&self) {
        let Ok(mut slot) = self.history.lock() else {
            return;
        };
        let enabled = self.store.config().history.enabled;
        if enabled && slot.is_none() {
            if let Some(path) = &self.history_path {
                match crate::history::open_history_store(path) {
                    Ok(s) => *slot = Some(s),
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
        Ok(previous)
    }

    /// One full graph-health pass — the session-boundary validation: the
    /// decay pass archives what has expired, the conflict scan queues fresh
    /// look-alike pairs, and the drift scan counts unresolved code_refs, so
    /// a session starts (and leaves) with the graph prepared rather than
    /// waiting for the six-hourly sweep. Journaled as a `graph_validated`
    /// activity row; returns the summary note.
    pub fn validate_graph(&self) -> Result<String> {
        self.last_validated
            .store(crate::store::now(), std::sync::atomic::Ordering::Relaxed);
        let ttl = self.store.config().policy.decay_ttl_days;
        let archived = self.decay(ttl, false)?.len();
        let retired = self.retire_superseded_sweep()?;
        // Calibrate before scanning, so this session's conflict pass already
        // runs under the floor the graph's own judgments support.
        let tuned = self.auto_tune()?;
        let suspects = self.scan_conflicts()?;
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
        notes.sort_by(|a, b| a.id.cmp(&b.id));
        let vocab: Vec<String> = notes.iter().filter_map(|n| probe_terms(&n.title)).collect();
        // Two probe families, each answering "how high can a score climb
        // here when the answer does not exist" from a different angle:
        // question-shaped templates over borrowed vocabulary, and ICT
        // transplants — real sentences with their subjects coined out. The
        // fit is the max of the per-family quantiles: the line must clear
        // whichever register this graph's noise speaks loudest in.
        let total = cfg.policy.weak_line_probes;
        let top_of = |probe: &str| -> Result<f64> {
            let qv = self.embedder.embed_one(probe)?;
            // Probes calibrate against the WHOLE graph — a windowed noise
            // sample would fit a line to a slice of history.
            let mut hits =
                self.store
                    .search_hybrid(probe, Some(&qv), &[], 12, Default::default())?;
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
                template_tops.push(top_of(&phantom_probe(i / 2, terms))?);
            } else {
                let n = &notes[(i / 2) * 17 % notes.len()];
                transplant_tops.push(top_of(&transplant_probe(
                    i / 2,
                    &n.title,
                    n.body.as_deref(),
                ))?);
            }
        }
        template_tops.sort_by(f64::total_cmp);
        transplant_tops.sort_by(f64::total_cmp);
        let q = cfg.policy.weak_line_quantile;
        let fitted = quantile(&template_tops, q).max(quantile(&transplant_tops, q));
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
    pub fn audit_log(
        &self,
        before: Option<i64>,
        entity_id: Option<&str>,
        limit: usize,
    ) -> Result<AuditPage> {
        self.store.audit_page(before, entity_id, limit)
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
    pub fn add_node(&self, mut n: NewNode) -> Result<Node> {
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
        let node = self.store.add_node(n)?;
        self.embed_node(&node)?;
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
        if let Some(t) = &patch.node_type {
            self.check_node_type(t)?;
        }
        let touches_text = patch.title.is_some()
            || patch.body.is_some()
            || patch.tags.is_some()
            || patch.code_refs.is_some();
        let before = self.store.get_node(id)?;
        let node = self.store.update_node(id, patch)?;
        if touches_text {
            self.embed_node(&node)?;
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
        let before = self.store.get_node(id)?;
        let removed = self.store.delete_node(id)?;
        if removed {
            self.audit_node("deleted", before.as_ref(), None)?;
            self.notify(ChangeEvent::NodeDeleted(id.to_string()));
        }
        Ok(removed)
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
        self.retire_superseded(&edge)?;
        Ok(edge)
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
        Ok(())
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
        let qv = self.embedder.embed_one(query)?;
        let fetch = match &self.reranker {
            Some(_) => (limit * 3).clamp(12, 50),
            None => limit,
        };
        let mut hits = self
            .store
            .search_hybrid(query, Some(&qv), types, fetch, filter.window)?;
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
        hits.truncate(limit);
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
        for hit in &mut hits {
            hit.neighbors = self.store.neighbors(&hit.id, NEIGHBOR_CAP)?;
        }
        order_hits(&mut hits, filter.order);
        // Observability stamp on what was actually returned — never the
        // over-fetched candidates the reranker discarded. (Trust doesn't
        // read this either way; see policy.)
        let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        self.store.touch(&ids)?;
        Ok(hits)
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
        Ok(SearchFilter { window, order })
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
    pub fn add_node_checked(&self, n: NewNode) -> Result<WriteOutcome> {
        let scrubbed_title = crate::redact::scrub(&n.title);
        let scrubbed_body = n.body.as_deref().map(crate::redact::scrub);
        let vec = self.embedder.embed_one(&embed_text(
            &scrubbed_title,
            scrubbed_body.as_deref(),
            &n.tags,
            &n.code_refs,
        ))?;

        let duplicate_similarity = self.store.config().policy.duplicate_similarity;
        for (id, distance) in self.store.search_vec(&vec, WRITE_CHECK_K)? {
            let similarity = 1.0 - distance;
            if similarity < duplicate_similarity {
                break; // results are distance-ordered; nothing closer follows
            }
            if let Some(node) = self.store.get_node(&id)?
                && node.node_type == n.node_type
                && node.valid_until.is_none()
            {
                // At duplicate similarity co-reference holds, so an NLI
                // contradiction is trustworthy — it flags the negated
                // near-duplicate a cosine score can't see.
                let (nli_label, nli_score) = match &self.nli {
                    Some(nli) => {
                        let text = match &scrubbed_body {
                            Some(b) => format!("{scrubbed_title}. {b}"),
                            None => scrubbed_title.clone(),
                        };
                        let excerpt: String = text.chars().take(400).collect();
                        match nli.judge_pair(&excerpt, &claim(&node)) {
                            Ok(sym) => {
                                let (l, s) = sym.hint();
                                (Some(l.to_string()), Some(s as f64))
                            }
                            Err(_) => (None, None),
                        }
                    }
                    None => (None, None),
                };
                return Ok(WriteOutcome::Matched {
                    node,
                    similarity,
                    nli_label,
                    nli_score,
                });
            }
        }

        let missing_refs = self.missing_refs(&n.code_refs);
        let node = self.add_node(n)?;
        let warnings = self.write_warnings(&vec, &node.id)?;
        let suspects = if self.record_suspects(&vec, &node.id)? > 0 {
            self.suspects_involving(&node.id)?
        } else {
            Vec::new()
        };
        let canon = self.canon_verdicts(&vec, &claim(&node), &node.id)?;
        Ok(WriteOutcome::Created {
            node,
            warnings,
            suspects,
            missing_refs,
            canon,
        })
    }

    /// The write-time canon check (PLAN §7A): judge the fresh text against
    /// its nearest existing knowledge. Entailment is directional and cheap
    /// to trust — `supports` says the canon already backs this claim (link
    /// it, or wonder why it needed rewriting). `contradicts` is only issued
    /// inside the suspect similarity band, where the co-reference
    /// presupposition holds — below it an MNLI verdict is noise. Capped, and
    /// skipped entirely without the logic layer.
    fn canon_verdicts(
        &self,
        vec: &[f32],
        text: &str,
        exclude_id: &str,
    ) -> Result<Vec<CanonVerdict>> {
        const CANON_CHECK_CAP: usize = 5;
        const CANON_SUPPORT: f32 = 0.6;
        const CANON_CONTRADICTION: f32 = 0.7;
        let Some(nli) = &self.nli else {
            return Ok(Vec::new());
        };
        let cfg = self.store.config();
        let excerpt: String = text.chars().take(400).collect();
        let mut out = Vec::new();
        let mut examined = 0;
        for (id, distance) in self.store.search_vec(vec, WRITE_CHECK_K)? {
            if id == exclude_id {
                continue;
            }
            let similarity = 1.0 - distance;
            if similarity < cfg.policy.warn_similarity {
                break; // distance-ordered: nothing closer follows
            }
            if examined >= CANON_CHECK_CAP {
                break;
            }
            let Some(node) = self.store.get_node(&id)? else {
                continue;
            };
            if node.valid_until.is_some() || is_anchor(&cfg, &node) {
                continue;
            }
            examined += 1;
            let Ok(j) = nli.judge_pair(&claim(&node), &excerpt) else {
                continue;
            };
            let verdict = if j.contradiction() >= CANON_CONTRADICTION
                && similarity >= cfg.policy.conflict_suspect_similarity
            {
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
        Ok(out)
    }

    /// `update_node` plus conflict warnings and freshly-queued suspects when
    /// any embedded field changed.
    pub fn update_node_checked(&self, id: &str, patch: NodePatch) -> Result<CheckedUpdate> {
        let touches_text = patch.title.is_some()
            || patch.body.is_some()
            || patch.tags.is_some()
            || patch.code_refs.is_some();
        let node = self.update_node(id, patch)?;
        let missing_refs = self.missing_refs(&node.code_refs);
        let (warnings, suspects, canon) = if touches_text {
            let vec = self.embedder.embed_one(&embed_text(
                &node.title,
                node.body.as_deref(),
                &node.tags,
                &node.code_refs,
            ))?;
            let suspects = if self.record_suspects(&vec, &node.id)? > 0 {
                self.suspects_involving(&node.id)?
            } else {
                Vec::new()
            };
            let canon = self.canon_verdicts(&vec, &claim(&node), &node.id)?;

            (self.write_warnings(&vec, &node.id)?, suspects, canon)
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        Ok(CheckedUpdate {
            node,
            warnings,
            suspects,
            missing_refs,
            canon,
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
            if source == Source::Claude && node.trust_override.is_some() {
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

    /// Nearby nodes that are contradicted (active `conflicts-with`) or
    /// superseded — returned with writes so the writing assistant notices it
    /// may be re-treading contested or stale ground (PLAN §7, pull-based).
    fn write_warnings(&self, vec: &[f32], exclude_id: &str) -> Result<Vec<WriteWarning>> {
        let mut warnings = Vec::new();
        let warn_similarity = self.store.config().policy.warn_similarity;
        for (id, distance) in self.store.search_vec(vec, WRITE_CHECK_K)? {
            if id == exclude_id {
                continue;
            }
            let similarity = 1.0 - distance;
            if similarity < warn_similarity {
                break;
            }
            let Some(node) = self.store.get_node(&id)? else {
                continue;
            };
            let reason = if node.valid_until.is_some() {
                "superseded"
            } else if self.store.has_active_conflict(&id)? {
                "in-active-conflict"
            } else {
                continue;
            };
            warnings.push(WriteWarning {
                id: node.id,
                title: node.title,
                reason: reason.to_string(),
                similarity,
            });
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
            if cfg.versioning.enabled {
                let line = match self.store.current_version()? {
                    Some(v) => format!(
                        "Current working version: {v} — new notes are stamped with it; call `set_version` when the project moves on."
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
                    let line = node_line(n, bc.handoff.excerpt);
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
                     the older), or `dismiss` (unrelated/fine together).";
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
                let line = node_line(&n, bc.recent.excerpt);
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
                let line = node_line(n, bc.open.excerpt);
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
                let fetched = self.store.nodes_by_type_active(&node_type, total)?;
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
                    let line = node_line(&n, excerpt);
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

    /// Queue suspects near one freshly-written node — the write-time half of
    /// the scan, reusing the vector the write already computed.
    fn record_suspects(&self, vec: &[f32], node_id: &str) -> Result<usize> {
        let Some(node) = self.store.get_node(node_id)? else {
            return Ok(0);
        };
        let added = self.suspects_near(&node, vec)?;
        if added > 0 {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(added)
    }

    /// Sweep the whole graph for unlinked look-alike pairs (the pane's
    /// "Scan now" and the daemon's periodic pass). Returns how many new
    /// suspects were queued.
    pub fn scan_conflicts(&self) -> Result<usize> {
        let mut added = 0;
        for node in self.store.scannable_nodes()? {
            let Some(vec) = self.store.embedding_of(&node.id)? else {
                continue;
            };
            added += self.suspects_near(&node, &vec)?;
        }
        if added > 0 {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(added)
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
        )?;
        let mut nodes = Vec::new();
        for h in &hits {
            if let Some(n) = self.store.get_node(&h.id)? {
                nodes.push(n);
            }
        }
        let pairs: Vec<(String, String)> =
            nodes.iter().map(|n| (claim(n), text.to_string())).collect();
        let judgments = nli.judge(&pairs)?;

        let mut report = ClaimReport {
            claim: text.to_string(),
            supports: Vec::new(),
            contradicts: Vec::new(),
            silent: Vec::new(),
        };
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
        Ok(report)
    }

    /// Conflict sweep (the Checkup panel's "Find hidden conflicts"): rescan
    /// at the standing similarity threshold, queueing only pairs the NLI
    /// layer marks as contradictions. The floor stays at 0.85 deliberately:
    /// MNLI-class models presuppose co-reference, and below that band
    /// unrelated same-shaped titles read as confident contradictions (see
    /// the dogfood finding of 2026-07-13 — 140 junk pairs at a 0.8 gate).
    /// Reaching lower waits for a domain-calibrated model via the
    /// judged-suspects eval corpus.
    pub fn audit_conflicts(&self) -> Result<AuditSweep> {
        self.audit_sweep(
            "contradiction",
            self.store.config().policy.conflict_suspect_similarity,
        )
    }

    /// Duplicate sweep (the Audit panel's "Find duplicates"): mutual
    /// entailment above a 0.80 similarity floor — two nodes stating the same
    /// thing. Queued as suspects; the judge's `replaces` verdict is the merge.
    pub fn audit_duplicates(&self) -> Result<AuditSweep> {
        self.audit_sweep("entailment", 0.80)
    }

    /// Shared sweep: nominate unlinked, unraised look-alike pairs whose NLI
    /// hint matches `target`. NLI pair budget capped — an audit that takes a
    /// minute under the engine lock is worse than one that says "truncated,
    /// run me again".
    fn audit_sweep(&self, target: &'static str, floor: f64) -> Result<AuditSweep> {
        const NLI_PAIR_BUDGET: usize = 300;
        let cfg = self.store.config();
        self.require_nli()?;
        let mut sweep = AuditSweep {
            queued: 0,
            examined: 0,
            truncated: false,
        };
        'nodes: for node in self.store.scannable_nodes()? {
            let Some(vec) = self.store.embedding_of(&node.id)? else {
                continue;
            };
            for (id, distance) in self.store.search_vec(&vec, 12)? {
                let similarity = 1.0 - distance;
                if id == node.id {
                    continue;
                }
                if similarity < floor {
                    break; // distance-ordered
                }
                let Some(other) = self.store.get_node(&id)? else {
                    continue;
                };
                if is_anchor(&cfg, &other)
                    || other.valid_until.is_some()
                    || self.store.pair_linked(&node.id, &other.id)?
                    || self.store.suspect_between(&node.id, &other.id)?
                {
                    continue;
                }
                if sweep.examined >= NLI_PAIR_BUDGET {
                    sweep.truncated = true;
                    break 'nodes;
                }
                sweep.examined += 1;
                let Some((label, score, direction)) = self.nli_hint(&node, &other) else {
                    continue;
                };
                if label != target || score < cfg.policy.nli_sweep_min_confidence {
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
                // Answer candidates by role: any non-worklist, non-anchor
                // type can settle an open item (Resolution/Decision/Insight
                // and the canon types in the shipped set).
                let can_answer = cfg
                    .type_def(candidate.node_type.as_str())
                    .is_some_and(|t| !t.roles.worklist && !t.roles.anchor);
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
        let cfg = self.store.config();
        if is_anchor(&cfg, node) || node.valid_until.is_some() {
            return Ok(0);
        }
        let mut added = 0;
        for (id, distance) in self.store.search_vec(vec, WRITE_CHECK_K)? {
            if id == node.id {
                continue;
            }
            let similarity = 1.0 - distance;
            if similarity < cfg.policy.conflict_suspect_similarity {
                break; // distance-ordered: nothing closer follows
            }
            let Some(other) = self.store.get_node(&id)? else {
                continue;
            };
            if is_anchor(&cfg, &other)
                || other.valid_until.is_some()
                || self.store.pair_linked(&node.id, &other.id)?
                || self.store.suspect_between(&node.id, &other.id)?
            {
                continue;
            }
            let (newer, older) = if node.created_at >= other.created_at {
                (&node.id, &other.id)
            } else {
                (&other.id, &node.id)
            };
            let hint = self.nli_hint(node, &other);
            self.store.add_suspect(newer, older, similarity, hint)?;
            added += 1;
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
    pub fn nli_hint(
        &self,
        a: &Node,
        b: &Node,
    ) -> Option<(&'static str, f64, Option<&'static str>)> {
        const DIRECTION_MARGIN: f32 = 0.15;
        let nli = self.nli.as_ref()?;
        let sym = nli.judge_pair(&claim(a), &claim(b)).ok()?;
        let (label, score) = sym.hint();
        let direction = if label == "contradiction" {
            // forward = (a premise → b hypothesis): high forward
            // contradiction reads b as the negated claim.
            let carrier = if sym.forward.contradiction
                >= sym.backward.contradiction + DIRECTION_MARGIN
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
        Some((label, score as f64, direction))
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
                if source == Source::Claude
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
        self.embed_node_into(self.store.as_ref(), node)
    }

    /// The one embedding recipe, aimed at an explicit store — the curated
    /// store on every normal write, the history store for harvested nodes
    /// (same composition, so history search rides the same pipeline).
    fn embed_node_into(&self, store: &dyn Store, node: &Node) -> Result<()> {
        let mut texts = vec![embed_text(
            &node.title,
            node.body.as_deref(),
            &node.tags,
            &node.code_refs,
        )];
        texts.extend(claim_texts(&node.title, node.body.as_deref()));
        let vectors = self.embedder.embed(&texts)?;
        store.upsert_embeddings(&node.id, &vectors)
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
again — just capture knowledge as it emerges.\n";

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
    let mut line = format!("- {} [{} {}", n.title, n.node_type.as_str(), n.id);
    if let Some(version) = n.version.as_deref() {
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
    if let Some(body) = n.body.as_deref().filter(|b| !b.is_empty()) {
        line.push_str(" — ");
        line.push_str(&excerpt_words(&body.replace('\n', " "), excerpt_max));
    }
    line
}

/// Whether a node's type carries the `anchor` role under this graph's
/// ontology (code-subject labels: similar by nature, not by contradiction).
fn is_anchor(cfg: &crate::config::GraphConfig, n: &Node) -> bool {
    cfg.type_def(n.node_type.as_str())
        .is_some_and(|t| t.roles.anchor)
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchFilter {
    pub window: crate::timespec::TimeWindow,
    pub order: crate::timespec::SearchOrder,
}

impl SearchFilter {
    /// Does this filter change anything? Used to keep the unfiltered path
    /// identical rather than merely equivalent.
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
