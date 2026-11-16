//! Opening a SQLite store: the extension, schema creation and migrations.

use super::*;

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

    pub(super) fn init(conn: Connection) -> Result<Self> {
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
    pub(super) fn policy(&self) -> PolicyConfig {
        self.cfg.reading().policy.clone()
    }

    /// Forward-only migrations for databases created before a column existed.
    pub(super) fn migrate(conn: &Connection) -> Result<()> {
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
        // Version tracking: the project version a node was captured
        // at, stamped from the graph's current working version.
        if !column_exists(conn, "nodes", "version")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN version TEXT;")?;
        }
        // History layer: the layer-specific extension bag.
        if !column_exists(conn, "nodes", "props")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN props TEXT;")?;
        }
        // Custom fields: user-defined first-class values.
        if !column_exists(conn, "nodes", "fields")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN fields TEXT;")?;
        }
        // Audit undo: rows of one operation share an id.
        if !column_exists(conn, "audit", "op_id")? {
            conn.execute_batch("ALTER TABLE audit ADD COLUMN op_id TEXT;")?;
        }
        // Local cortex (v0.5.0): suspects carry an optional NLI hint.
        if !column_exists(conn, "suspects", "nli_label")? {
            conn.execute_batch(
                "ALTER TABLE suspects ADD COLUMN nli_label TEXT;
                 ALTER TABLE suspects ADD COLUMN nli_score REAL;",
            )?;
        }
        // Directional conflict hints: which side carries the negation.
        if !column_exists(conn, "suspects", "via")? {
            conn.execute_batch("ALTER TABLE suspects ADD COLUMN via TEXT;")?;
        }
        if !column_exists(conn, "suspects", "nli_direction")? {
            conn.execute_batch("ALTER TABLE suspects ADD COLUMN nli_direction TEXT;")?;
        }
        Ok(())
    }

    /// Keep `nodes_fts` in lockstep with [`FTS_SCHEMA`]: when a database's FTS
    /// mirror predates a column (tags, code_refs), drop it — triggers included
    /// — and recreate + rebuild from the content table. Fresh databases just
    /// create.
    pub(super) fn ensure_fts(conn: &Connection) -> Result<()> {
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

    pub(super) fn edges_where(&self, col: &str, node_id: &str) -> Result<Vec<Edge>> {
        let base = EDGE_SELECT.rsplit_once(" WHERE ").map(|(s, _)| s).unwrap();
        let sql = format!("{base} WHERE {col}=?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([node_id], row_to_edge)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
