//! The TepinDB [`Store`] driver (PLAN §7C step 5) — Engram's graph on the
//! sibling project's primitives tier: one `.tepin` file holds the documents
//! (`nodes` / `edges` / `suspects` / `audit` / `meta` collections), the BM25
//! keyword index over the same fields the SQLite FTS mirror covered, and
//! manual-mode vectors (one vector per node, Engram's own embedder — tepin's
//! bundled model stays off via `default-features = false`).
//!
//! Where SQLite answered with SQL, this driver answers with `find` + plain
//! Rust; the shared composites (hybrid fusion, traversal, decay filtering)
//! come from the trait's provided methods, so ranking behavior is identical
//! across backends by construction.

use std::path::Path;

use serde_json::{Value, json};
use tepin_core::ServeMode;
use tepindb::{BatchOp, Db};

use crate::rag::DEFAULT_EMBED_MODEL;
use crate::store::{SNIPPET_CLOSE, SNIPPET_OPEN, Store, normalize_tags, now};
use crate::types::*;
use crate::{Error, Result};

const NODES: &str = "nodes";
const EDGES: &str = "edges";
const SUSPECTS: &str = "suspects";
const AUDIT: &str = "audit";
const META: &str = "meta";

/// The keyword/vector text fields — the same composition the SQLite FTS
/// mirror and `EMBED_COMPOSITION` v2 index.
const NODE_FIELDS: [&str; 4] = ["title", "body", "tags", "code_refs"];

/// Whether a graph path belongs to this driver (`.tepin` by convention;
/// the migration writes `graph.tepin` next to the old `graph.db`).
pub fn is_tepin_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "tepin")
}

/// One repo's graph in a single `.tepin` file. Since tepin 0.4 the model
/// swap is `reset_embedder` — a metadata operation — so the handle never
/// needs replacing and the struct is just the `Db`.
pub struct TepinStore {
    db: Db,
}

impl TepinStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        // Retry rides out cold-start races (two sessions opening at once);
        // Host makes this handle serve reads to other processes while it
        // holds the lock — `npx tepindb inspect` on a live store works
        // through the sidecar instead of dying on `database_locked`.
        let db = Db::options()
            .retry_for(std::time::Duration::from_secs(3))
            .serve(ServeMode::Host)
            .open(path.as_ref())?;
        let store = Self { db };
        configure(store.db())?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            db: Db::open_in_memory()?,
        };
        configure(store.db())?;
        Ok(store)
    }

    fn db(&self) -> &Db {
        &self.db
    }

    fn get_doc(&self, collection: &str, id: &str) -> Result<Option<Value>> {
        no_collection_is_empty(self.db().get(collection, id))
    }

    fn find_docs(&self, collection: &str, filter: &Value) -> Result<Vec<Value>> {
        no_collection_is_empty(self.db().find(collection, filter))
    }

    fn get_meta(&self, key: &str) -> Result<Option<Value>> {
        self.get_doc(META, key)
    }

    fn set_meta(&self, key: &str, mut doc: Value) -> Result<()> {
        doc["_id"] = json!(key);
        self.db().upsert(META, doc)?;
        Ok(())
    }

    fn write_node(&self, node: &Node, _exists: bool) -> Result<()> {
        self.db().upsert(NODES, node_doc(node)?)?;
        Ok(())
    }

    fn edges_matching(&self, field: &str, id: &str) -> Result<Vec<Edge>> {
        self.find_docs(EDGES, &json!({ field: id }))?
            .into_iter()
            .map(doc_edge)
            .collect()
    }

    fn suspects_matching(&self, filter: Value) -> Result<Vec<Suspect>> {
        self.find_docs(SUSPECTS, &filter)?
            .into_iter()
            .map(doc_suspect)
            .collect()
    }

    /// The `model_id` stamped on stored vectors — the recorded identity, or
    /// the default model for stores that predate model selection.
    fn vector_model_id(&self) -> Result<String> {
        Ok(Store::embed_model(self)?
            .map(|m| m.name)
            .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_string()))
    }
}

/// Idempotent per-open setup: manual-vector mode + keyword fields on `nodes`,
/// endpoint indexes on `edges`, self-describing purposes. Guarded by what
/// `collections()` already reports — reconfiguring rebuilds indexes, so an
/// already-configured file must pay nothing.
fn configure(db: &Db) -> Result<()> {
    let infos = db.collections()?;
    let info = |name: &str| infos.iter().find(|c| c.name == name);

    let nodes_ok = info(NODES).is_some_and(|c| c.manual_vectors && c.embed == NODE_FIELDS);
    if !nodes_ok {
        db.set_manual_vectors(NODES, &NODE_FIELDS)?;
    }
    for field in ["from_id", "to_id"] {
        if !info(EDGES).is_some_and(|c| c.indexes.iter().any(|i| i == field)) {
            db.create_index(EDGES, field)?;
        }
    }
    let purposes = [
        (
            NODES,
            "Engram memory nodes — typed reasoning/decision knowledge; one doc per node, one vector per node (manual mode, Engram's embedder)",
        ),
        (
            EDGES,
            "Sentence-shaped links between nodes (because/answers/replaces/conflicts-with/…); indexed by from_id and to_id",
        ),
        (
            SUSPECTS,
            "Suspected-conflict queue: unlinked look-alike node pairs awaiting a judgment",
        ),
        (
            AUDIT,
            "Append-only mutation journal; _id is the zero-padded seq",
        ),
        (
            META,
            "Store-level facts: embed_version, embed_model, audit_seq",
        ),
    ];
    for (name, purpose) in purposes {
        if info(name).is_none_or(|c| c.purpose.is_none()) {
            db.set_purpose(name, purpose)?;
        }
    }
    Ok(())
}

impl Store for TepinStore {
    // ---- store-level metadata -------------------------------------------

    fn embed_version(&self) -> Result<i64> {
        Ok(self
            .get_meta("embed_version")?
            .and_then(|d| d["value"].as_i64())
            .unwrap_or(0))
    }

    fn set_embed_version(&self, v: i64) -> Result<()> {
        self.set_meta("embed_version", json!({ "value": v }))
    }

    fn embed_model(&self) -> Result<Option<EmbedModelId>> {
        match self.get_meta("embed_model")? {
            Some(doc) => Ok(Some(EmbedModelId {
                name: doc["name"].as_str().unwrap_or_default().to_string(),
                dim: doc["dim"].as_u64().unwrap_or(0) as usize,
            })),
            None => Ok(None),
        }
    }

    fn set_embed_model(&self, model: &EmbedModelId) -> Result<()> {
        self.set_meta(
            "embed_model",
            json!({ "name": model.name, "dim": model.dim }),
        )
    }

    fn reset_vectors(&self, _dim: usize) -> Result<()> {
        // tepin 0.4's reset_embedder (an Engram dossier ask): clears the
        // per-file model pin and every stored vector as a metadata operation
        // — documents and the keyword index untouched, no file rebuild. The
        // caller re-embeds immediately after, which re-pins the new model.
        self.db().reset_embedder()?;
        Ok(())
    }

    fn stats(&self) -> Result<StoreStats> {
        let db = self.db();
        let infos = db.collections()?;
        let count = |name: &str| {
            infos
                .iter()
                .find(|c| c.name == name)
                .map(|c| c.count as i64)
                .unwrap_or(0)
        };
        let mut embedded = 0i64;
        for doc in no_collection_is_empty(db.find(NODES, &json!({})))? {
            if let Some(id) = doc["_id"].as_str()
                && !db.get_vectors(NODES, id)?.is_empty()
            {
                embedded += 1;
            }
        }
        Ok(StoreStats {
            backend: "tepindb",
            nodes: count(NODES),
            edges: count(EDGES),
            embedded,
        })
    }

    fn health(&self) -> Result<StoreHealth> {
        // redb validates its checksummed B-tree on open and fsyncs every
        // commit; a corrupt file would have failed Db::open.
        Ok(StoreHealth {
            journal_mode: None,
            integrity_ok: true,
            detail: None,
        })
    }

    // ---- nodes -----------------------------------------------------------

    fn add_node(&self, n: NewNode) -> Result<Node> {
        let id = crate::id::new_id();
        // Same clamp as the SQLite backend: provided dates for historical
        // material, never a future stamp.
        let created = n.created_at.map(|t| t.min(now())).unwrap_or_else(now);
        let node = Node {
            id: id.clone(),
            node_type: n.node_type,
            title: crate::redact::scrub(&n.title),
            body: n.body.as_deref().map(crate::redact::scrub),
            durability: n.durability,
            source: n.source,
            session_id: n.session_id,
            created_at: created,
            valid_from: Some(created),
            valid_until: None,
            status: n.status,
            last_seen: None,
            confirmed_at: None,
            // User-authored knowledge is approved by construction.
            approved_at: (n.source == Source::User).then_some(created),
            demoted_at: None,
            trust_override: None,
            trust: 0.0,
            stale: false,
            code_refs: n.code_refs,
            tags: normalize_tags(&n.tags),
        };
        self.write_node(&node, false)?;
        self.get_node(&id)?.ok_or(Error::NotFound(id))
    }

    fn get_node(&self, id: &str) -> Result<Option<Node>> {
        self.get_doc(NODES, id)?.map(doc_node).transpose()
    }

    fn update_node(&self, id: &str, p: NodePatch) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        if let Some(v) = p.node_type {
            node.node_type = v;
        }
        if let Some(v) = p.title {
            node.title = crate::redact::scrub(&v);
        }
        if let Some(v) = p.body {
            node.body = Some(crate::redact::scrub(&v));
        }
        if let Some(v) = p.durability {
            node.durability = v;
        }
        if let Some(v) = p.status {
            node.status = Some(v);
        }
        if let Some(v) = p.valid_until {
            node.valid_until = Some(v);
        }
        if let Some(v) = p.code_refs {
            node.code_refs = v;
        }
        if let Some(v) = p.tags {
            node.tags = normalize_tags(&v);
        }
        // A deliberate update is re-validation: it confirms the node (the
        // unapproved trust anchor) and clears any evidence demotion.
        let ts = now();
        node.last_seen = Some(ts);
        node.confirmed_at = Some(ts);
        node.demoted_at = None;
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn approve(&self, id: &str) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        let ts = now();
        node.approved_at = Some(ts);
        node.last_seen = Some(ts);
        node.confirmed_at = Some(ts);
        node.demoted_at = None;
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn revoke_approval(&self, id: &str) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        node.approved_at = None;
        node.trust_override = None;
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn set_trust_override(&self, id: &str, value: Option<f64>) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        node.trust_override = value.map(|v| v.clamp(0.0, 1.0));
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn demote(&self, id: &str, ts: i64) -> Result<bool> {
        let Some(mut node) = self.get_node(id)? else {
            return Ok(false);
        };
        if node.demoted_at.is_some() || node.trust_override.is_some() {
            return Ok(false);
        }
        node.demoted_at = Some(ts);
        self.write_node(&node, true)?;
        Ok(true)
    }

    fn clear_demotion(&self, id: &str) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        node.demoted_at = None;
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn delete_node(&self, id: &str) -> Result<bool> {
        let db = self.db();
        if no_collection_is_empty(db.get(NODES, id))?.is_none() {
            return Ok(false);
        }
        // Cascade edges and suspects in the same atomic batch (SQLite did
        // this in one transaction). Deleting the doc drops its vectors and
        // index entries inside tepin.
        let mut ops = vec![BatchOp::Delete {
            collection: NODES.into(),
            id: id.into(),
        }];
        let mut edge_ids: Vec<String> = Vec::new();
        for field in ["from_id", "to_id"] {
            for doc in no_collection_is_empty(db.find(EDGES, &json!({ field: id })))? {
                if let Some(eid) = doc["_id"].as_str()
                    && !edge_ids.iter().any(|e| e == eid)
                {
                    edge_ids.push(eid.to_string());
                }
            }
        }
        ops.extend(edge_ids.into_iter().map(|eid| BatchOp::Delete {
            collection: EDGES.into(),
            id: eid,
        }));
        for field in ["a_id", "b_id"] {
            for doc in no_collection_is_empty(db.find(SUSPECTS, &json!({ field: id })))? {
                if let Some(sid) = doc["_id"].as_str() {
                    ops.push(BatchOp::Delete {
                        collection: SUSPECTS.into(),
                        id: sid.to_string(),
                    });
                }
            }
        }
        db.batch(ops)?;
        Ok(true)
    }

    fn upsert_node(&self, n: &Node) -> Result<()> {
        // Still re-runs redaction (defense in depth), like the SQLite upsert.
        let mut node = n.clone();
        node.title = crate::redact::scrub(&node.title);
        node.body = node.body.as_deref().map(crate::redact::scrub);
        node.tags = normalize_tags(&node.tags);
        let exists = self.get_doc(NODES, &node.id)?.is_some();
        self.write_node(&node, exists)
    }

    fn all_nodes(&self) -> Result<Vec<Node>> {
        let mut out: Vec<Node> = self
            .find_docs(NODES, &json!({}))?
            .into_iter()
            .map(doc_node)
            .collect::<Result<_>>()?;
        out.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(out)
    }

    fn touch(&self, ids: &[String]) -> Result<()> {
        let ts = now();
        for id in ids {
            if let Some(mut node) = self.get_node(id)? {
                node.last_seen = Some(ts);
                self.write_node(&node, true)?;
            }
        }
        Ok(())
    }

    fn backdate_node(&self, id: &str, created_at: i64) -> Result<()> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        node.created_at = created_at;
        node.valid_from = Some(created_at);
        self.write_node(&node, true)
    }

    // ---- edges -----------------------------------------------------------

    fn add_edge(&self, e: NewEdge) -> Result<Edge> {
        // SQLite enforced the edges→nodes FK; here the driver does.
        for endpoint in [&e.from_id, &e.to_id] {
            if self.get_doc(NODES, endpoint)?.is_none() {
                return Err(Error::NotFound(endpoint.clone()));
            }
        }
        let id = crate::id::new_id();
        let created = now();
        let edge = Edge {
            id: id.clone(),
            edge_type: e.edge_type,
            from_id: e.from_id,
            to_id: e.to_id,
            source: e.source,
            created_at: created,
            confidence: e.confidence,
            strength: e.strength,
            note: e.note,
            valid_from: Some(created),
            valid_until: None,
            status: e.status,
        };
        self.db().insert(EDGES, edge_doc(&edge)?)?;
        self.get_edge(&id)?.ok_or(Error::NotFound(id))
    }

    fn get_edge(&self, id: &str) -> Result<Option<Edge>> {
        self.get_doc(EDGES, id)?.map(doc_edge).transpose()
    }

    fn update_edge(&self, id: &str, p: EdgePatch) -> Result<Edge> {
        let mut edge = self
            .get_edge(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        if let Some(v) = p.edge_type {
            edge.edge_type = v;
        }
        if let Some(v) = p.status {
            edge.status = Some(v);
        }
        if let Some(v) = p.note {
            edge.note = Some(v);
        }
        if let Some(v) = p.confidence {
            edge.confidence = Some(v);
        }
        if let Some(v) = p.strength {
            edge.strength = Some(v);
        }
        self.db().update(EDGES, id, edge_doc(&edge)?)?;
        self.get_edge(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn delete_edge(&self, id: &str) -> Result<bool> {
        let db = self.db();
        if no_collection_is_empty(db.get(EDGES, id))?.is_none() {
            return Ok(false);
        }
        db.delete(EDGES, id)?;
        Ok(true)
    }

    fn upsert_edge(&self, e: &Edge) -> Result<()> {
        self.db().upsert(EDGES, edge_doc(e)?)?;
        Ok(())
    }

    fn edges_out(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.edges_matching("from_id", node_id)
    }

    fn edges_in(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.edges_matching("to_id", node_id)
    }

    fn all_edges(&self) -> Result<Vec<Edge>> {
        self.find_docs(EDGES, &json!({}))?
            .into_iter()
            .map(doc_edge)
            .collect()
    }

    // ---- bulk ------------------------------------------------------------

    fn import_raw(&self, nodes: &[Node], edges: &[Edge]) -> Result<()> {
        // One atomic multi-collection batch of native upserts (tepin 0.4),
        // nodes before edges — no per-document existence pre-pass.
        let db = self.db();
        let mut ops: Vec<BatchOp> = Vec::with_capacity(nodes.len() + edges.len());
        for n in nodes {
            let mut node = n.clone();
            node.title = crate::redact::scrub(&node.title);
            node.body = node.body.as_deref().map(crate::redact::scrub);
            node.tags = normalize_tags(&node.tags);
            ops.push(BatchOp::Upsert {
                collection: NODES.into(),
                doc: node_doc(&node)?,
            });
        }
        for e in edges {
            ops.push(BatchOp::Upsert {
                collection: EDGES.into(),
                doc: edge_doc(e)?,
            });
        }
        if !ops.is_empty() {
            db.batch(ops)?;
        }
        Ok(())
    }

    fn archive_nodes(&self, ids: &[String], ts: i64) -> Result<()> {
        let db = self.db();
        let mut ops: Vec<BatchOp> = Vec::new();
        for id in ids {
            if let Some(node) = self.get_node(id)?
                && node.valid_until.is_none()
            {
                let mut node = node;
                node.valid_until = Some(ts);
                ops.push(BatchOp::Update {
                    collection: NODES.into(),
                    id: id.clone(),
                    doc: node_doc(&node)?,
                });
            }
        }
        if !ops.is_empty() {
            db.batch(ops)?;
        }
        Ok(())
    }

    // ---- search primitives ----------------------------------------------

    fn search_fts(&self, query: &str, types: &[NodeType], limit: usize) -> Result<Vec<SearchHit>> {
        let terms = tokenize(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        // Over-fetch: archived/off-type hits fall out below.
        let raw = self
            .db()
            .keyword_search(Some(NODES), query, limit * 4 + 16)?;
        let mut out = Vec::new();
        for hit in raw {
            let Some(node) = self.get_node(&hit.id)? else {
                continue;
            };
            if node.valid_until.is_some() {
                continue;
            }
            if !types.is_empty() && !types.contains(&node.node_type) {
                continue;
            }
            out.push(SearchHit {
                id: node.id.clone(),
                node_type: node.node_type,
                title: node.title.clone(),
                snippet: make_snippet(&node, &terms),
                score: hit.score as f64,
                durability: node.durability,
                status: node.status,
                trust: node.trust,
                stale: node.stale,
                neighbors: Vec::new(),
                project: None,
            });
            if out.len() == limit {
                break;
            }
        }
        Ok(out)
    }

    fn search_vec(&self, query: &[f32], k: usize) -> Result<Vec<(String, f64)>> {
        let hits = self.db().search_by_vector(Some(NODES), query, k)?;
        Ok(hits
            .into_iter()
            .map(|h| (h.id, (1.0 - h.score as f64).clamp(0.0, 2.0)))
            .collect())
    }

    fn upsert_embeddings(&self, node_id: &str, vectors: &[Vec<f32>]) -> Result<()> {
        // TepinDB's chunk model natively: chunk 0 = the node-level vector,
        // chunks 1..N the claims; search_by_vector already scores per-doc
        // best-chunk, so claim-level recall needs nothing else here.
        let model_id = self.vector_model_id()?;
        self.db().set_vectors(NODES, node_id, &model_id, vectors)?;
        Ok(())
    }

    fn embedding_of(&self, node_id: &str) -> Result<Option<Vec<f32>>> {
        Ok(self.db().get_vectors(NODES, node_id)?.into_iter().next())
    }

    // ---- suspects --------------------------------------------------------

    fn suspect_between(&self, a: &str, b: &str) -> Result<bool> {
        Ok(!self
            .suspects_matching(json!({ "a_id": a, "b_id": b }))?
            .is_empty()
            || !self
                .suspects_matching(json!({ "a_id": b, "b_id": a }))?
                .is_empty())
    }

    fn add_suspect(
        &self,
        a_id: &str,
        b_id: &str,
        similarity: f64,
        hint: Option<(&str, f64)>,
    ) -> Result<Suspect> {
        let id = crate::id::new_id();
        let suspect = Suspect {
            id: id.clone(),
            a_id: a_id.to_string(),
            b_id: b_id.to_string(),
            similarity,
            created_at: now(),
            status: SuspectStatus::Suspected,
            nli_label: hint.map(|(l, _)| l.to_string()),
            nli_score: hint.map(|(_, s)| s),
        };
        let mut doc = serde_json::to_value(&suspect)?;
        doc["_id"] = json!(id);
        self.db().insert(SUSPECTS, doc)?;
        self.get_suspect(&id)?.ok_or(Error::NotFound(id))
    }

    fn get_suspect(&self, id: &str) -> Result<Option<Suspect>> {
        self.get_doc(SUSPECTS, id)?.map(doc_suspect).transpose()
    }

    fn set_suspect_status(&self, id: &str, status: SuspectStatus) -> Result<Suspect> {
        let mut suspect = self
            .get_suspect(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        suspect.status = status;
        let mut doc = serde_json::to_value(&suspect)?;
        doc["_id"] = json!(id);
        self.db().update(SUSPECTS, id, doc)?;
        self.get_suspect(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn suspects_pending(&self) -> Result<Vec<SuspectView>> {
        let mut pending =
            self.suspects_matching(json!({ "status": SuspectStatus::Suspected.as_str() }))?;
        pending.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        let mut out = Vec::new();
        for s in pending {
            let (Some(a), Some(b)) = (self.get_node(&s.a_id)?, self.get_node(&s.b_id)?) else {
                continue;
            };
            // Pairs with an archived endpoint drop out — superseding one
            // side settles the question.
            if a.valid_until.is_some() || b.valid_until.is_some() {
                continue;
            }
            out.push(SuspectView {
                id: s.id,
                similarity: s.similarity,
                created_at: s.created_at,
                nli_label: s.nli_label,
                nli_score: s.nli_score,
                a: SuspectEndpoint {
                    id: a.id,
                    node_type: a.node_type,
                    title: a.title,
                },
                b: SuspectEndpoint {
                    id: b.id,
                    node_type: b.node_type,
                    title: b.title,
                },
            });
        }
        Ok(out)
    }

    fn all_suspects(&self) -> Result<Vec<Suspect>> {
        let mut out = self.suspects_matching(json!({}))?;
        out.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(out)
    }

    fn upsert_suspect(&self, s: &Suspect) -> Result<()> {
        let mut doc = serde_json::to_value(s)?;
        doc["_id"] = json!(s.id);
        self.db().upsert(SUSPECTS, doc)?;
        Ok(())
    }

    // ---- audit journal ---------------------------------------------------

    fn add_audit(&self, e: &AuditEntry) -> Result<()> {
        let db = self.db();
        let seq = self
            .get_meta("audit_seq")?
            .and_then(|d| d["value"].as_i64())
            .unwrap_or(0)
            + 1;
        let counter = match self.get_meta("audit_seq")?.is_some() {
            true => BatchOp::Update {
                collection: META.into(),
                id: "audit_seq".into(),
                doc: json!({ "_id": "audit_seq", "value": seq }),
            },
            false => BatchOp::Insert {
                collection: META.into(),
                doc: json!({ "_id": "audit_seq", "value": seq }),
            },
        };
        let mut doc = serde_json::to_value(e)?;
        doc["seq"] = json!(seq);
        // Zero-padded seq as _id keeps journal rows naturally sorted.
        doc["_id"] = json!(format!("{seq:012}"));
        db.batch(vec![
            counter,
            BatchOp::Insert {
                collection: AUDIT.into(),
                doc,
            },
        ])?;
        Ok(())
    }

    fn audit_page(
        &self,
        before: Option<i64>,
        entity_id: Option<&str>,
        limit: usize,
    ) -> Result<AuditPage> {
        let filter = match entity_id {
            Some(eid) => json!({ "entity_id": eid }),
            None => json!({}),
        };
        let mut entries: Vec<AuditEntry> = self
            .find_docs(AUDIT, &filter)?
            .into_iter()
            .map(|doc| Ok(serde_json::from_value(doc)?))
            .collect::<Result<_>>()?;
        let total = entries.len() as i64;
        entries.sort_by_key(|e| std::cmp::Reverse(e.seq));
        if let Some(before) = before {
            entries.retain(|e| e.seq < before);
        }
        entries.truncate(limit);
        Ok(AuditPage { entries, total })
    }

    // ---- tags ------------------------------------------------------------

    fn tag_stats(&self, limit: usize) -> Result<Vec<TagStat>> {
        use std::collections::HashMap;
        let mut stats: HashMap<String, (i64, i64)> = HashMap::new();
        for node in self.all_nodes()? {
            if node.valid_until.is_some() {
                continue;
            }
            let freshness = node.last_seen.unwrap_or(node.created_at);
            for tag in &node.tags {
                let entry = stats.entry(tag.clone()).or_insert((0, 0));
                entry.0 += 1;
                entry.1 = entry.1.max(freshness);
            }
        }
        let mut out: Vec<TagStat> = stats
            .into_iter()
            .map(|(tag, (count, last_used))| TagStat {
                tag,
                count,
                last_used,
            })
            .collect();
        out.sort_by_key(|t| std::cmp::Reverse((t.last_used, t.count)));
        out.truncate(limit);
        Ok(out)
    }
}

/// TepinDB creates collections lazily on first insert, so a purposed-but-
/// empty collection errors `collection_not_found` on reads — for this driver
/// that simply means "no documents yet".
fn no_collection_is_empty<T: Default>(r: tepindb::Result<T>) -> Result<T> {
    match r {
        Err(e) if e.code == "collection_not_found" => Ok(T::default()),
        other => Ok(other?),
    }
}

// ---- document mapping ----------------------------------------------------

fn node_doc(n: &Node) -> Result<Value> {
    let mut doc = serde_json::to_value(n)?;
    let obj = doc.as_object_mut().expect("node serializes to an object");
    // Computed at read time, never stored.
    obj.remove("trust");
    obj.remove("stale");
    obj.insert("_id".into(), json!(n.id));
    Ok(doc)
}

fn doc_node(doc: Value) -> Result<Node> {
    let mut n: Node = serde_json::from_value(doc)?;
    n.trust = crate::policy::trust(&n.trust_inputs(), now());
    n.stale = crate::policy::is_stale(n.trust);
    Ok(n)
}

fn edge_doc(e: &Edge) -> Result<Value> {
    let mut doc = serde_json::to_value(e)?;
    doc["_id"] = json!(e.id);
    Ok(doc)
}

fn doc_edge(doc: Value) -> Result<Edge> {
    Ok(serde_json::from_value(doc)?)
}

fn doc_suspect(doc: Value) -> Result<Suspect> {
    Ok(serde_json::from_value(doc)?)
}

// ---- keyword snippets ----------------------------------------------------

/// TepinDB's BM25 tokenization, mirrored: lowercase, split on
/// non-alphanumeric, drop tokens shorter than 2 chars.
fn tokenize(q: &str) -> Vec<String> {
    q.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect()
}

/// FTS5's `snippet()` replacement: pick the field with the most matched
/// terms, clip a ~12-word window around the first match, and mark matching
/// words with the sentinel pair the pane/MCP already understand.
fn make_snippet(node: &Node, terms: &[String]) -> String {
    let fields = [
        node.title.clone(),
        node.body.clone().unwrap_or_default(),
        node.tags.join(" "),
        node.code_refs.join(" "),
    ];
    let matches_in = |text: &str| tokenize(text).iter().filter(|t| terms.contains(t)).count();
    let best = fields
        .iter()
        .max_by_key(|f| matches_in(f))
        .filter(|f| matches_in(f) > 0);
    match best {
        Some(text) => highlight(text, terms, 12),
        None => crate::store::excerpt(node),
    }
}

fn highlight(text: &str, terms: &[String], window: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let word_matches = |w: &str| tokenize(w).iter().any(|t| terms.contains(t));
    let first = words.iter().position(|w| word_matches(w)).unwrap_or(0);
    let start = first.saturating_sub(window / 3);
    let end = (start + window).min(words.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    for (i, word) in words[start..end].iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if word_matches(word) {
            out.push(SNIPPET_OPEN);
            out.push_str(word);
            out.push(SNIPPET_CLOSE);
        } else {
            out.push_str(word);
        }
    }
    if end < words.len() {
        out.push('…');
    }
    out
}
