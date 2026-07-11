/// Base schema: nodes, edges, indexes. The FTS mirror lives in [`FTS_SCHEMA`]
/// (rebuilt by migration when its column set changes); the `vec_nodes` virtual
/// table lives in a separate migration that runs only once the sqlite-vec
/// extension is loaded (see the rag module).
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS nodes (
  id          TEXT PRIMARY KEY,
  type        TEXT NOT NULL,
  title       TEXT NOT NULL,
  body        TEXT,
  durability  TEXT NOT NULL,
  source      TEXT NOT NULL,
  session_id  TEXT,
  created_at  INTEGER NOT NULL,
  valid_from  INTEGER,
  valid_until INTEGER,
  status      TEXT,
  code_refs   TEXT,
  tags        TEXT,               -- JSON array of user-facing slice labels
  last_seen   INTEGER,            -- last time retrieval surfaced this node
  approved_at INTEGER             -- last explicit approval; trust anchors here
);

CREATE TABLE IF NOT EXISTS edges (
  id          TEXT PRIMARY KEY,
  type        TEXT NOT NULL,
  from_id     TEXT NOT NULL REFERENCES nodes(id),
  to_id       TEXT NOT NULL REFERENCES nodes(id),
  source      TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  confidence  REAL,
  strength    REAL,
  note        TEXT,
  valid_from  INTEGER,
  valid_until INTEGER,
  status      TEXT
);

CREATE INDEX IF NOT EXISTS idx_edges_from ON edges(from_id);
CREATE INDEX IF NOT EXISTS idx_edges_to   ON edges(to_id);
CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(type);
CREATE INDEX IF NOT EXISTS idx_nodes_stat ON nodes(status);

-- Conflict-scan queue (PLAN §7): locally-detected candidate contradictions.
-- Pairs are stored newer-first (a = newer); judged rows stay so a pair is
-- never re-raised.
CREATE TABLE IF NOT EXISTS suspects (
  id          TEXT PRIMARY KEY,
  a_id        TEXT NOT NULL REFERENCES nodes(id),
  b_id        TEXT NOT NULL REFERENCES nodes(id),
  similarity  REAL NOT NULL,
  created_at  INTEGER NOT NULL,
  status      TEXT NOT NULL DEFAULT 'suspected',
  UNIQUE(a_id, b_id)
);

-- Append-only audit journal (PLAN §10): one row per node/edge mutation with
-- before/after snapshots plus the binary-side context of the writing process.
-- Rows are only ever inserted; `seq` is the pagination cursor.
CREATE TABLE IF NOT EXISTS audit (
  seq         INTEGER PRIMARY KEY AUTOINCREMENT,
  ts          INTEGER NOT NULL,
  action      TEXT NOT NULL,       -- created | updated | approved | archived | deleted | imported
  entity      TEXT NOT NULL,       -- node | edge | graph
  entity_id   TEXT NOT NULL,
  title       TEXT,                -- display label snapshot; survives deletion
  before_json TEXT,                -- full entity JSON before the write (null on create)
  after_json  TEXT,                -- full entity JSON after the write (null on delete)
  origin      TEXT NOT NULL,       -- pane | mcp | daemon | cli | library
  session_id  TEXT,
  cwd         TEXT,
  pid         INTEGER,
  version     TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_entity ON audit(entity_id);
"#;

/// The FTS5 mirror and the triggers that keep it in sync. Dropped and re-run
/// (plus a rebuild) whenever a DB's `nodes_fts` lacks a column — see
/// `Store::ensure_fts`.
pub const FTS_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts
  USING fts5(title, body, tags, code_refs, content='nodes', content_rowid='rowid');

CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
  INSERT INTO nodes_fts(rowid, title, body, tags, code_refs) VALUES (new.rowid, new.title, new.body, new.tags, new.code_refs);
END;
CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
  INSERT INTO nodes_fts(nodes_fts, rowid, title, body, tags, code_refs) VALUES('delete', old.rowid, old.title, old.body, old.tags, old.code_refs);
END;
CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE ON nodes BEGIN
  INSERT INTO nodes_fts(nodes_fts, rowid, title, body, tags, code_refs) VALUES('delete', old.rowid, old.title, old.body, old.tags, old.code_refs);
  INSERT INTO nodes_fts(rowid, title, body, tags, code_refs) VALUES (new.rowid, new.title, new.body, new.tags, new.code_refs);
END;
"#;
