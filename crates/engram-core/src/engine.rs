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

    /// Add a node and embed it (title + body) in one step. Trust is computed
    /// from timestamps at read time; user-authored nodes are approved by
    /// construction (the store stamps `approved_at`).
    pub fn add_node(&self, n: NewNode) -> Result<Node> {
        let node = self.store.add_node(n)?;
        self.embed_node(&node)?;
        self.notify(ChangeEvent::NodeAdded(node.clone()));
        Ok(node)
    }

    /// Patch a node and re-embed if its text changed. Any update refreshes
    /// `last_seen` (the store stamps it): edited knowledge is in-use knowledge.
    pub fn update_node(&self, id: &str, patch: NodePatch) -> Result<Node> {
        let touches_text = patch.title.is_some() || patch.body.is_some();
        let node = self.store.update_node(id, patch)?;
        if touches_text {
            self.embed_node(&node)?;
        }
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
        let node = self.store.approve(id)?;
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
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
        self.record_suspects(&vec, &node.id)?;
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
                        "- {}: \"{}\" [{}] vs \"{}\" [{}] ({:.0}% similar)",
                        s.id,
                        s.a.title,
                        s.a.node_type.as_str(),
                        s.b.title,
                        s.b.node_type.as_str(),
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
        for id in &ids {
            if let Some(node) = self.store.get_node(id)? {
                self.notify(ChangeEvent::NodeUpdated(node));
            }
        }
        Ok(ids)
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
    if n.stale {
        line.push_str(" STALE");
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
