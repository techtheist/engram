//! The storage boundary (PLAN §7C step 5). [`Store`] is the trait every
//! backend implements; `SqliteStore` (store_sqlite) is the reference
//! implementation and `TepinStore` (store_tepin) the TepinDB driver. Engine
//! and Hub talk only to this trait — which backend a graph file uses is an
//! open-time decision ([`open_store`]), never an application-logic change.
//!
//! Composite reads that are pure Rust over the primitives — hybrid fusion,
//! neighbor collection, traversal, decay filtering — live here as provided
//! methods so every backend shares one behavior; backends may override them
//! only to say the same thing faster.

use std::path::Path;

use crate::Result;
use crate::types::*;

/// Unix seconds. The single clock for created_at / valid_from.
pub fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// FTS snippet match markers: private-use sentinels no real body text carries
/// (bodies legitimately contain `[` — node-id references would false-mark).
/// The MCP layer rewrites them to `[`/`]` for assistants; the pane turns them
/// into `<mark>` highlights.
pub const SNIPPET_OPEN: char = '\u{e000}';
pub const SNIPPET_CLOSE: char = '\u{e001}';

/// Open the store backing `path`, picking the backend by file identity:
/// a `.tepin` file (or path) gets the TepinDB driver, everything else the
/// SQLite reference implementation. The cutover (PLAN §7C step 5) is exactly
/// this dispatch — application code above never branches on backend.
pub fn open_store(path: impl AsRef<Path>) -> Result<Box<dyn Store>> {
    let path = resolve_db_path(path.as_ref());
    if crate::store_tepin::is_tepin_path(&path) {
        return Ok(Box::new(crate::store_tepin::TepinStore::open(&path)?));
    }
    Ok(Box::new(crate::store_sqlite::SqliteStore::open(&path)?))
}

/// The path dance around the storage sunset: wiring everywhere (hooks,
/// `.mcp.json`, the registry, clap defaults) names `graph.db`, and this
/// resolution decides what that means today. A migrated repo's `.tepin`
/// sibling always wins (the SQLite file stays behind as the backup it is);
/// an existing `graph.db` with no sibling keeps working untouched; and a
/// brand-new store — neither file exists — is born `graph.tepin` (v0.6.2,
/// the roadmap's first sunset step: new graphs never start on SQLite).
pub fn resolve_db_path(path: &Path) -> std::path::PathBuf {
    if path.extension().is_some_and(|e| e == "db") {
        let tepin = path.with_extension("tepin");
        if tepin.is_file() || !path.exists() {
            return tepin;
        }
    }
    path.to_path_buf()
}

/// One repo's graph store. All methods take `&self`; writes are serialized by
/// the daemon's single-writer design (every Engine sits behind a mutex).
/// `Send` because engines cross thread boundaries inside `Arc<Mutex<_>>`.
pub trait Store: Send {
    // ---- store-level metadata -------------------------------------------

    /// Which embedding composition the stored vectors were computed with
    /// (see `engine::EMBED_COMPOSITION`). 0 = legacy title+body. The engine
    /// re-embeds and bumps this when it's behind.
    fn embed_version(&self) -> Result<i64>;
    fn set_embed_version(&self, v: i64) -> Result<()>;

    /// The embedding model identity this store's vectors were computed with;
    /// `None` on stores that predate model selection (treated as the default
    /// model by the engine's guard).
    fn embed_model(&self) -> Result<Option<EmbedModelId>>;
    fn set_embed_model(&self, model: &EmbedModelId) -> Result<()>;

    /// Drop every stored vector and re-shape vector storage for `dim`-wide
    /// embeddings. The engine calls this only inside the guarded re-embed
    /// that immediately repopulates the vectors (model swap, PLAN §7A).
    fn reset_vectors(&self, dim: usize) -> Result<()>;

    /// Backend-neutral counts for `/system` and doctor.
    fn stats(&self) -> Result<StoreStats>;

    /// Backend-reported integrity (SQLite: journal mode + quick_check).
    fn health(&self) -> Result<StoreHealth>;

    // ---- nodes -----------------------------------------------------------

    fn add_node(&self, n: NewNode) -> Result<Node>;
    fn get_node(&self, id: &str) -> Result<Option<Node>>;
    fn update_node(&self, id: &str, p: NodePatch) -> Result<Node>;
    /// Stamp an explicit approval: trust restarts at its ceiling. Re-approving
    /// refreshes the stamp; approval also clears any evidence demotion.
    fn approve(&self, id: &str) -> Result<Node>;
    /// Withdraw an approval: trust falls back to the confirmed/created anchor.
    /// Also clears any pin — revoking is the "undo my endorsements" gesture.
    fn revoke_approval(&self, id: &str) -> Result<Node>;
    /// Set (or clear, with `None`) the user's constant-trust pin, clamped 0..=1.
    fn set_trust_override(&self, id: &str, value: Option<f64>) -> Result<Node>;
    /// Stamp contradicting evidence on a node — the event that starts the decay
    /// ramp on stable knowledge. No-op when already demoted or pinned. Returns
    /// whether the stamp landed.
    fn demote(&self, id: &str, ts: i64) -> Result<bool>;
    /// Withdraw a demotion (the evidence that caused it is gone).
    fn clear_demotion(&self, id: &str) -> Result<Node>;
    /// User-only hard delete; cascades the node's edges and suspects (PLAN §6B).
    fn delete_node(&self, id: &str) -> Result<bool>;
    /// Insert or replace a node by id, preserving its timestamps (import).
    fn upsert_node(&self, n: &Node) -> Result<()>;
    fn all_nodes(&self) -> Result<Vec<Node>>;
    /// Stamp `last_seen`: retrieval surfaced these nodes. Observability only —
    /// trust never reads this stamp.
    fn touch(&self, ids: &[String]) -> Result<()>;
    /// Rewrite a node's creation clock (created_at + valid_from). Maintenance
    /// and test support — decay scenarios need a past that no live write can
    /// produce.
    fn backdate_node(&self, id: &str, created_at: i64) -> Result<()>;

    // ---- edges -----------------------------------------------------------

    fn add_edge(&self, e: NewEdge) -> Result<Edge>;
    fn get_edge(&self, id: &str) -> Result<Option<Edge>>;
    fn update_edge(&self, id: &str, p: EdgePatch) -> Result<Edge>;
    fn delete_edge(&self, id: &str) -> Result<bool>;
    fn upsert_edge(&self, e: &Edge) -> Result<()>;
    fn edges_out(&self, node_id: &str) -> Result<Vec<Edge>>;
    fn edges_in(&self, node_id: &str) -> Result<Vec<Edge>>;
    fn all_edges(&self) -> Result<Vec<Edge>>;

    // ---- bulk ------------------------------------------------------------

    /// Import nodes (first) then edges in ONE atomic unit so references hold
    /// and a failure rolls the whole import back. Embeddings are regenerated
    /// by the caller (Engine) after this returns.
    fn import_raw(&self, nodes: &[Node], edges: &[Edge]) -> Result<()>;
    /// Archive nodes atomically: sets `valid_until`, preserving history
    /// (supersede-not-delete, PLAN §6B).
    fn archive_nodes(&self, ids: &[String], ts: i64) -> Result<()>;

    // ---- search primitives ----------------------------------------------

    /// Keyword search over title/body/tags/code_refs with per-hit snippet
    /// (matches marked with [`SNIPPET_OPEN`]/[`SNIPPET_CLOSE`]), higher
    /// score = better, archived nodes excluded.
    fn search_fts(&self, query: &str, types: &[NodeType], limit: usize) -> Result<Vec<SearchHit>>;
    /// k-nearest node ids by cosine distance (smaller = closer).
    fn search_vec(&self, query: &[f32], k: usize) -> Result<Vec<(String, f64)>>;
    /// Store (or replace) a node's embedding.
    fn upsert_embedding(&self, node_id: &str, embedding: &[f32]) -> Result<()>;
    /// The stored embedding for a node.
    fn embedding_of(&self, node_id: &str) -> Result<Option<Vec<f32>>>;

    // ---- suspects (conflict-scan queue) ---------------------------------

    /// Whether the pair was ever raised, in either order and any status —
    /// judged (confirmed/dismissed) pairs are never re-raised.
    fn suspect_between(&self, a: &str, b: &str) -> Result<bool>;
    /// Queue a suspected pair (caller orders newer-first: a = newer). The
    /// optional NLI hint rides along for queue triage — it suggests, never
    /// judges.
    fn add_suspect(
        &self,
        a_id: &str,
        b_id: &str,
        similarity: f64,
        hint: Option<(&str, f64)>,
    ) -> Result<Suspect>;
    fn get_suspect(&self, id: &str) -> Result<Option<Suspect>>;
    fn set_suspect_status(&self, id: &str, status: SuspectStatus) -> Result<Suspect>;
    /// Pending suspects with their endpoints' display fields, newest first.
    /// Pairs with an archived endpoint drop out.
    fn suspects_pending(&self) -> Result<Vec<SuspectView>>;
    /// Every suspect row, any status — judged pairs included (they carry the
    /// never-re-raise memory the migration must not lose).
    fn all_suspects(&self) -> Result<Vec<Suspect>>;
    /// Insert or replace a suspect verbatim, preserving id, timestamps and
    /// judgment (migration / restore).
    fn upsert_suspect(&self, s: &Suspect) -> Result<()>;

    // ---- audit journal ---------------------------------------------------

    /// Append one journal row. `seq` on the input is ignored (assigned by the
    /// store); rows are only ever inserted, never updated or deleted.
    fn add_audit(&self, e: &AuditEntry) -> Result<()>;
    /// One journal page, newest first. Keyset pagination: pass the last seen
    /// `seq` as `before` for the next page; `entity_id` narrows to one
    /// node/edge's history; `total` counts under the same filter.
    fn audit_page(
        &self,
        before: Option<i64>,
        entity_id: Option<&str>,
        limit: usize,
    ) -> Result<AuditPage>;

    // ---- tags ------------------------------------------------------------

    /// Every tag in use on current nodes with count and freshness, freshest
    /// first.
    fn tag_stats(&self, limit: usize) -> Result<Vec<TagStat>>;

    // ---- provided composites (shared behavior, pure Rust) ----------------

    /// Open Problems/Intents — the live worklist.
    fn list_open(&self, types: &[NodeType]) -> Result<Vec<Node>> {
        let types = if types.is_empty() {
            &[NodeType::Problem, NodeType::Intent][..]
        } else {
            types
        };
        let mut out: Vec<Node> = self
            .all_nodes()?
            .into_iter()
            .filter(|n| {
                n.valid_until.is_none()
                    && n.status == Some(NodeStatus::Open)
                    && types.contains(&n.node_type)
            })
            .collect();
        sort_newest_first(&mut out);
        Ok(out)
    }

    /// Nodes touched by an active `conflicts-with` edge — the contradiction
    /// surface shown in the worklist.
    fn nodes_in_active_conflicts(&self) -> Result<Vec<Node>> {
        let mut ids: Vec<String> = Vec::new();
        for e in self.active_conflict_edges()? {
            for id in [&e.from_id, &e.to_id] {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
        let mut out = Vec::new();
        for id in ids {
            if let Some(n) = self.get_node(&id)? {
                out.push(n);
            }
        }
        sort_newest_first(&mut out);
        Ok(out)
    }

    /// Active `conflicts-with` edges — the unresolved contradiction list.
    fn active_conflict_edges(&self) -> Result<Vec<Edge>> {
        let mut out: Vec<Edge> = self
            .all_edges()?
            .into_iter()
            .filter(|e| {
                e.edge_type == EdgeType::ConflictsWith
                    && matches!(e.status, None | Some(EdgeStatus::Active))
                    && e.valid_until.is_none()
            })
            .collect();
        out.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        Ok(out)
    }

    /// Whether a node sits on an active `conflicts-with` edge.
    fn has_active_conflict(&self, id: &str) -> Result<bool> {
        let on_edge = |e: &Edge| {
            e.edge_type == EdgeType::ConflictsWith
                && matches!(e.status, None | Some(EdgeStatus::Active))
        };
        Ok(self.edges_out(id)?.iter().any(on_edge) || self.edges_in(id)?.iter().any(on_edge))
    }

    /// Whether any edge — either direction, any type or status — connects the
    /// pair. Linked nodes are already consciously related: not scan material.
    fn pair_linked(&self, a: &str, b: &str) -> Result<bool> {
        Ok(self.edges_out(a)?.iter().any(|e| e.to_id == b)
            || self.edges_in(a)?.iter().any(|e| e.from_id == b))
    }

    /// Active non-anchor nodes — the conflict scan's iteration set (anchor
    /// labels are similar by nature, not by contradiction).
    fn scannable_nodes(&self) -> Result<Vec<Node>> {
        let mut out: Vec<Node> = self
            .all_nodes()?
            .into_iter()
            .filter(|n| n.valid_until.is_none() && n.node_type != NodeType::Anchor)
            .collect();
        sort_newest_first(&mut out);
        Ok(out)
    }

    /// How many current (non-archived) nodes of one type exist.
    fn count_by_type_active(&self, t: NodeType) -> Result<i64> {
        Ok(self
            .all_nodes()?
            .iter()
            .filter(|n| n.valid_until.is_none() && n.node_type == t)
            .count() as i64)
    }

    /// Current (non-archived) nodes of one type, most trusted first. Ordered
    /// by deliberate-act timestamps, never last_seen — otherwise the brief
    /// would re-select whatever it briefed yesterday (inclusion stamps
    /// last_seen), a self-reinforcing loop.
    fn nodes_by_type_active(&self, t: NodeType, limit: usize) -> Result<Vec<Node>> {
        let mut out: Vec<Node> = self
            .all_nodes()?
            .into_iter()
            .filter(|n| n.valid_until.is_none() && n.node_type == t)
            .collect();
        out.sort_by(|a, b| {
            let key = |n: &Node| {
                (
                    n.trust_override.is_some(),
                    n.approved_at.is_some(),
                    n.approved_at.or(n.confirmed_at).unwrap_or(n.created_at),
                    n.id.clone(),
                )
            };
            key(b).cmp(&key(a))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Most recently created current nodes (any type); id breaks second-level
    /// timestamp ties by insertion order (ids are time-sortable).
    fn recent_nodes(&self, limit: usize) -> Result<Vec<Node>> {
        let mut out: Vec<Node> = self
            .all_nodes()?
            .into_iter()
            .filter(|n| n.valid_until.is_none())
            .collect();
        sort_newest_first(&mut out);
        out.truncate(limit);
        Ok(out)
    }

    /// Nodes the decay pass would archive at `now_ts`: active, Claude-authored,
    /// never approved, unpinned, episodic/volatile, and stale for longer than
    /// `ttl_secs` (policy::stale_since). Stable durability, pins, and
    /// user/approved knowledge never auto-archive.
    fn decay_candidates(&self, ttl_secs: i64, now_ts: i64) -> Result<Vec<Node>> {
        let mut pool: Vec<Node> = self
            .all_nodes()?
            .into_iter()
            .filter(|n| {
                n.valid_until.is_none()
                    && n.source == Source::Claude
                    && n.approved_at.is_none()
                    && n.trust_override.is_none()
                    && matches!(n.durability, Durability::Episodic | Durability::Volatile)
            })
            .collect();
        pool.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        let mut out = Vec::new();
        for node in pool {
            let Some(since) = crate::policy::stale_since(&node.trust_inputs()) else {
                continue;
            };
            if now_ts - since >= ttl_secs {
                out.push(node);
            }
        }
        Ok(out)
    }

    /// Hybrid retrieval: blend normalized keyword (bm25) and vector (cosine)
    /// relevance, then *modulate* by trust (user-sourced / stable /
    /// high-confidence). Trust multiplies relevance rather than adding to it,
    /// so an irrelevant-but-trusted node can't outrank an actual match —
    /// PLAN §6A retrieval.
    fn search_hybrid(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        types: &[NodeType],
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        use std::collections::HashMap;

        let over = (limit * 4).max(20);
        let fts = self.search_fts(query, types, over)?;
        let vec_hits = match query_vec {
            Some(v) => self.search_vec(v, over)?,
            None => Vec::new(),
        };

        let max_fts = fts.iter().map(|h| h.score).fold(0.0_f64, f64::max);
        let mut keyword: HashMap<String, f64> = HashMap::new();
        let mut snippets: HashMap<String, String> = HashMap::new();
        for h in fts {
            if max_fts > 0.0 {
                keyword.insert(h.id.clone(), h.score / max_fts);
            }
            snippets.insert(h.id, h.snippet);
        }
        let mut semantic: HashMap<String, f64> = HashMap::new();
        for (id, distance) in vec_hits {
            semantic.insert(id, (1.0 - distance).clamp(0.0, 1.0));
        }

        let ids: std::collections::HashSet<String> =
            keyword.keys().chain(semantic.keys()).cloned().collect();

        let mut hits: Vec<SearchHit> = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(node) = self.get_node(&id)? else {
                continue;
            };
            // Archived/superseded nodes (vector candidates) don't surface.
            if node.valid_until.is_some() {
                continue;
            }
            if !types.is_empty() && !types.contains(&node.node_type) {
                continue;
            }
            let kw = keyword.get(&id).copied().unwrap_or(0.0);
            let sem_raw = semantic.get(&id).copied().unwrap_or(0.0);
            let sem = ((sem_raw - crate::policy::SEARCH_SEMANTIC_FLOOR)
                / (1.0 - crate::policy::SEARCH_SEMANTIC_FLOOR))
                .clamp(0.0, 1.0);
            let relevance = 0.5 * kw + 0.5 * sem;
            if relevance <= 0.0 {
                continue;
            }
            let age = now() - node.created_at;
            let score = relevance * (1.0 + trust_boost(&node)) * recency_factor(age);
            let snippet = snippets
                .remove(id.as_str())
                .unwrap_or_else(|| excerpt(&node));
            hits.push(SearchHit {
                id: node.id,
                node_type: node.node_type,
                title: node.title,
                snippet,
                score,
                durability: node.durability,
                status: node.status,
                trust: node.trust,
                stale: node.stale,
                neighbors: Vec::new(),
                project: None,
            });
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(top) = hits.first().map(|h| h.score) {
            hits.retain(|h| {
                h.score >= crate::policy::SEARCH_MIN_SCORE
                    && h.score >= top * crate::policy::SEARCH_RELATIVE_CUT
            });
        }
        hits.truncate(limit);
        Ok(hits)
    }

    /// A hit's 1-hop subgraph as compact refs, `conflicts-with` then `replaces`
    /// first (the edges that make retrieval *active*), capped.
    fn neighbors(&self, id: &str, cap: usize) -> Result<Vec<NeighborRef>> {
        let mut refs: Vec<NeighborRef> = Vec::new();
        let incident = self
            .edges_out(id)?
            .into_iter()
            .map(|e| (e, "out"))
            .chain(self.edges_in(id)?.into_iter().map(|e| (e, "in")));
        for (e, direction) in incident {
            let other = if direction == "out" {
                &e.to_id
            } else {
                &e.from_id
            };
            let Some(n) = self.get_node(other)? else {
                continue;
            };
            refs.push(NeighborRef {
                edge_id: e.id,
                edge_type: e.edge_type,
                direction: direction.to_string(),
                edge_status: e.status,
                id: n.id,
                node_type: n.node_type,
                title: n.title,
                archived: n.valid_until.is_some(),
            });
        }
        refs.sort_by_key(|r| match r.edge_type {
            EdgeType::ConflictsWith => 0,
            EdgeType::Replaces => 1,
            _ => 2,
        });
        refs.truncate(cap);
        Ok(refs)
    }

    /// Bounded breadth-first subgraph around `from` (PLAN Appendix A `traverse`).
    fn traverse(
        &self,
        from: &str,
        edge_types: &[EdgeType],
        depth: usize,
    ) -> Result<(Vec<Node>, Vec<Edge>)> {
        use std::collections::{HashSet, VecDeque};
        let mut seen_nodes: HashSet<String> = HashSet::new();
        let mut seen_edges: HashSet<String> = HashSet::new();
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut queue = VecDeque::new();

        if let Some(n) = self.get_node(from)? {
            seen_nodes.insert(n.id.clone());
            nodes.push(n);
            queue.push_back((from.to_string(), 0usize));
        }
        while let Some((id, d)) = queue.pop_front() {
            if d >= depth {
                continue;
            }
            let mut incident = self.edges_out(&id)?;
            incident.extend(self.edges_in(&id)?);
            for e in incident {
                if !edge_types.is_empty() && !edge_types.contains(&e.edge_type) {
                    continue;
                }
                let other = if e.from_id == id {
                    e.to_id.clone()
                } else {
                    e.from_id.clone()
                };
                if seen_edges.insert(e.id.clone()) {
                    edges.push(e);
                }
                if seen_nodes.insert(other.clone()) {
                    if let Some(n) = self.get_node(&other)? {
                        nodes.push(n);
                    }
                    queue.push_back((other, d + 1));
                }
            }
        }
        Ok((nodes, edges))
    }
}

/// Newest first by (created_at, id) — ids are time-sortable, so the id
/// tie-break matches insertion order across backends.
fn sort_newest_first(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
}

/// Newness multiplier for hybrid search, in [1.0, 1.0 + SEARCH_RECENCY_BOOST]:
/// full bonus at age zero, halving every SEARCH_RECENCY_HALF_LIFE_SECS. Breaks
/// near-ties toward current knowledge; embeddings carry no time signal, so the
/// preference has to live in scoring.
fn recency_factor(age_secs: i64) -> f64 {
    let half_life = crate::policy::SEARCH_RECENCY_HALF_LIFE_SECS as f64;
    let decayed = 0.5_f64.powf(age_secs.max(0) as f64 / half_life);
    1.0 + crate::policy::SEARCH_RECENCY_BOOST * decayed
}

#[cfg(test)]
pub(crate) fn recency_factor_for_tests(age_secs: i64) -> f64 {
    recency_factor(age_secs)
}

fn trust_boost(node: &Node) -> f64 {
    let mut b = 0.0;
    if node.source == Source::User {
        b += 0.15;
    }
    match node.durability {
        Durability::Stable => b += 0.05,
        Durability::Episodic => {}
        Durability::Volatile => b -= 0.05,
    }
    // Small type prior: the reasoning canon (why / decided / what-bit-us)
    // outranks scratch on near-ties. A ranking preference only — type never
    // touches trust itself, durability is the decay knob.
    match node.node_type {
        NodeType::Principle | NodeType::Caution => b += 0.05,
        NodeType::Decision | NodeType::Insight => b += 0.04,
        _ => {}
    }
    b += 0.15 * node.trust;
    b
}

pub(crate) fn excerpt(node: &Node) -> String {
    let text = node.body.as_deref().unwrap_or(&node.title);
    text.chars().take(160).collect()
}

/// Canonical tag form: kebab-cased lowercase, deduped, empties dropped — so
/// "Phase 1" and "phase-1" are one tag and the pane's dropdown stays clean.
pub fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let tag = t
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-");
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}
