use std::path::Path;
use std::sync::Once;

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};

use crate::rag::EMBED_DIM;
use crate::schema::{FTS_SCHEMA, SCHEMA};
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

/// Unix seconds. The single clock for created_at / valid_from.
pub fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The graph store: one SQLite connection bound to one repo's `.engram/graph.db`.
/// Writes are serialized by `&mut`-free design + the daemon's single writer; WAL
/// gives concurrent reads (PLAN §6B).
pub struct Store {
    conn: Connection,
}

impl Store {
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
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_nodes USING vec0(
               node_id TEXT PRIMARY KEY,
               embedding float[{EMBED_DIM}] distance_metric=cosine
             );"
        ))?;
        let store = Self { conn };
        store.shorten_legacy_ids()?;
        Ok(store)
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
        Ok(())
    }

    /// Keep `nodes_fts` in lockstep with [`FTS_SCHEMA`]: when a database's FTS
    /// mirror predates a column (tags), drop it — triggers included — and
    /// recreate + rebuild from the content table. Fresh databases just create.
    fn ensure_fts(conn: &Connection) -> Result<()> {
        let fts_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='nodes_fts'",
            [],
            |r| r.get(0),
        )?;
        let outdated = fts_exists > 0 && !column_exists(conn, "nodes_fts", "tags")?;
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

    /// Escape hatch for the rag module to add the sqlite-vec table / queries.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ---- nodes -----------------------------------------------------------

    pub fn add_node(&self, n: NewNode) -> Result<Node> {
        let id = crate::id::new_id();
        let created = now();
        let title = crate::redact::scrub(&n.title);
        let body = n.body.as_deref().map(crate::redact::scrub);
        let code_refs = serde_json::to_string(&n.code_refs)?;
        let tags = serde_json::to_string(&normalize_tags(&n.tags))?;
        self.conn.execute(
            "INSERT INTO nodes
               (id, type, title, body, durability, source, session_id,
                created_at, valid_from, valid_until, status, code_refs, tags, last_seen, approved_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
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
            ],
        )?;
        self.get_node(&id)?.ok_or_else(|| Error::NotFound(id))
    }

    pub fn get_node(&self, id: &str) -> Result<Option<Node>> {
        let sql = format!("{NODE_SELECT} WHERE id=?1");
        Ok(self.conn.query_row(&sql, [id], row_to_node).optional()?)
    }

    pub fn update_node(&self, id: &str, p: NodePatch) -> Result<Node> {
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
        // Every update proves the node is still in use: refresh last_seen.
        sets.push("last_seen=?");
        vals.push(Box::new(now()));

        let sql = format!("UPDATE nodes SET {} WHERE id=?", sets.join(", "));
        vals.push(Box::new(id.to_string()));
        let bound: Vec<&dyn ToSql> = vals.iter().map(|b| b.as_ref()).collect();
        self.conn.execute(&sql, bound.as_slice())?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    /// Stamp an explicit approval: trust restarts at its ceiling and decays
    /// on the slow approved curve. Re-approving refreshes the stamp.
    pub fn approve(&self, id: &str) -> Result<Node> {
        let ts = now();
        self.conn.execute(
            "UPDATE nodes SET approved_at=?1, last_seen=?2 WHERE id=?3",
            params![ts, ts, id],
        )?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    /// User-only hard delete; cascades the node's edges (PLAN §6B).
    pub fn delete_node(&self, id: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM edges WHERE from_id=?1 OR to_id=?1", [id])?;
        tx.execute("DELETE FROM suspects WHERE a_id=?1 OR b_id=?1", [id])?;
        tx.execute("DELETE FROM vec_nodes WHERE node_id=?1", [id])?;
        let n = tx.execute("DELETE FROM nodes WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Open Problems/Intents — the live worklist (PLAN Appendix A `list_open`).
    pub fn list_open(&self, types: &[NodeType]) -> Result<Vec<Node>> {
        let types = if types.is_empty() {
            &[NodeType::Problem, NodeType::Intent][..]
        } else {
            types
        };
        let sql = format!(
            "{NODE_SELECT} WHERE valid_until IS NULL AND status='open' AND type IN ({}) ORDER BY created_at DESC",
            placeholders(types.len())
        );
        let strs: Vec<&'static str> = types.iter().map(|t| t.as_str()).collect();
        let params: Vec<&dyn ToSql> = strs.iter().map(|s| s as &dyn ToSql).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), row_to_node)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- raw upserts (import) -------------------------------------------

    /// Insert or replace a node by id, preserving its timestamps. Used by
    /// import; still re-runs redaction (defense in depth). `ON CONFLICT DO
    /// UPDATE` (not `INSERT OR REPLACE`) so we don't delete-and-reinsert a row
    /// that edges reference.
    pub fn upsert_node(&self, n: &Node) -> Result<()> {
        let title = crate::redact::scrub(&n.title);
        let body = n.body.as_deref().map(crate::redact::scrub);
        let code_refs = serde_json::to_string(&n.code_refs)?;
        let tags = serde_json::to_string(&normalize_tags(&n.tags))?;
        self.conn.execute(
            "INSERT INTO nodes
               (id, type, title, body, durability, source, session_id,
                created_at, valid_from, valid_until, status, code_refs, tags, last_seen, approved_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
             ON CONFLICT(id) DO UPDATE SET
               type=excluded.type, title=excluded.title, body=excluded.body,
               durability=excluded.durability, source=excluded.source,
               session_id=excluded.session_id, created_at=excluded.created_at,
               valid_from=excluded.valid_from, valid_until=excluded.valid_until,
               status=excluded.status, code_refs=excluded.code_refs,
               tags=excluded.tags, last_seen=excluded.last_seen,
               approved_at=excluded.approved_at",
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
            ],
        )?;
        Ok(())
    }

    pub fn upsert_edge(&self, e: &Edge) -> Result<()> {
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

    /// Import nodes (first) then edges in one transaction so FK references hold
    /// and a failure rolls the whole import back. Embeddings are regenerated by
    /// the caller (Engine) after this returns.
    pub fn import_raw(&self, nodes: &[Node], edges: &[Edge]) -> Result<()> {
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

    /// Nodes touched by an active `conflicts-with` edge — the contradiction
    /// surface shown in the worklist (PLAN §7).
    pub fn nodes_in_active_conflicts(&self) -> Result<Vec<Node>> {
        let sql = format!(
            "{NODE_SELECT} WHERE id IN (\
               SELECT from_id FROM edges WHERE type='conflicts-with' AND (status IS NULL OR status='active') \
               UNION \
               SELECT to_id FROM edges WHERE type='conflicts-with' AND (status IS NULL OR status='active')\
             ) ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_node)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- edges -----------------------------------------------------------

    pub fn add_edge(&self, e: NewEdge) -> Result<Edge> {
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

    pub fn get_edge(&self, id: &str) -> Result<Option<Edge>> {
        Ok(self
            .conn
            .query_row(EDGE_SELECT, [id], row_to_edge)
            .optional()?)
    }

    pub fn edges_out(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.edges_where("from_id", node_id)
    }

    pub fn edges_in(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.edges_where("to_id", node_id)
    }

    fn edges_where(&self, col: &str, node_id: &str) -> Result<Vec<Edge>> {
        let base = EDGE_SELECT.rsplit_once(" WHERE ").map(|(s, _)| s).unwrap();
        let sql = format!("{base} WHERE {col}=?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([node_id], row_to_edge)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- keyword search (FTS5) ------------------------------------------

    pub fn search_fts(
        &self,
        query: &str,
        types: &[NodeType],
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let fts = fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            "SELECT n.id, n.type, n.title, \
                    snippet(nodes_fts, -1, '[', ']', '…', 12) AS snip, \
                    n.durability, n.status, bm25(nodes_fts) AS rank, \
                    n.created_at, n.last_seen, n.approved_at \
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
            let trust = crate::policy::trust(row.get(7)?, row.get(8)?, row.get(9)?, now());
            Ok(SearchHit {
                id: row.get(0)?,
                node_type: conv(1, &type_s, NodeType::parse)?,
                title: row.get(2)?,
                snippet: row.get(3)?,
                // bm25 returns smaller = better (negative); flip so higher = better.
                score: -rank,
                durability: conv(4, &dur_s, Durability::parse)?,
                status: status_s
                    .map(|s| conv(5, &s, NodeStatus::parse))
                    .transpose()?,
                trust,
                stale: crate::policy::is_stale(trust),
                neighbors: Vec::new(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- whole-graph reads + traversal ----------------------------------

    pub fn all_nodes(&self) -> Result<Vec<Node>> {
        let sql = format!("{NODE_SELECT} ORDER BY created_at");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_node)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn all_edges(&self) -> Result<Vec<Edge>> {
        let base = EDGE_SELECT.rsplit_once(" WHERE ").map(|(s, _)| s).unwrap();
        let mut stmt = self.conn.prepare(base)?;
        let rows = stmt.query_map([], row_to_edge)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Bounded breadth-first subgraph around `from` (PLAN Appendix A `traverse`).
    pub fn traverse(
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

    // ---- vector + hybrid search -----------------------------------------

    /// Store (or replace) a node's embedding. vec0 has no UPSERT, so delete-then-insert.
    pub fn upsert_embedding(&self, node_id: &str, embedding: &[f32]) -> Result<()> {
        let json = serde_json::to_string(embedding)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM vec_nodes WHERE node_id = ?1", [node_id])?;
        tx.execute(
            "INSERT INTO vec_nodes(node_id, embedding) VALUES (?1, ?2)",
            params![node_id, json],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// k-nearest node ids by cosine distance (smaller = closer).
    pub fn search_vec(&self, query: &[f32], k: usize) -> Result<Vec<(String, f64)>> {
        let json = serde_json::to_string(query)?;
        let mut stmt = self.conn.prepare(
            "SELECT node_id, distance FROM vec_nodes \
             WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
        )?;
        let rows = stmt.query_map(params![json, k as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Hybrid retrieval: blend normalized keyword (bm25) and vector (cosine)
    /// relevance, then *modulate* by trust (user-sourced / stable /
    /// high-confidence). Trust multiplies relevance rather than adding to it,
    /// so an irrelevant-but-trusted node can't outrank an actual match —
    /// PLAN §6A retrieval.
    pub fn search_hybrid(
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
            let age = crate::now() - node.created_at;
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
    pub fn neighbors(&self, id: &str, cap: usize) -> Result<Vec<NeighborRef>> {
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

    /// Whether a node sits on an active `conflicts-with` edge.
    pub fn has_active_conflict(&self, id: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE type='conflicts-with' \
             AND (status IS NULL OR status='active') AND (from_id=?1 OR to_id=?1)",
            [id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    // ---- conflict scan (PLAN §7) -------------------------------------------

    /// The stored embedding for a node (vec0 keeps little-endian f32 bytes).
    pub fn embedding_of(&self, node_id: &str) -> Result<Option<Vec<f32>>> {
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

    /// Whether any edge — either direction, any type or status — connects the
    /// pair. Linked nodes are already consciously related: not scan material.
    pub fn pair_linked(&self, a: &str, b: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges \
             WHERE (from_id=?1 AND to_id=?2) OR (from_id=?2 AND to_id=?1)",
            params![a, b],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Whether the pair was ever raised, in either order and any status —
    /// judged (confirmed/dismissed) pairs are never re-raised.
    pub fn suspect_between(&self, a: &str, b: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM suspects \
             WHERE (a_id=?1 AND b_id=?2) OR (a_id=?2 AND b_id=?1)",
            params![a, b],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Queue a suspected pair (caller orders newer-first: a = newer).
    pub fn add_suspect(&self, a_id: &str, b_id: &str, similarity: f64) -> Result<Suspect> {
        let id = crate::id::new_id();
        self.conn.execute(
            "INSERT INTO suspects (id, a_id, b_id, similarity, created_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, 'suspected')",
            params![id, a_id, b_id, similarity, now()],
        )?;
        self.get_suspect(&id)?.ok_or(Error::NotFound(id))
    }

    pub fn get_suspect(&self, id: &str) -> Result<Option<Suspect>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, a_id, b_id, similarity, created_at, status \
                 FROM suspects WHERE id=?1",
                [id],
                row_to_suspect,
            )
            .optional()?)
    }

    pub fn set_suspect_status(&self, id: &str, status: SuspectStatus) -> Result<Suspect> {
        self.conn.execute(
            "UPDATE suspects SET status=?1 WHERE id=?2",
            params![status.as_str(), id],
        )?;
        self.get_suspect(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    /// Pending suspects with their endpoints' display fields, newest first.
    /// Pairs with an archived endpoint drop out — superseding one side settles
    /// the question.
    pub fn suspects_pending(&self) -> Result<Vec<SuspectView>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.similarity, s.created_at,
                    a.id, a.type, a.title, b.id, b.type, b.title
             FROM suspects s
             JOIN nodes a ON a.id = s.a_id
             JOIN nodes b ON b.id = s.b_id
             WHERE s.status='suspected'
               AND a.valid_until IS NULL AND b.valid_until IS NULL
             ORDER BY s.created_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let a_type: String = r.get(4)?;
            let b_type: String = r.get(7)?;
            Ok(SuspectView {
                id: r.get(0)?,
                similarity: r.get(1)?,
                created_at: r.get(2)?,
                a: SuspectEndpoint {
                    id: r.get(3)?,
                    node_type: conv(4, &a_type, NodeType::parse)?,
                    title: r.get(5)?,
                },
                b: SuspectEndpoint {
                    id: r.get(6)?,
                    node_type: conv(7, &b_type, NodeType::parse)?,
                    title: r.get(8)?,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Active non-anchor nodes — the conflict scan's iteration set (anchor
    /// labels are similar by nature, not by contradiction).
    pub fn scannable_nodes(&self) -> Result<Vec<Node>> {
        let sql = format!(
            "{NODE_SELECT} WHERE valid_until IS NULL AND type != 'Anchor' ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_node)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- decay (PLAN §6B) ----------------------------------------------------

    /// Nodes the decay pass would archive at `now_ts`: active, Claude-authored,
    /// never approved, episodic/volatile, and stale for longer than `ttl_secs`
    /// (policy::stale_since). Stable durability and user/approved knowledge
    /// never auto-archive.
    pub fn decay_candidates(&self, ttl_secs: i64, now_ts: i64) -> Result<Vec<Node>> {
        let sql = format!(
            "{NODE_SELECT} WHERE valid_until IS NULL AND source='claude' \
             AND approved_at IS NULL AND durability IN ('episodic','volatile') \
             ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_node)?;
        let mut out = Vec::new();
        for node in rows {
            let node = node?;
            let Some(since) =
                crate::policy::stale_since(node.created_at, node.last_seen, node.approved_at)
            else {
                continue;
            };
            if now_ts - since >= ttl_secs {
                out.push(node);
            }
        }
        Ok(out)
    }

    /// Archive nodes in one transaction: sets `valid_until`, preserving history
    /// (supersede-not-delete, PLAN §6B).
    pub fn archive_nodes(&self, ids: &[String], ts: i64) -> Result<()> {
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

    pub fn update_edge(&self, id: &str, p: EdgePatch) -> Result<Edge> {
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

    pub fn delete_edge(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM edges WHERE id=?1", [id])? > 0)
    }

    // ---- audit journal (PLAN §10) -----------------------------------------

    /// Append one journal row. `seq` on the input is ignored (assigned by
    /// SQLite); rows are only ever inserted, never updated or deleted.
    pub fn add_audit(&self, e: &AuditEntry) -> Result<()> {
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

    /// One journal page, newest first. Keyset pagination: pass the last seen
    /// `seq` as `before` for the next page. `entity_id` narrows to one
    /// node/edge's history; `total` counts under the same filter.
    pub fn audit_page(
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

    // ---- brief queries ----------------------------------------------------

    /// How many current (non-archived) nodes of one type exist — the brief's
    /// overflow counts.
    pub fn count_by_type_active(&self, t: NodeType) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE valid_until IS NULL AND type=?1",
            [t.as_str()],
            |r| r.get(0),
        )?)
    }

    /// Current (non-archived) nodes of one type, most trusted first.
    pub fn nodes_by_type_active(&self, t: NodeType, limit: usize) -> Result<Vec<Node>> {
        let sql = format!(
            "{NODE_SELECT} WHERE valid_until IS NULL AND type=?1 \
             ORDER BY (approved_at IS NOT NULL) DESC, \
             COALESCE(approved_at, last_seen, created_at) DESC, rowid DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![t.as_str(), limit as i64], row_to_node)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Active `conflicts-with` edges — the unresolved contradiction list.
    pub fn active_conflict_edges(&self) -> Result<Vec<Edge>> {
        let base = EDGE_SELECT.rsplit_once(" WHERE ").map(|(s, _)| s).unwrap();
        let sql = format!(
            "{base} WHERE type='conflicts-with' AND (status IS NULL OR status='active') \
             AND valid_until IS NULL ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_edge)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Most recently created current nodes (any type). `created_at` has
    /// second granularity, so rowid breaks ties by insertion order.
    pub fn recent_nodes(&self, limit: usize) -> Result<Vec<Node>> {
        let sql = format!(
            "{NODE_SELECT} WHERE valid_until IS NULL \
             ORDER BY created_at DESC, rowid DESC LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([limit as i64], row_to_node)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every tag in use on current nodes with count and freshness, freshest
    /// first (PLAN §10 tags). "Used" leans on the trust clock: a node touched
    /// by write or retrieval refreshes its tags' recency too.
    pub fn tag_stats(&self, limit: usize) -> Result<Vec<TagStat>> {
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

    /// Stamp `last_seen`: being surfaced by retrieval (search hit, brief
    /// inclusion) is what keeps a node's trust alive.
    pub fn touch(&self, ids: &[String]) -> Result<()> {
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
    b += 0.15 * node.trust;
    b
}

fn excerpt(node: &Node) -> String {
    let text = node.body.as_deref().unwrap_or(&node.title);
    text.chars().take(160).collect()
}

const NODE_SELECT: &str = "SELECT id, type, title, body, durability, source, session_id, \
     created_at, valid_from, valid_until, status, code_refs, last_seen, approved_at, tags FROM nodes";

const EDGE_SELECT: &str = "SELECT id, type, from_id, to_id, source, created_at, \
     confidence, strength, note, valid_from, valid_until, status FROM edges WHERE id=?1";

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

fn row_to_node(row: &Row) -> rusqlite::Result<Node> {
    let type_s: String = row.get(1)?;
    let dur_s: String = row.get(4)?;
    let src_s: String = row.get(5)?;
    let status_s: Option<String> = row.get(10)?;
    let refs_s: Option<String> = row.get(11)?;
    let created_at: i64 = row.get(7)?;
    let last_seen: Option<i64> = row.get(12)?;
    let approved_at: Option<i64> = row.get(13)?;
    let tags_s: Option<String> = row.get(14)?;
    let trust = crate::policy::trust(created_at, last_seen, approved_at, now());
    Ok(Node {
        id: row.get(0)?,
        node_type: conv(1, &type_s, NodeType::parse)?,
        title: row.get(2)?,
        body: row.get(3)?,
        durability: conv(4, &dur_s, Durability::parse)?,
        source: conv(5, &src_s, Source::parse)?,
        session_id: row.get(6)?,
        created_at,
        valid_from: row.get(8)?,
        valid_until: row.get(9)?,
        status: status_s
            .map(|s| conv(10, &s, NodeStatus::parse))
            .transpose()?,
        last_seen,
        approved_at,
        trust,
        stale: crate::policy::is_stale(trust),
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
