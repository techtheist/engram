//! The reference [`Store`] implementation: one SQLite connection bound to one
//! repo's `.engram/graph.db`. Writes are serialized by `&mut`-free design +
//! the daemon's single writer; WAL gives concurrent reads (PLAN §6B).
//! Composite reads (hybrid fusion, traversal, …) come from the trait's
//! provided methods; this file overrides only what SQL answers faster.

use std::path::Path;
use std::sync::{Arc, Once, RwLock};

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};

use crate::config::{GraphConfig, PolicyConfig};
use crate::rag::EMBED_DIM;
use crate::schema::{FTS_SCHEMA, SCHEMA};
use crate::store::{SNIPPET_CLOSE, SNIPPET_OPEN, Store, normalize_tags, now};
use crate::types::*;
use crate::{Error, Result};

/// Register sqlite-vec as an auto-extension exactly once. Auto-extensions only
/// affect connections opened *after* registration, so this must run before any
/// `Connection::open`.
fn ensure_vec_extension() {
    use rusqlite::ffi;
    type EntryPoint = unsafe extern "C" fn(
        *mut ffi::sqlite3,
        *mut *mut std::os::raw::c_char,
        *const ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;

    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        let init =
            std::mem::transmute::<*const (), EntryPoint>(sqlite_vec::sqlite3_vec_init as *const ());
        ffi::sqlite3_auto_extension(Some(init));
    });
}

pub struct SqliteStore {
    conn: Connection,
    /// The parsed per-graph configuration, cached at open and refreshed by
    /// `set_graph_config` — trust hydration reads it on every row.
    cfg: RwLock<Arc<GraphConfig>>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        ensure_vec_extension();
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        ensure_vec_extension();
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Self::migrate(&conn)?;
        Self::ensure_fts(&conn)?;
        // Vector width follows the active embedding model (meta `vec_dim`,
        // stamped by reset_vectors on a model swap); EMBED_DIM covers every
        // store that predates model selection.
        let dim: usize = meta_get(&conn, "vec_dim")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(EMBED_DIM);
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_nodes USING vec0(
               node_id TEXT PRIMARY KEY,
               embedding float[{dim}] distance_metric=cosine
             );"
        ))?;
        let cfg = GraphConfig::from_stored(meta_get(&conn, "graph_config")?.as_deref());
        let store = Self {
            conn,
            cfg: RwLock::new(Arc::new(cfg)),
        };
        store.shorten_legacy_ids()?;
        Ok(store)
    }

    /// The live policy numbers, cloned out of the cached config for row
    /// hydration closures.
    fn policy(&self) -> PolicyConfig {
        self.cfg.read().unwrap().policy.clone()
    }

    /// Forward-only migrations for databases created before a column existed.
    fn migrate(conn: &Connection) -> Result<()> {
        // Computed-trust model (v0.1.15): last_confirmed becomes last_seen,
        // approvals get their own timestamp. The old stored-confidence column
        // stays in legacy DBs (harmless, unread) rather than risking a
        // DROP COLUMN on user data.
        if column_exists(conn, "nodes", "last_confirmed")? {
            conn.execute_batch("ALTER TABLE nodes RENAME COLUMN last_confirmed TO last_seen;")?;
        }
        if !column_exists(conn, "nodes", "last_seen")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN last_seen INTEGER;")?;
        }
        if !column_exists(conn, "nodes", "approved_at")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN approved_at INTEGER;")?;
            // Data migration: user-authored nodes and heavily-reconfirmed
            // Claude nodes were trusted under the stored-confidence model —
            // carry that over as an approval so their trust doesn't collapse.
            if column_exists(conn, "nodes", "confidence")? {
                conn.execute_batch(
                    "UPDATE nodes SET approved_at = COALESCE(last_seen, created_at)
                     WHERE source = 'user' OR COALESCE(confidence, 0) >= 0.9;",
                )?;
            }
        }
        if !column_exists(conn, "nodes", "tags")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN tags TEXT;")?;
        }
        // Trust v2 (v0.4.2): retrieval stops certifying its own outputs.
        // `confirmed_at` becomes the unapproved trust anchor (deliberate acts
        // only); `last_seen` stays as observability. Backfilled from last_seen
        // so no node loses trust at the moment of upgrade — the semantics
        // change only going forward. Volatile nodes get a fresh anchor
        // instead: v2 also shrinks their window 183d → 30d, and an old anchor
        // under the new window would back-date their stale crossing past the
        // decay TTL — silently archiving healthy notes on the next sweep.
        if !column_exists(conn, "nodes", "confirmed_at")? {
            conn.execute_batch(
                "ALTER TABLE nodes ADD COLUMN confirmed_at INTEGER;
                 UPDATE nodes SET confirmed_at = last_seen;",
            )?;
            conn.execute(
                "UPDATE nodes SET confirmed_at = ?1 \
                 WHERE durability = 'volatile' AND valid_until IS NULL",
                params![now()],
            )?;
        }
        if !column_exists(conn, "nodes", "demoted_at")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN demoted_at INTEGER;")?;
        }
        if !column_exists(conn, "nodes", "trust_override")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN trust_override REAL;")?;
        }
        // Version tracking (0.7.0): the project version a node was captured
        // at, stamped from the graph's current working version.
        if !column_exists(conn, "nodes", "version")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN version TEXT;")?;
        }
        // Local cortex (v0.5.0): suspects carry an optional NLI hint.
        if !column_exists(conn, "suspects", "nli_label")? {
            conn.execute_batch(
                "ALTER TABLE suspects ADD COLUMN nli_label TEXT;
                 ALTER TABLE suspects ADD COLUMN nli_score REAL;",
            )?;
        }
        // Directional conflict hints (0.7.0): which side carries the negation.
        if !column_exists(conn, "suspects", "nli_direction")? {
            conn.execute_batch("ALTER TABLE suspects ADD COLUMN nli_direction TEXT;")?;
        }
        Ok(())
    }

    /// Keep `nodes_fts` in lockstep with [`FTS_SCHEMA`]: when a database's FTS
    /// mirror predates a column (tags, code_refs), drop it — triggers included
    /// — and recreate + rebuild from the content table. Fresh databases just
    /// create.
    fn ensure_fts(conn: &Connection) -> Result<()> {
        let fts_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='nodes_fts'",
            [],
            |r| r.get(0),
        )?;
        let outdated = fts_exists > 0
            && (!column_exists(conn, "nodes_fts", "tags")?
                || !column_exists(conn, "nodes_fts", "code_refs")?);
        if outdated {
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS nodes_ai;
                 DROP TRIGGER IF EXISTS nodes_ad;
                 DROP TRIGGER IF EXISTS nodes_au;
                 DROP TABLE nodes_fts;",
            )?;
        }
        conn.execute_batch(FTS_SCHEMA)?;
        if outdated {
            conn.execute_batch("INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');")?;
        }
        Ok(())
    }

    /// One-time rewrite of legacy UUID ids to short ids (`id::new_id`),
    /// cascading through edge endpoints, edge ids, and stored embeddings.
    /// Idempotent — short ids are left alone. Runs on every open; a fully
    /// migrated database matches nothing and pays one cheap SELECT.
    pub fn shorten_legacy_ids(&self) -> Result<()> {
        let legacy_nodes: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM nodes WHERE length(id) > 12")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let legacy_edges: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM edges WHERE length(id) > 12")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if legacy_nodes.is_empty() && legacy_edges.is_empty() {
            return Ok(());
        }

        // Endpoint rewrites would trip the edges→nodes FK mid-flight; defer
        // enforcement to commit (transaction-scoped, resets automatically).
        self.conn.execute_batch("PRAGMA defer_foreign_keys=ON;")?;
        let tx = self.conn.unchecked_transaction()?;
        for old in &legacy_nodes {
            let new = crate::id::new_id();
            tx.execute("UPDATE nodes SET id=?1 WHERE id=?2", params![new, old])?;
            tx.execute(
                "UPDATE edges SET from_id=?1 WHERE from_id=?2",
                params![new, old],
            )?;
            tx.execute(
                "UPDATE edges SET to_id=?1 WHERE to_id=?2",
                params![new, old],
            )?;
            // vec0 doesn't support PK updates: move the stored vector instead.
            let embedding: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT embedding FROM vec_nodes WHERE node_id=?1",
                    [old],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(blob) = embedding {
                tx.execute("DELETE FROM vec_nodes WHERE node_id=?1", [old])?;
                tx.execute(
                    "INSERT INTO vec_nodes(node_id, embedding) VALUES (?1, ?2)",
                    params![new, blob],
                )?;
            }
        }
        for old in &legacy_edges {
            tx.execute(
                "UPDATE edges SET id=?1 WHERE id=?2",
                params![crate::id::new_id(), old],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Escape hatch for backend-specific maintenance (kept off the trait so
    /// nothing above the storage boundary can reach raw SQLite).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    fn edges_where(&self, col: &str, node_id: &str) -> Result<Vec<Edge>> {
        let base = EDGE_SELECT.rsplit_once(" WHERE ").map(|(s, _)| s).unwrap();
        let sql = format!("{base} WHERE {col}=?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([node_id], row_to_edge)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

impl Store for SqliteStore {
    // ---- store-level metadata -------------------------------------------

    fn embed_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    fn set_embed_version(&self, v: i64) -> Result<()> {
        self.conn
            .execute_batch(&format!("PRAGMA user_version = {v};"))?;
        Ok(())
    }

    fn embed_model(&self) -> Result<Option<EmbedModelId>> {
        match meta_get(&self.conn, "embed_model")? {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    fn graph_config(&self) -> Result<Option<String>> {
        meta_get(&self.conn, "graph_config")
    }

    fn set_graph_config(&self, json: &str) -> Result<()> {
        meta_set(&self.conn, "graph_config", json)?;
        *self.cfg.write().unwrap() = Arc::new(GraphConfig::from_stored(Some(json)));
        Ok(())
    }

    fn config(&self) -> Arc<GraphConfig> {
        self.cfg.read().unwrap().clone()
    }

    fn current_version(&self) -> Result<Option<String>> {
        meta_get(&self.conn, "current_version")
    }

    fn set_current_version(&self, version: Option<&str>) -> Result<()> {
        match version {
            Some(v) => meta_set(&self.conn, "current_version", v),
            None => {
                self.conn
                    .execute("DELETE FROM meta WHERE key='current_version'", [])?;
                Ok(())
            }
        }
    }

    fn set_embed_model(&self, model: &EmbedModelId) -> Result<()> {
        meta_set(&self.conn, "embed_model", &serde_json::to_string(model)?)
    }

    fn reset_vectors(&self, dim: usize) -> Result<()> {
        self.conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS vec_nodes;
             CREATE VIRTUAL TABLE vec_nodes USING vec0(
               node_id TEXT PRIMARY KEY,
               embedding float[{dim}] distance_metric=cosine
             );"
        ))?;
        meta_set(&self.conn, "vec_dim", &dim.to_string())
    }

    fn stats(&self) -> Result<StoreStats> {
        let count = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok(StoreStats {
            backend: "sqlite",
            nodes: count("SELECT count(*) FROM nodes")?,
            edges: count("SELECT count(*) FROM edges")?,
            embedded: count("SELECT count(*) FROM vec_nodes")?,
        })
    }

    fn health(&self) -> Result<StoreHealth> {
        let journal_mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let check: String = self
            .conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        let integrity_ok = check == "ok";
        Ok(StoreHealth {
            journal_mode: Some(journal_mode),
            integrity_ok,
            detail: (!integrity_ok).then_some(check),
        })
    }

    // ---- nodes -----------------------------------------------------------

    fn add_node(&self, n: NewNode) -> Result<Node> {
        let id = crate::id::new_id();
        // Historical material carries its own date (digestion); the future
        // is clamped away so nothing can buy itself a recency boost.
        let created = n.created_at.map(|t| t.min(now())).unwrap_or_else(now);
        let title = crate::redact::scrub(&n.title);
        let body = n.body.as_deref().map(crate::redact::scrub);
        let code_refs = serde_json::to_string(&n.code_refs)?;
        let tags = serde_json::to_string(&normalize_tags(&n.tags))?;
        self.conn.execute(
            "INSERT INTO nodes
               (id, type, title, body, durability, source, session_id,
                created_at, valid_from, valid_until, status, code_refs, tags, last_seen,
                approved_at, version)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                id,
                n.node_type.as_str(),
                title,
                body,
                n.durability.as_str(),
                n.source.as_str(),
                n.session_id,
                created,
                created,
                Option::<i64>::None,
                n.status.map(NodeStatus::as_str),
                code_refs,
                tags,
                Option::<i64>::None,
                // User-authored knowledge is approved by construction.
                (n.source == Source::User).then_some(created),
                n.version,
            ],
        )?;
        // Trust anchors at created_at until a deliberate act confirms the node.
        self.get_node(&id)?.ok_or_else(|| Error::NotFound(id))
    }

    fn get_node(&self, id: &str) -> Result<Option<Node>> {
        let policy = self.policy();
        let sql = format!("{NODE_SELECT} WHERE id=?1");
        Ok(self
            .conn
            .query_row(&sql, [id], |r| row_to_node(r, &policy))
            .optional()?)
    }

    fn update_node(&self, id: &str, p: NodePatch) -> Result<Node> {
        let mut sets: Vec<&str> = Vec::new();
        let mut vals: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(v) = p.node_type {
            sets.push("type=?");
            vals.push(Box::new(v.as_str().to_string()));
        }
        if let Some(v) = p.title {
            sets.push("title=?");
            vals.push(Box::new(crate::redact::scrub(&v)));
        }
        if let Some(v) = p.body {
            sets.push("body=?");
            vals.push(Box::new(crate::redact::scrub(&v)));
        }
        if let Some(v) = p.durability {
            sets.push("durability=?");
            vals.push(Box::new(v.as_str().to_string()));
        }
        if let Some(v) = p.status {
            sets.push("status=?");
            vals.push(Box::new(v.as_str().to_string()));
        }
        if let Some(v) = p.valid_until {
            sets.push("valid_until=?");
            vals.push(Box::new(v));
        }
        if let Some(v) = p.code_refs {
            sets.push("code_refs=?");
            vals.push(Box::new(serde_json::to_string(&v)?));
        }
        if let Some(v) = p.tags {
            sets.push("tags=?");
            vals.push(Box::new(serde_json::to_string(&normalize_tags(&v))?));
        }
        if let Some(v) = p.version {
            sets.push("version=?");
            vals.push(Box::new(v));
        }
        // A deliberate update is re-validation: it confirms the node (the
        // unapproved trust anchor) and clears any evidence demotion. This —
        // not retrieval — is what refreshes trust.
        let ts = now();
        sets.push("last_seen=?");
        vals.push(Box::new(ts));
        sets.push("confirmed_at=?");
        vals.push(Box::new(ts));
        sets.push("demoted_at=NULL");

        let sql = format!("UPDATE nodes SET {} WHERE id=?", sets.join(", "));
        vals.push(Box::new(id.to_string()));
        let bound: Vec<&dyn ToSql> = vals.iter().map(|b| b.as_ref()).collect();
        self.conn.execute(&sql, bound.as_slice())?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn approve(&self, id: &str) -> Result<Node> {
        let ts = now();
        self.conn.execute(
            "UPDATE nodes SET approved_at=?1, last_seen=?1, confirmed_at=?1, demoted_at=NULL \
             WHERE id=?2",
            params![ts, id],
        )?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn revoke_approval(&self, id: &str) -> Result<Node> {
        self.conn.execute(
            "UPDATE nodes SET approved_at=NULL, trust_override=NULL WHERE id=?1",
            [id],
        )?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn set_trust_override(&self, id: &str, value: Option<f64>) -> Result<Node> {
        self.conn.execute(
            "UPDATE nodes SET trust_override=?1 WHERE id=?2",
            params![value.map(|v| v.clamp(0.0, 1.0)), id],
        )?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn demote(&self, id: &str, ts: i64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE nodes SET demoted_at=?1 \
             WHERE id=?2 AND demoted_at IS NULL AND trust_override IS NULL",
            params![ts, id],
        )?;
        Ok(n > 0)
    }

    fn clear_demotion(&self, id: &str) -> Result<Node> {
        self.conn
            .execute("UPDATE nodes SET demoted_at=NULL WHERE id=?1", [id])?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn delete_node(&self, id: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM edges WHERE from_id=?1 OR to_id=?1", [id])?;
        tx.execute("DELETE FROM suspects WHERE a_id=?1 OR b_id=?1", [id])?;
        tx.execute(
            "DELETE FROM vec_nodes WHERE node_id = ?1 OR node_id LIKE ?1 || '#%'",
            [id],
        )?;
        let n = tx.execute("DELETE FROM nodes WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    fn list_open(&self, types: &[NodeType]) -> Result<Vec<Node>> {
        let policy = self.policy();
        let cfg = Store::config(self);
        let strs: Vec<String> = if types.is_empty() {
            cfg.worklist_types().iter().map(|s| s.to_string()).collect()
        } else {
            types.iter().map(|t| t.as_str().to_string()).collect()
        };
        let sql = format!(
            "{NODE_SELECT} WHERE valid_until IS NULL AND status='open' AND type IN ({}) ORDER BY created_at DESC",
            placeholders(strs.len())
        );
        let params: Vec<&dyn ToSql> = strs.iter().map(|s| s as &dyn ToSql).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |r| row_to_node(r, &policy))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn upsert_node(&self, n: &Node) -> Result<()> {
        // Still re-runs redaction (defense in depth). `ON CONFLICT DO UPDATE`
        // (not `INSERT OR REPLACE`) so we don't delete-and-reinsert a row that
        // edges reference.
        let title = crate::redact::scrub(&n.title);
        let body = n.body.as_deref().map(crate::redact::scrub);
        let code_refs = serde_json::to_string(&n.code_refs)?;
        let tags = serde_json::to_string(&normalize_tags(&n.tags))?;
        self.conn.execute(
            "INSERT INTO nodes
               (id, type, title, body, durability, source, session_id,
                created_at, valid_from, valid_until, status, code_refs, tags, last_seen,
                approved_at, confirmed_at, demoted_at, trust_override, version)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)
             ON CONFLICT(id) DO UPDATE SET
               type=excluded.type, title=excluded.title, body=excluded.body,
               durability=excluded.durability, source=excluded.source,
               session_id=excluded.session_id, created_at=excluded.created_at,
               valid_from=excluded.valid_from, valid_until=excluded.valid_until,
               status=excluded.status, code_refs=excluded.code_refs,
               tags=excluded.tags, last_seen=excluded.last_seen,
               approved_at=excluded.approved_at, confirmed_at=excluded.confirmed_at,
               demoted_at=excluded.demoted_at, trust_override=excluded.trust_override,
               version=excluded.version",
            params![
                n.id,
                n.node_type.as_str(),
                title,
                body,
                n.durability.as_str(),
                n.source.as_str(),
                n.session_id,
                n.created_at,
                n.valid_from,
                n.valid_until,
                n.status.map(NodeStatus::as_str),
                code_refs,
                tags,
                n.last_seen,
                n.approved_at,
                n.confirmed_at,
                n.demoted_at,
                n.trust_override,
                n.version,
            ],
        )?;
        Ok(())
    }

    fn retype_nodes(&self, from: &str, to: &str) -> Result<u64> {
        Ok(self
            .conn
            .execute("UPDATE nodes SET type=?2 WHERE type=?1", params![from, to])?
            as u64)
    }

    fn retype_edges(&self, from: &str, to: &str) -> Result<u64> {
        Ok(self
            .conn
            .execute("UPDATE edges SET type=?2 WHERE type=?1", params![from, to])?
            as u64)
    }

    fn nodes_in_active_conflicts(&self) -> Result<Vec<Node>> {
        let policy = self.policy();
        let sql = format!(
            "{NODE_SELECT} WHERE id IN (\
               SELECT from_id FROM edges WHERE type=?1 AND (status IS NULL OR status='active') \
               UNION \
               SELECT to_id FROM edges WHERE type=?1 AND (status IS NULL OR status='active')\
             ) ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([Store::config(self).contradiction_verb()], |r| {
            row_to_node(r, &policy)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn all_nodes(&self) -> Result<Vec<Node>> {
        let policy = self.policy();
        let sql = format!("{NODE_SELECT} ORDER BY created_at");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| row_to_node(r, &policy))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn touch(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let sql = format!(
            "UPDATE nodes SET last_seen=? WHERE id IN ({})",
            placeholders(ids.len())
        );
        let ts = now();
        let mut vals: Vec<&dyn ToSql> = Vec::with_capacity(ids.len() + 1);
        vals.push(&ts);
        for id in ids {
            vals.push(id);
        }
        self.conn.execute(&sql, vals.as_slice())?;
        Ok(())
    }

    fn backdate_node(&self, id: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE nodes SET created_at=?1, valid_from=?1 WHERE id=?2",
            params![created_at, id],
        )?;
        Ok(())
    }

    // ---- edges -----------------------------------------------------------

    fn add_edge(&self, e: NewEdge) -> Result<Edge> {
        let id = crate::id::new_id();
        let created = now();
        self.conn.execute(
            "INSERT INTO edges
               (id, type, from_id, to_id, source, created_at,
                confidence, strength, note, valid_from, valid_until, status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                id,
                e.edge_type.as_str(),
                e.from_id,
                e.to_id,
                e.source.as_str(),
                created,
                e.confidence,
                e.strength,
                e.note,
                created,
                Option::<i64>::None,
                e.status.map(EdgeStatus::as_str),
            ],
        )?;
        self.get_edge(&id)?.ok_or_else(|| Error::NotFound(id))
    }

    fn get_edge(&self, id: &str) -> Result<Option<Edge>> {
        Ok(self
            .conn
            .query_row(EDGE_SELECT, [id], row_to_edge)
            .optional()?)
    }

    fn update_edge(&self, id: &str, p: EdgePatch) -> Result<Edge> {
        let mut sets: Vec<&str> = Vec::new();
        let mut vals: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(v) = p.edge_type {
            sets.push("type=?");
            vals.push(Box::new(v.as_str().to_string()));
        }
        if let Some(v) = p.status {
            sets.push("status=?");
            vals.push(Box::new(v.as_str().to_string()));
        }
        if let Some(v) = p.note {
            sets.push("note=?");
            vals.push(Box::new(v));
        }
        if let Some(v) = p.confidence {
            sets.push("confidence=?");
            vals.push(Box::new(v));
        }
        if let Some(v) = p.strength {
            sets.push("strength=?");
            vals.push(Box::new(v));
        }
        if !sets.is_empty() {
            let sql = format!("UPDATE edges SET {} WHERE id=?", sets.join(", "));
            vals.push(Box::new(id.to_string()));
            let bound: Vec<&dyn ToSql> = vals.iter().map(|b| b.as_ref()).collect();
            self.conn.execute(&sql, bound.as_slice())?;
        }
        self.get_edge(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn delete_edge(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM edges WHERE id=?1", [id])? > 0)
    }

    fn upsert_edge(&self, e: &Edge) -> Result<()> {
        self.conn.execute(
            "INSERT INTO edges
               (id, type, from_id, to_id, source, created_at,
                confidence, strength, note, valid_from, valid_until, status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(id) DO UPDATE SET
               type=excluded.type, from_id=excluded.from_id, to_id=excluded.to_id,
               source=excluded.source, created_at=excluded.created_at,
               confidence=excluded.confidence, strength=excluded.strength,
               note=excluded.note, valid_from=excluded.valid_from,
               valid_until=excluded.valid_until, status=excluded.status",
            params![
                e.id,
                e.edge_type.as_str(),
                e.from_id,
                e.to_id,
                e.source.as_str(),
                e.created_at,
                e.confidence,
                e.strength,
                e.note,
                e.valid_from,
                e.valid_until,
                e.status.map(EdgeStatus::as_str),
            ],
        )?;
        Ok(())
    }

    fn edges_out(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.edges_where("from_id", node_id)
    }

    fn edges_in(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.edges_where("to_id", node_id)
    }

    fn all_edges(&self) -> Result<Vec<Edge>> {
        let base = EDGE_SELECT.rsplit_once(" WHERE ").map(|(s, _)| s).unwrap();
        let mut stmt = self.conn.prepare(base)?;
        let rows = stmt.query_map([], row_to_edge)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn active_conflict_edges(&self) -> Result<Vec<Edge>> {
        let base = EDGE_SELECT.rsplit_once(" WHERE ").map(|(s, _)| s).unwrap();
        let sql = format!(
            "{base} WHERE type=?1 AND (status IS NULL OR status='active') \
             AND valid_until IS NULL ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([Store::config(self).contradiction_verb()], row_to_edge)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn pair_linked(&self, a: &str, b: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges \
             WHERE (from_id=?1 AND to_id=?2) OR (from_id=?2 AND to_id=?1)",
            params![a, b],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    fn has_active_conflict(&self, id: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE type=?2 \
             AND (status IS NULL OR status='active') AND (from_id=?1 OR to_id=?1)",
            params![id, Store::config(self).contradiction_verb()],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    // ---- bulk ------------------------------------------------------------

    fn import_raw(&self, nodes: &[Node], edges: &[Edge]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for n in nodes {
            self.upsert_node(n)?;
        }
        for e in edges {
            self.upsert_edge(e)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn archive_nodes(&self, ids: &[String], ts: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE nodes SET valid_until=?1 WHERE id=?2 AND valid_until IS NULL",
                params![ts, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- search primitives ----------------------------------------------

    fn search_fts(&self, query: &str, types: &[NodeType], limit: usize) -> Result<Vec<SearchHit>> {
        let policy = self.policy();
        let fts = fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = format!(
            "SELECT n.id, n.type, n.title, \
                    snippet(nodes_fts, -1, '{SNIPPET_OPEN}', '{SNIPPET_CLOSE}', '…', 12) AS snip, \
                    n.durability, n.status, bm25(nodes_fts) AS rank, \
                    n.created_at, n.confirmed_at, n.approved_at, \
                    n.demoted_at, n.trust_override \
             FROM nodes_fts JOIN nodes n ON n.rowid = nodes_fts.rowid \
             WHERE nodes_fts MATCH ?1 AND n.valid_until IS NULL",
        );
        if !types.is_empty() {
            let list = types
                .iter()
                .map(|t| format!("'{}'", t.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND n.type IN ({list})"));
        }
        sql.push_str(" ORDER BY rank LIMIT ?2");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![fts, limit as i64], |row| {
            let type_s: String = row.get(1)?;
            let dur_s: String = row.get(4)?;
            let status_s: Option<String> = row.get(5)?;
            let rank: f64 = row.get(6)?;
            let durability = conv(4, &dur_s, Durability::parse)?;
            let status = status_s
                .map(|s| conv(5, &s, NodeStatus::parse))
                .transpose()?;
            let trust = crate::policy::trust(
                &crate::policy::TrustInputs {
                    created_at: row.get(7)?,
                    confirmed_at: row.get(8)?,
                    approved_at: row.get(9)?,
                    demoted_at: row.get(10)?,
                    trust_override: row.get(11)?,
                    durability,
                    status,
                },
                now(),
                &policy,
            );
            Ok(SearchHit {
                id: row.get(0)?,
                node_type: conv(1, &type_s, NodeType::parse)?,
                title: row.get(2)?,
                snippet: row.get(3)?,
                // bm25 returns smaller = better (negative); flip so higher = better.
                score: -rank,
                durability,
                status,
                trust,
                stale: crate::policy::is_stale(trust, &policy),
                neighbors: Vec::new(),
                project: None,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn search_vec(&self, query: &[f32], k: usize) -> Result<Vec<(String, f64)>> {
        // Multiple chunks per node share the KNN space: over-fetch, keep each
        // node's best chunk (rows arrive distance-ascending), return k nodes.
        let json = serde_json::to_string(query)?;
        let mut stmt = self.conn.prepare(
            "SELECT node_id, distance FROM vec_nodes \
             WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
        )?;
        let over = (k * 4).max(k + 8) as i64;
        let rows = stmt.query_map(params![json, over], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?;
        let mut out: Vec<(String, f64)> = Vec::new();
        for row in rows {
            let (key, distance) = row?;
            let id = key.split('#').next().unwrap_or(&key).to_string();
            if !out.iter().any(|(seen, _)| *seen == id) {
                out.push((id, distance));
                if out.len() == k {
                    break;
                }
            }
        }
        Ok(out)
    }

    fn upsert_embeddings(&self, node_id: &str, vectors: &[Vec<f32>]) -> Result<()> {
        // vec0 has no UPSERT, so delete-then-insert. The node-level vector
        // keeps the plain id (embedding_of and legacy layouts read it);
        // claim vectors live under `id#NN`.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM vec_nodes WHERE node_id = ?1 OR node_id LIKE ?1 || '#%'",
            [node_id],
        )?;
        for (i, vector) in vectors.iter().enumerate() {
            let key = if i == 0 {
                node_id.to_string()
            } else {
                format!("{node_id}#{i:02}")
            };
            tx.execute(
                "INSERT INTO vec_nodes(node_id, embedding) VALUES (?1, ?2)",
                params![key, serde_json::to_string(vector)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn embedding_of(&self, node_id: &str) -> Result<Option<Vec<f32>>> {
        // vec0 keeps little-endian f32 bytes.
        let blob: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT embedding FROM vec_nodes WHERE node_id=?1",
                [node_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(blob.map(|b| {
            b.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        }))
    }

    // ---- suspects --------------------------------------------------------

    fn suspect_between(&self, a: &str, b: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM suspects \
             WHERE (a_id=?1 AND b_id=?2) OR (a_id=?2 AND b_id=?1)",
            params![a, b],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    fn add_suspect(
        &self,
        a_id: &str,
        b_id: &str,
        similarity: f64,
        hint: Option<(&str, f64, Option<&str>)>,
    ) -> Result<Suspect> {
        let id = crate::id::new_id();
        self.conn.execute(
            "INSERT INTO suspects (id, a_id, b_id, similarity, created_at, status, nli_label, nli_score, nli_direction)
             VALUES (?1, ?2, ?3, ?4, ?5, 'suspected', ?6, ?7, ?8)",
            params![
                id,
                a_id,
                b_id,
                similarity,
                now(),
                hint.map(|(l, _, _)| l),
                hint.map(|(_, s, _)| s),
                hint.and_then(|(_, _, d)| d),
            ],
        )?;
        self.get_suspect(&id)?.ok_or(Error::NotFound(id))
    }

    fn get_suspect(&self, id: &str) -> Result<Option<Suspect>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, a_id, b_id, similarity, created_at, status, nli_label, nli_score, \
                 nli_direction FROM suspects WHERE id=?1",
                [id],
                row_to_suspect,
            )
            .optional()?)
    }

    fn set_suspect_status(&self, id: &str, status: SuspectStatus) -> Result<Suspect> {
        self.conn.execute(
            "UPDATE suspects SET status=?1 WHERE id=?2",
            params![status.as_str(), id],
        )?;
        self.get_suspect(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn suspects_pending(&self) -> Result<Vec<SuspectView>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.similarity, s.created_at, s.nli_label, s.nli_score,
                    a.id, a.type, a.title, b.id, b.type, b.title, s.nli_direction
             FROM suspects s
             JOIN nodes a ON a.id = s.a_id
             JOIN nodes b ON b.id = s.b_id
             WHERE s.status='suspected'
               AND a.valid_until IS NULL AND b.valid_until IS NULL
             ORDER BY s.created_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let a_type: String = r.get(6)?;
            let b_type: String = r.get(9)?;
            Ok(SuspectView {
                id: r.get(0)?,
                similarity: r.get(1)?,
                created_at: r.get(2)?,
                nli_label: r.get(3)?,
                nli_score: r.get(4)?,
                nli_direction: r.get(11)?,
                a: SuspectEndpoint {
                    id: r.get(5)?,
                    node_type: conv(6, &a_type, NodeType::parse)?,
                    title: r.get(7)?,
                },
                b: SuspectEndpoint {
                    id: r.get(8)?,
                    node_type: conv(9, &b_type, NodeType::parse)?,
                    title: r.get(10)?,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn all_suspects(&self) -> Result<Vec<Suspect>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, a_id, b_id, similarity, created_at, status, nli_label, nli_score, \
             nli_direction FROM suspects ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], row_to_suspect)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn upsert_suspect(&self, s: &Suspect) -> Result<()> {
        self.conn.execute(
            "INSERT INTO suspects (id, a_id, b_id, similarity, created_at, status, nli_label, nli_score)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET
               a_id=excluded.a_id, b_id=excluded.b_id, similarity=excluded.similarity,
               created_at=excluded.created_at, status=excluded.status,
               nli_label=excluded.nli_label, nli_score=excluded.nli_score",
            params![
                s.id,
                s.a_id,
                s.b_id,
                s.similarity,
                s.created_at,
                s.status.as_str(),
                s.nli_label,
                s.nli_score,
            ],
        )?;
        Ok(())
    }

    fn scannable_nodes(&self) -> Result<Vec<Node>> {
        let policy = self.policy();
        let cfg = Store::config(self);
        let anchors: Vec<&str> = cfg
            .ontology
            .types
            .iter()
            .filter(|t| t.roles.anchor)
            .map(|t| t.name.as_str())
            .collect();
        let exclude = if anchors.is_empty() {
            String::new()
        } else {
            format!(" AND type NOT IN ({})", placeholders(anchors.len()))
        };
        let sql =
            format!("{NODE_SELECT} WHERE valid_until IS NULL{exclude} ORDER BY created_at DESC");
        let params: Vec<&dyn ToSql> = anchors.iter().map(|s| s as &dyn ToSql).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |r| row_to_node(r, &policy))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- decay -----------------------------------------------------------

    fn decay_candidates(&self, ttl_secs: i64, now_ts: i64) -> Result<Vec<Node>> {
        let policy = self.policy();
        let sql = format!(
            "{NODE_SELECT} WHERE valid_until IS NULL AND source='claude' \
             AND approved_at IS NULL AND trust_override IS NULL \
             AND durability IN ('episodic','volatile') \
             ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| row_to_node(r, &policy))?;
        let mut out = Vec::new();
        for node in rows {
            let node = node?;
            let Some(since) = crate::policy::stale_since(&node.trust_inputs(), &policy) else {
                continue;
            };
            if now_ts - since >= ttl_secs {
                out.push(node);
            }
        }
        Ok(out)
    }

    // ---- audit journal ---------------------------------------------------

    fn add_audit(&self, e: &AuditEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO audit
               (ts, action, entity, entity_id, title, before_json, after_json,
                origin, session_id, cwd, pid, version)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                e.ts,
                e.action,
                e.entity,
                e.entity_id,
                e.title,
                e.before.as_ref().map(serde_json::Value::to_string),
                e.after.as_ref().map(serde_json::Value::to_string),
                e.origin,
                e.session_id,
                e.cwd,
                e.pid,
                e.version,
            ],
        )?;
        Ok(())
    }

    fn audit_page(
        &self,
        before: Option<i64>,
        entity_id: Option<&str>,
        limit: usize,
    ) -> Result<AuditPage> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit WHERE (?1 IS NULL OR entity_id = ?1)",
            params![entity_id],
            |r| r.get(0),
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT seq, ts, action, entity, entity_id, title, before_json,
                    after_json, origin, session_id, cwd, pid, version
             FROM audit
             WHERE (?1 IS NULL OR seq < ?1) AND (?2 IS NULL OR entity_id = ?2)
             ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![before, entity_id, limit as i64], row_to_audit)?;
        Ok(AuditPage {
            entries: rows.collect::<rusqlite::Result<_>>()?,
            total,
        })
    }

    // ---- brief queries ---------------------------------------------------

    fn count_by_type_active(&self, t: &NodeType) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE valid_until IS NULL AND type=?1",
            [t.as_str()],
            |r| r.get(0),
        )?)
    }

    fn nodes_by_type_active(&self, t: &NodeType, limit: usize) -> Result<Vec<Node>> {
        let policy = self.policy();
        let sql = format!(
            "{NODE_SELECT} WHERE valid_until IS NULL AND type=?1 \
             ORDER BY (trust_override IS NOT NULL) DESC, (approved_at IS NOT NULL) DESC, \
             COALESCE(approved_at, confirmed_at, created_at) DESC, rowid DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![t.as_str(), limit as i64], |r| {
            row_to_node(r, &policy)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn recent_nodes(&self, limit: usize) -> Result<Vec<Node>> {
        let policy = self.policy();
        // created_at has second granularity; rowid breaks ties by insertion order.
        let sql = format!(
            "{NODE_SELECT} WHERE valid_until IS NULL \
             ORDER BY created_at DESC, rowid DESC LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([limit as i64], |r| row_to_node(r, &policy))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn tag_stats(&self, limit: usize) -> Result<Vec<TagStat>> {
        // "Used" leans on the trust clock: a node touched by write or
        // retrieval refreshes its tags' recency too.
        let mut stmt = self.conn.prepare(
            "SELECT je.value, COUNT(*), MAX(COALESCE(n.last_seen, n.created_at)) \
             FROM nodes n, json_each(COALESCE(n.tags, '[]')) je \
             WHERE n.valid_until IS NULL \
             GROUP BY je.value \
             ORDER BY 3 DESC, 2 DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(TagStat {
                tag: r.get(0)?,
                count: r.get(1)?,
                last_used: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .optional()?)
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

const NODE_SELECT: &str = "SELECT id, type, title, body, durability, source, session_id, \
     created_at, valid_from, valid_until, status, code_refs, last_seen, approved_at, tags, \
     confirmed_at, demoted_at, trust_override, version FROM nodes";

const EDGE_SELECT: &str = "SELECT id, type, from_id, to_id, source, created_at, \
     confidence, strength, note, valid_from, valid_until, status FROM edges WHERE id=?1";

fn placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(",")
}

fn column_exists(conn: &Connection, table: &str, col: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == col {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Map a parse error into a column-conversion error so query closures stay `?`-clean.
fn conv<T>(idx: usize, val: &str, f: impl Fn(&str) -> Result<T>) -> rusqlite::Result<T> {
    f(val).map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e)))
}

fn row_to_node(row: &Row, policy: &PolicyConfig) -> rusqlite::Result<Node> {
    let type_s: String = row.get(1)?;
    let dur_s: String = row.get(4)?;
    let src_s: String = row.get(5)?;
    let status_s: Option<String> = row.get(10)?;
    let refs_s: Option<String> = row.get(11)?;
    let created_at: i64 = row.get(7)?;
    let last_seen: Option<i64> = row.get(12)?;
    let approved_at: Option<i64> = row.get(13)?;
    let tags_s: Option<String> = row.get(14)?;
    let confirmed_at: Option<i64> = row.get(15)?;
    let demoted_at: Option<i64> = row.get(16)?;
    let trust_override: Option<f64> = row.get(17)?;
    let durability = conv(4, &dur_s, Durability::parse)?;
    let status = status_s
        .map(|s| conv(10, &s, NodeStatus::parse))
        .transpose()?;
    let trust = crate::policy::trust(
        &crate::policy::TrustInputs {
            created_at,
            confirmed_at,
            approved_at,
            demoted_at,
            trust_override,
            durability,
            status,
        },
        now(),
        policy,
    );
    Ok(Node {
        id: row.get(0)?,
        node_type: conv(1, &type_s, NodeType::parse)?,
        title: row.get(2)?,
        body: row.get(3)?,
        durability,
        source: conv(5, &src_s, Source::parse)?,
        session_id: row.get(6)?,
        created_at,
        valid_from: row.get(8)?,
        valid_until: row.get(9)?,
        status,
        last_seen,
        confirmed_at,
        approved_at,
        demoted_at,
        trust_override,
        trust,
        stale: crate::policy::is_stale(trust, policy),
        version: row.get(18)?,
        code_refs: match refs_s {
            Some(s) => serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(11, Type::Text, Box::new(e))
            })?,
            None => Vec::new(),
        },
        tags: match tags_s {
            Some(s) => serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(14, Type::Text, Box::new(e))
            })?,
            None => Vec::new(),
        },
    })
}

fn row_to_audit(row: &Row) -> rusqlite::Result<AuditEntry> {
    let parse_json = |idx: usize, s: Option<String>| {
        s.map(|s| {
            serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
            })
        })
        .transpose()
    };
    Ok(AuditEntry {
        seq: row.get(0)?,
        ts: row.get(1)?,
        action: row.get(2)?,
        entity: row.get(3)?,
        entity_id: row.get(4)?,
        title: row.get(5)?,
        before: parse_json(6, row.get(6)?)?,
        after: parse_json(7, row.get(7)?)?,
        origin: row.get(8)?,
        session_id: row.get(9)?,
        cwd: row.get(10)?,
        pid: row.get(11)?,
        version: row.get(12)?,
    })
}

fn row_to_suspect(row: &Row) -> rusqlite::Result<Suspect> {
    let status_s: String = row.get(5)?;
    Ok(Suspect {
        id: row.get(0)?,
        a_id: row.get(1)?,
        b_id: row.get(2)?,
        similarity: row.get(3)?,
        created_at: row.get(4)?,
        status: conv(5, &status_s, SuspectStatus::parse)?,
        nli_label: row.get(6)?,
        nli_score: row.get(7)?,
        nli_direction: row.get(8)?,
    })
}

fn row_to_edge(row: &Row) -> rusqlite::Result<Edge> {
    let type_s: String = row.get(1)?;
    let src_s: String = row.get(4)?;
    let status_s: Option<String> = row.get(11)?;
    Ok(Edge {
        id: row.get(0)?,
        edge_type: conv(1, &type_s, EdgeType::parse)?,
        from_id: row.get(2)?,
        to_id: row.get(3)?,
        source: conv(4, &src_s, Source::parse)?,
        created_at: row.get(5)?,
        confidence: row.get(6)?,
        strength: row.get(7)?,
        note: row.get(8)?,
        valid_from: row.get(9)?,
        valid_until: row.get(10)?,
        status: status_s
            .map(|s| conv(11, &s, EdgeStatus::parse))
            .transpose()?,
    })
}

/// Turn arbitrary user text into a safe FTS5 MATCH expression: each whitespace
/// token becomes a quoted term, OR-ed. OR (not the default AND) because natural
/// multi-word queries rarely contain *every* term — bm25 already ranks docs
/// matching more terms higher, so OR keeps recall without hurting precision.
fn fts_query(q: &str) -> String {
    q.split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}
