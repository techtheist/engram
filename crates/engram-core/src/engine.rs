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
}

/// How many 1-hop neighbors ride along with each search hit.
const NEIGHBOR_CAP: usize = 5;
/// How many nearest nodes the write-time duplicate/conflict checks consider.
const WRITE_CHECK_K: usize = 8;

pub type Listener = Box<dyn Fn(ChangeEvent) + Send + Sync>;

pub struct Engine {
    store: Store,
    embedder: Box<dyn Embedder>,
    listener: Option<Listener>,
}

impl Engine {
    pub fn new(store: Store, embedder: Box<dyn Embedder>) -> Self {
        Self {
            store,
            embedder,
            listener: None,
        }
    }

    /// Install the single change listener (the daemon wires this to SSE).
    pub fn set_listener(&mut self, listener: Listener) {
        self.listener = Some(listener);
    }

    fn notify(&self, event: ChangeEvent) {
        if let Some(l) = &self.listener {
            l(event);
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Add a node and embed it (title + body) in one step. Applies the trust
    /// policy: user nodes start trusted, Claude nodes start provisional.
    pub fn add_node(&self, mut n: NewNode) -> Result<Node> {
        if n.confidence.is_none() {
            n.confidence = Some(match n.source {
                Source::User => crate::policy::USER_CONFIDENCE,
                Source::Claude => crate::policy::PROVISIONAL_CONFIDENCE,
            });
        }
        let node = self.store.add_node(n)?;
        self.embed_node(&node)?;
        self.notify(ChangeEvent::NodeAdded(node.clone()));
        Ok(node)
    }

    /// Patch a node and re-embed if its text changed. An update is a
    /// *reconfirmation*: unless the caller sets confidence explicitly, nudge it
    /// toward trusted (capped per source). The store also refreshes the node's
    /// decay clock.
    pub fn update_node(&self, id: &str, mut patch: NodePatch) -> Result<Node> {
        let touches_text = patch.title.is_some() || patch.body.is_some();
        if patch.confidence.is_none()
            && let Some(cur) = self.store.get_node(id)?
        {
            let cap = match cur.source {
                Source::User => crate::policy::USER_CONFIDENCE,
                Source::Claude => crate::policy::CLAUDE_CONFIDENCE_CAP,
            };
            let bumped = (cur
                .confidence
                .unwrap_or(crate::policy::PROVISIONAL_CONFIDENCE)
                + crate::policy::RECONFIRM_BUMP)
                .min(cap);
            patch.confidence = Some(bumped);
        }
        let node = self.store.update_node(id, patch)?;
        if touches_text {
            self.embed_node(&node)?;
        }
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
    }

    /// Reconfirm a node without changing its content: bumps confidence toward
    /// trusted and refreshes the decay clock. Use when search surfaces a node
    /// that's still accurate (PLAN §6A trust model).
    pub fn reconfirm(&self, id: &str) -> Result<Node> {
        self.update_node(id, NodePatch::default())
    }

    /// Archive stale provisional episodic nodes (not reconfirmed within `ttl`).
    /// Returns the archived ids. Trusted, stable, and user nodes are untouched.
    pub fn decay(&self, ttl_secs: i64) -> Result<Vec<String>> {
        let archived = self.store.decay(ttl_secs, crate::now())?;
        for id in &archived {
            if let Some(node) = self.store.get_node(id)? {
                self.notify(ChangeEvent::NodeUpdated(node));
            }
        }
        Ok(archived)
    }

    /// What `decay` would archive, without archiving — `as_of` simulates a
    /// future clock so decay behavior is testable before two weeks pass.
    pub fn decay_preview(&self, ttl_secs: i64, as_of: Option<i64>) -> Result<Vec<String>> {
        self.store
            .decay_candidates(ttl_secs, as_of.unwrap_or_else(crate::now))
    }

    pub fn delete_node(&self, id: &str) -> Result<bool> {
        let removed = self.store.delete_node(id)?;
        if removed {
            self.notify(ChangeEvent::NodeDeleted(id.to_string()));
        }
        Ok(removed)
    }

    pub fn get_node(&self, id: &str) -> Result<Option<Node>> {
        self.store.get_node(id)
    }

    pub fn add_edge(&self, e: NewEdge) -> Result<Edge> {
        let edge = self.store.add_edge(e)?;
        self.notify(ChangeEvent::EdgeAdded(edge.clone()));
        Ok(edge)
    }

    pub fn update_edge(&self, id: &str, p: EdgePatch) -> Result<Edge> {
        let edge = self.store.update_edge(id, p)?;
        self.notify(ChangeEvent::EdgeUpdated(edge.clone()));
        Ok(edge)
    }

    /// Remove one edge. Unlike node deletion this is open to Claude too —
    /// repairing a mislink must not require the pane.
    pub fn delete_edge(&self, id: &str) -> Result<bool> {
        let removed = self.store.delete_edge(id)?;
        if removed {
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
    /// are sorted (created_at, id) so re-exports produce stable git diffs.
    pub fn export(&self) -> Result<ExportGraph> {
        let mut nodes = self.store.all_nodes()?;
        let mut edges = self.store.all_edges()?;
        let key_n = |n: &Node| (n.created_at, n.id.clone());
        let key_e = |e: &Edge| (e.created_at, e.id.clone());
        nodes.sort_by_key(key_n);
        edges.sort_by_key(key_e);
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
        Ok(ImportSummary {
            nodes: graph.nodes.len(),
            edges: graph.edges.len(),
        })
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
        let vec = self
            .embedder
            .embed_one(&embed_text(&scrubbed_title, scrubbed_body.as_deref()))?;

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
        Ok(WriteOutcome::Created { node, warnings })
    }

    /// `update_node` plus conflict warnings when the new text changed.
    pub fn update_node_checked(
        &self,
        id: &str,
        patch: NodePatch,
    ) -> Result<(Node, Vec<WriteWarning>)> {
        let touches_text = patch.title.is_some() || patch.body.is_some();
        let node = self.update_node(id, patch)?;
        let warnings = if touches_text {
            let vec = self
                .embedder
                .embed_one(&embed_text(&node.title, node.body.as_deref()))?;
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
    /// canon — unresolved conflicts, the open worklist, principles, decisions,
    /// cautions, and what changed recently. Every included node's decay clock is
    /// refreshed: being briefed counts as reuse.
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
                    "- \"{}\" [{}] conflicts with \"{}\" [{}]",
                    a.title,
                    a.node_type.as_str(),
                    b.title,
                    b.node_type.as_str(),
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

            let open = self.store.list_open(&[])?;
            if !open.is_empty() && !push_line(&mut out, "\n## Open problems & intents") {
                break 'assemble;
            }
            for n in open {
                if !seen.insert(n.id.clone()) {
                    continue;
                }
                let line = node_line(&n, false);
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
                included.push(n.id);
            }

            for (heading, node_type, cap) in [
                ("\n## Principles", NodeType::Principle, 8),
                ("\n## Decisions", NodeType::Decision, 12),
                ("\n## Cautions", NodeType::Caution, 10),
            ] {
                let nodes = self.store.nodes_by_type_active(node_type, cap)?;
                if !nodes.is_empty() && !push_line(&mut out, heading) {
                    break 'assemble;
                }
                for n in nodes {
                    if !seen.insert(n.id.clone()) {
                        continue;
                    }
                    let line = node_line(&n, false);
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                    included.push(n.id);
                }
            }

            let recent: Vec<Node> = self
                .store
                .recent_nodes(5)?
                .into_iter()
                .filter(|n| !seen.contains(&n.id))
                .collect();
            if !recent.is_empty() && !push_line(&mut out, "\n## Recently added") {
                break 'assemble;
            }
            for n in recent {
                let line = node_line(&n, true);
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
                seen.insert(n.id.clone());
                included.push(n.id);
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

    fn embed_node(&self, node: &Node) -> Result<()> {
        let vec = self
            .embedder
            .embed_one(&embed_text(&node.title, node.body.as_deref()))?;
        self.store.upsert_embedding(&node.id, &vec)
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

/// The text a node is embedded as — kept in one place so write-time similarity
/// checks embed exactly what storage embeds.
fn embed_text(title: &str, body: Option<&str>) -> String {
    match body {
        Some(b) if !b.is_empty() => format!("{title}\n{b}"),
        _ => title.to_string(),
    }
}

/// Longest excerpt a brief line carries. Word-boundary cut, so lines read as
/// prose, not as a mid-token truncation.
const EXCERPT_CHARS: usize = 240;

/// One brief line per node: title, type (+ status when set), then a
/// single-line excerpt. Ids appear only where `with_id` — the brief is prose
/// to read, not a lookup table; anything here is re-findable via `search`.
fn node_line(n: &Node, with_id: bool) -> String {
    let mut line = format!("- {} [{}", n.title, n.node_type.as_str());
    if with_id {
        line.push(' ');
        line.push_str(&n.id);
    }
    if let Some(status) = n.status {
        line.push(' ');
        line.push_str(status.as_str());
    }
    line.push(']');
    if let Some(body) = n.body.as_deref().filter(|b| !b.is_empty()) {
        line.push_str(" — ");
        line.push_str(&excerpt_words(&body.replace('\n', " "), EXCERPT_CHARS));
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
