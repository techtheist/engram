/// Base schema: nodes, edges, indexes, and the FTS5 mirror kept in sync by
/// triggers. The `vec_nodes` virtual table lives in a separate migration that
/// runs only once the sqlite-vec extension is loaded (see the rag module).
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

CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts
  USING fts5(title, body, content='nodes', content_rowid='rowid');

CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
  INSERT INTO nodes_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
  INSERT INTO nodes_fts(nodes_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
END;
CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE ON nodes BEGIN
  INSERT INTO nodes_fts(nodes_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
  INSERT INTO nodes_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
"#;
