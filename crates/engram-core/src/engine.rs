//! The integration point the HTTP and MCP layers talk to: a `Store` plus an
//! `Embedder`. It keeps embeddings in lockstep with node writes and exposes
//! the retrieval surface (hybrid search) so callers never touch vectors
//! directly.

use crate::rag::Embedder;
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
}

/// How many 1-hop neighbors ride along with each search hit.
const NEIGHBOR_CAP: usize = 5;
/// How many suspected conflicts the brief lists (strongest first).
const BRIEF_SUSPECT_CAP: usize = 8;
/// How many recently-used tags the brief names — enough for the assistant to
/// reuse the live vocabulary instead of inventing near-synonyms.
const BRIEF_TAG_CAP: usize = 7;
/// How many recently-created nodes the brief lists — the "what changed
/// lately" window, placed right after the suspects so it never falls into
/// the budget-cut tail.
const BRIEF_RECENT_CAP: usize = 7;
/// How many open problems/intents the brief lists (newest first). The full
/// worklist stays one `list_open` away; uncapped it starves the canon sections.
const BRIEF_OPEN_CAP: usize = 10;
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
    store: Store,
    embedder: Box<dyn Embedder>,
    listener: Option<Listener>,
    audit_origin: AuditOrigin,
    /// Binary-side context captured once per process — the enrichment every
    /// audit row carries (PLAN §10 audit journal).
    audit_cwd: Option<String>,
    audit_pid: i64,
    audit_version: String,
}

impl Engine {
    pub fn new(store: Store, embedder: Box<dyn Embedder>) -> Self {
        Self {
            store,
            embedder,
            listener: None,
            audit_origin: AuditOrigin::default(),
            audit_cwd: std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string()),
            audit_pid: std::process::id() as i64,
            audit_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Install the single change listener (the daemon wires this to SSE).
    pub fn set_listener(&mut self, listener: Listener) {
        self.listener = Some(listener);
    }

    /// Stamp who the following writes belong to. Front-ends sharing this
    /// engine call it under their mutex lock before every operation.
    pub fn set_audit_origin(&mut self, origin: AuditOrigin) {
        self.audit_origin = origin;
    }

    fn notify(&self, event: ChangeEvent) {
        if let Some(l) = &self.listener {
            l(event);
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
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
    pub fn add_node(&self, n: NewNode) -> Result<Node> {
        let node = self.store.add_node(n)?;
        self.embed_node(&node)?;
        self.audit_node("created", None, Some(&node))?;
        self.notify(ChangeEvent::NodeAdded(node.clone()));
        Ok(node)
    }

    /// Patch a node and re-embed if any embedded field changed (title, body,
    /// tags, code_refs). Any update refreshes `last_seen` (the store stamps
    /// it): edited knowledge is in-use knowledge.
    pub fn update_node(&self, id: &str, patch: NodePatch) -> Result<Node> {
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

    /// Reconfirm a node without changing its content: refreshes `last_seen`,
    /// restarting trust on the seen curve. Use when a surfaced node proves
    /// still accurate (PLAN §6A trust model).
    pub fn reconfirm(&self, id: &str) -> Result<Node> {
        self.update_node(id, NodePatch::default())
    }

    /// Explicit approval: trust restarts at its ceiling on the slow approved
    /// curve. User action in the pane, or the assistant **only on explicit
    /// user demand / verbatim verification** (enforced by skill policy).
    pub fn approve(&self, id: &str) -> Result<Node> {
        let before = self.store.get_node(id)?;
        let node = self.store.approve(id)?;
        self.audit_node("approved", before.as_ref(), Some(&node))?;
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
        let edge = self.store.add_edge(e)?;
        self.audit_edge("created", None, Some(&edge))?;
        self.notify(ChangeEvent::EdgeAdded(edge.clone()));
        Ok(edge)
    }

    pub fn update_edge(&self, id: &str, p: EdgePatch) -> Result<Edge> {
        let before = self.store.get_edge(id)?;
        let edge = self.store.update_edge(id, p)?;
        self.audit_edge("updated", before.as_ref(), Some(&edge))?;
        self.notify(ChangeEvent::EdgeUpdated(edge.clone()));
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
        })
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
        self.store.import_raw(&graph.nodes, &graph.edges)?;
        for n in &graph.nodes {
            self.embed_node(n)?;
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

    /// Hybrid retrieval: embed the query, fuse keyword + vector hits, then
    /// attach each hit's 1-hop neighbors (conflicts/supersessions first) so
    /// contradictions surface passively with the match (PLAN §6A).
    pub fn search(&self, query: &str, types: &[NodeType], limit: usize) -> Result<Vec<SearchHit>> {
        let qv = self.embedder.embed_one(query)?;
        let mut hits = self.store.search_hybrid(query, Some(&qv), types, limit)?;
        for hit in &mut hits {
            hit.neighbors = self.store.neighbors(&hit.id, NEIGHBOR_CAP)?;
        }
        // Being surfaced is the trust signal: stamp last_seen on every hit
        // (after scoring, so the stamp doesn't influence this query's ranks).
        let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        self.store.touch(&ids)?;
        Ok(hits)
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

        for (id, distance) in self.store.search_vec(&vec, WRITE_CHECK_K)? {
            let similarity = 1.0 - distance;
            if similarity < crate::policy::DUPLICATE_SIMILARITY {
                break; // results are distance-ordered; nothing closer follows
            }
            if let Some(node) = self.store.get_node(&id)?
                && node.node_type == n.node_type
                && node.valid_until.is_none()
            {
                return Ok(WriteOutcome::Matched { node, similarity });
            }
        }

        let node = self.add_node(n)?;
        let warnings = self.write_warnings(&vec, &node.id)?;
        self.record_suspects(&vec, &node.id)?;
        Ok(WriteOutcome::Created { node, warnings })
    }

    /// `update_node` plus conflict warnings when the new text changed.
    pub fn update_node_checked(
        &self,
        id: &str,
        patch: NodePatch,
    ) -> Result<(Node, Vec<WriteWarning>)> {
        let touches_text = patch.title.is_some()
            || patch.body.is_some()
            || patch.tags.is_some()
            || patch.code_refs.is_some();
        let node = self.update_node(id, patch)?;
        let warnings = if touches_text {
            let vec = self.embedder.embed_one(&embed_text(
                &node.title,
                node.body.as_deref(),
                &node.tags,
                &node.code_refs,
            ))?;
            self.record_suspects(&vec, &node.id)?;
            self.write_warnings(&vec, &node.id)?
        } else {
            Vec::new()
        };
        Ok((node, warnings))
    }

    /// Nearby nodes that are contradicted (active `conflicts-with`) or
    /// superseded — returned with writes so the writing assistant notices it
    /// may be re-treading contested or stale ground (PLAN §7, pull-based).
    fn write_warnings(&self, vec: &[f32], exclude_id: &str) -> Result<Vec<WriteWarning>> {
        let mut warnings = Vec::new();
        for (id, distance) in self.store.search_vec(vec, WRITE_CHECK_K)? {
            if id == exclude_id {
                continue;
            }
            let similarity = 1.0 - distance;
            if similarity < crate::policy::WARN_SIMILARITY {
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
    /// the open worklist, then principles/decisions/cautions. Every record uses
    /// one line shape and carries its node id. Every included node's decay
    /// clock is refreshed: being briefed counts as reuse.
    pub fn brief(&self, max_chars: usize) -> Result<String> {
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
            // The live tag vocabulary, up front: one cheap line the writing
            // assistant must see (a budget-cut tail section never surfaces on
            // a mature graph). A genuinely new tag is fine — created on write.
            let tags = self.store.tag_stats(BRIEF_TAG_CAP)?;
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

            let conflicts = self.store.active_conflict_edges()?;
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

            let mut suspects = self.store.suspects_pending()?;
            // Strongest first, capped: the brief is a digest, the full queue
            // lives in list_suspects / the pane.
            suspects.sort_by(|x, y| y.similarity.total_cmp(&x.similarity));
            let overflow = suspects.len().saturating_sub(BRIEF_SUSPECT_CAP);
            suspects.truncate(BRIEF_SUSPECT_CAP);
            if !suspects.is_empty() {
                let heading = "\n## Suspected conflicts — judge these\nThe local scan flagged \
                     unlinked look-alike pairs. For each: `resolve_suspect(id, verdict)` with \
                     `conflict` (they contradict), `replaces` (the newer supersedes — archives \
                     the older), or `dismiss` (unrelated/fine together).";
                if !push_line(&mut out, heading) {
                    break 'assemble;
                }
                for s in suspects {
                    let line = format!(
                        "- {}: \"{}\" [{} {}] vs \"{}\" [{} {}] ({:.0}% similar)",
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
            let recent: Vec<Node> = self
                .store
                .recent_nodes(BRIEF_RECENT_CAP)?
                .into_iter()
                .filter(|n| !seen.contains(&n.id))
                .collect();
            if !recent.is_empty() && !push_line(&mut out, "\n## Recently added") {
                break 'assemble;
            }
            for n in recent {
                let line = node_line(&n, EXCERPT_CHARS);
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
            let open: Vec<Node> = self
                .store
                .list_open(&[])?
                .into_iter()
                .filter(|n| !seen.contains(&n.id))
                .collect();
            if !open.is_empty() && !push_line(&mut out, "\n## Open problems & intents") {
                break 'assemble;
            }
            for n in open.iter().take(BRIEF_OPEN_CAP) {
                let line = node_line(n, EXCERPT_CHARS);
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
                seen.insert(n.id.clone());
                included.push(n.id.clone());
            }
            if open.len() > BRIEF_OPEN_CAP {
                let line = format!(
                    "- …and {} more — `list_open` has the full worklist.",
                    open.len() - BRIEF_OPEN_CAP
                );
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
            }

            // Decisions carry the shortest excerpt: they're the widest section
            // and their titles are already declarative — breadth over depth.
            for (heading, node_type, cap, excerpt) in [
                ("\n## Principles", NodeType::Principle, 8, EXCERPT_CHARS),
                (
                    "\n## Decisions",
                    NodeType::Decision,
                    7,
                    DECISION_EXCERPT_CHARS,
                ),
                ("\n## Cautions", NodeType::Caution, 10, EXCERPT_CHARS),
            ] {
                // Fetch the full active set: nodes already claimed by an
                // earlier section (conflicts, recent) must not starve this
                // one, and `elsewhere` must count every such node — a capped
                // window misses seen nodes ranked below it and the overflow
                // line then double-counts them as "more".
                let total = self.store.count_by_type_active(node_type)? as usize;
                let fetched = self.store.nodes_by_type_active(node_type, total)?;
                let elsewhere = fetched.iter().filter(|n| seen.contains(&n.id)).count();
                let nodes: Vec<Node> = fetched
                    .into_iter()
                    .filter(|n| !seen.contains(&n.id))
                    .take(cap)
                    .collect();
                if !nodes.is_empty() && !push_line(&mut out, heading) {
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
        Ok(out)
    }

    // ---- conflict scan (PLAN §7): detection is local and automatic; judgment
    // stays with Claude in-session or the user in the pane. The daemon never
    // calls an LLM.

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

    /// Timeline (PLAN §10): the chronological story of one piece of
    /// knowledge — every generation connected to `id` through `replaces`
    /// edges, oldest first. A node that was never part of a supersession
    /// yields a single-entry timeline. Each superseded generation carries the
    /// note of the `replaces` edge that retired it (the why of the change).
    pub fn timeline(&self, id: &str) -> Result<Vec<TimelineEntry>> {
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
                if e.edge_type != EdgeType::Replaces {
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
    /// the graph and reality, surfaced for review like a conflict. Free-text
    /// responsibility labels (anything with whitespace) are not checkable
    /// and never drift.
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
        if node.node_type == NodeType::Anchor || node.valid_until.is_some() {
            return Ok(0);
        }
        let mut added = 0;
        for (id, distance) in self.store.search_vec(vec, WRITE_CHECK_K)? {
            if id == node.id {
                continue;
            }
            let similarity = 1.0 - distance;
            if similarity < crate::policy::CONFLICT_SUSPECT_SIMILARITY {
                break; // distance-ordered: nothing closer follows
            }
            let Some(other) = self.store.get_node(&id)? else {
                continue;
            };
            if other.node_type == NodeType::Anchor
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
            self.store.add_suspect(newer, older, similarity)?;
            added += 1;
        }
        Ok(added)
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
                edge_type: EdgeType::ConflictsWith,
                from_id: suspect.a_id.clone(),
                to_id: suspect.b_id.clone(),
                source,
                note: Some("confirmed from conflict scan".into()),
                confidence: Some(suspect.similarity),
                strength: None,
                status: None,
            })?),
            SuspectVerdict::Replaces => {
                let edge = self.add_edge(NewEdge {
                    edge_type: EdgeType::Replaces,
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
        let vec = self.embedder.embed_one(&embed_text(
            &node.title,
            node.body.as_deref(),
            &node.tags,
            &node.code_refs,
        ))?;
        self.store.upsert_embedding(&node.id, &vec)
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

/// A code_ref reads as a checkable path when it has no whitespace and looks
/// filesystem-shaped (a separator or an extension dot); "auth flow"-style
/// responsibility labels fail this and are skipped by the drift scan.
fn ref_is_path(r: &str) -> bool {
    !r.is_empty() && !r.contains(char::is_whitespace) && (r.contains('/') || r.contains('.'))
}

/// Which embedding composition stored vectors were computed with (kept in
/// `PRAGMA user_version`). Bump when [`embed_text`] changes what it includes;
/// [`Engine::ensure_embed_composition`] re-embeds databases that are behind.
/// 0 = legacy title+body; 2 = full fields (title, body, tags, code_refs).
pub const EMBED_COMPOSITION: i64 = 2;

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
const EXCERPT_CHARS: usize = 140;
/// The Decisions section runs shorter excerpts: decision titles are already
/// declarative sentences, so the body preview only needs to hint at the why.
const DECISION_EXCERPT_CHARS: usize = 80;

/// One brief line per node, one uniform shape everywhere:
/// `- Title [Type id status STALE] — excerpt`. Every record carries its id so
/// the assistant can act on it directly (`get_node`, `traverse`,
/// `update_node`) without a `search` round-trip.
fn node_line(n: &Node, excerpt_max: usize) -> String {
    let mut line = format!("- {} [{} {}", n.title, n.node_type.as_str(), n.id);
    if let Some(status) = n.status {
        line.push(' ');
        line.push_str(status.as_str());
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
