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
        let suspects = self.scan_conflicts()?;
        let drift = match self.repo_root().map(std::path::Path::to_path_buf) {
            Some(root) => self.scan_code_refs(&root)?.len(),
            None => 0,
        };
        let note = format!(
            "{archived} decayed, {suspects} new suspect{}, {drift} drifted ref{}",
            if suspects == 1 { "" } else { "s" },
            if drift == 1 { "" } else { "s" },
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
        Ok(edge)
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
        for t in types {
            self.check_node_type(t)?;
        }
        let qv = self.embedder.embed_one(query)?;
        let fetch = match &self.reranker {
            Some(_) => (limit * 3).clamp(12, 50),
            None => limit,
        };
        let mut hits = self.store.search_hybrid(query, Some(&qv), types, fetch)?;
        if let Some(reranker) = &self.reranker
            && hits.len() > 1
        {
            self.rerank(reranker.as_ref(), query, &mut hits);
        }
        hits.truncate(limit);
        for hit in &mut hits {
            hit.neighbors = self.store.neighbors(&hit.id, NEIGHBOR_CAP)?;
        }
        // Observability stamp on what was actually returned — never the
        // over-fetched candidates the reranker discarded. (Trust doesn't
        // read this either way; see policy.)
        let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        self.store.touch(&ids)?;
        Ok(hits)
    }

    /// Re-score candidates with the cross-encoder: relevance comes from the
    /// reranker logit (sigmoid-squashed), trust modulates it the same way it
    /// modulates the hybrid blend — relevance dominates, trust breaks ties
    /// (PLAN §6A). A reranker failure keeps hybrid order: precision is an
    /// upgrade, never a dependency.
    fn rerank(&self, reranker: &dyn Reranker, query: &str, hits: &mut [SearchHit]) {
        let docs: Vec<String> = hits
            .iter()
            .map(|h| {
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
        for (hit, logit) in hits.iter_mut().zip(scores) {
            let relevance = 1.0 / (1.0 + (-logit as f64).exp());
            hit.score = relevance * (1.0 + crate::policy::RERANK_TRUST_WEIGHT * hit.trust);
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
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
        let hits = self
            .store
            .search_hybrid(text, Some(&qv), &[], limit.clamp(4, 16))?;
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
            match j.label() {
                "entailment" => report.supports.push(verdict),
                "contradiction" => report.contradicts.push(verdict),
                _ => report.silent.push(verdict),
            }
        }
        report
            .contradicts
            .sort_by(|a, b| b.contradiction.total_cmp(&a.contradiction));
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
    fn nli_hint(&self, a: &Node, b: &Node) -> Option<(&'static str, f64, Option<&'static str>)> {
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
                self.update_node(
                    &suspect.b_id,
                    NodePatch {
                        valid_until: Some(crate::store::now()),
                        ..NodePatch::default()
                    },
                )?;
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
        let mut texts = vec![embed_text(
            &node.title,
            node.body.as_deref(),
            &node.tags,
            &node.code_refs,
        )];
        texts.extend(claim_texts(&node.title, node.body.as_deref()));
        let vectors = self.embedder.embed(&texts)?;
        self.store.upsert_embeddings(&node.id, &vectors)
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
