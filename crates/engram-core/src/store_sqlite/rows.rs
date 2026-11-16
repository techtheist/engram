//! Row mapping, shared SELECT lists and query helpers.

use super::*;

pub(super) fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .optional()?)
}

pub(super) fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// sqlite-vec's hard ceiling on a KNN `k` ("k value in knn query too large,
/// provided N and the limit is 4096"). It is a refusal, not a clamp, so every
/// caller path that can grow k has to respect it.
pub(super) const VEC_MAX_K: i64 = 4096;

pub(super) const NODE_SELECT: &str = "SELECT id, type, title, body, durability, source, session_id, \
     created_at, valid_from, valid_until, status, code_refs, last_seen, approved_at, tags, \
     confirmed_at, demoted_at, trust_override, version, props, fields FROM nodes";

pub(super) const EDGE_SELECT: &str = "SELECT id, type, from_id, to_id, source, created_at, \
     confidence, strength, note, valid_from, valid_until, status FROM edges WHERE id=?1";

pub(super) fn placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(",")
}

pub(super) fn column_exists(conn: &Connection, table: &str, col: &str) -> Result<bool> {
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
pub(super) fn conv<T>(idx: usize, val: &str, f: impl Fn(&str) -> Result<T>) -> rusqlite::Result<T> {
    f(val).map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e)))
}

pub(super) fn row_to_node(row: &Row, policy: &PolicyConfig) -> rusqlite::Result<Node> {
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
        props: match row.get::<_, Option<String>>(19)? {
            Some(s) => Some(serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(19, Type::Text, Box::new(e))
            })?),
            None => None,
        },
        fields: match row.get::<_, Option<String>>(20)? {
            Some(s) => Some(serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(20, Type::Text, Box::new(e))
            })?),
            None => None,
        },
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

pub(super) fn row_to_audit(row: &Row) -> rusqlite::Result<AuditEntry> {
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
        op_id: row.get(13)?,
        undone_by: None,
    })
}

pub(super) fn row_to_suspect(row: &Row) -> rusqlite::Result<Suspect> {
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
        via: row.get(9)?,
    })
}

pub(super) fn row_to_edge(row: &Row) -> rusqlite::Result<Edge> {
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
pub(super) fn fts_query(q: &str) -> String {
    q.split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}
