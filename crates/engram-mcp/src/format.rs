//! Reply helpers: JSON shaping, timestamps, hierarchies, error mapping.

use crate::*;

/// The store marks matches with private-use sentinels (the pane's highlight
/// markers); assistants read plain brackets instead.
pub(crate) fn debracket(hit: &mut engram_core::SearchHit) {
    hit.snippet = hit
        .snippet
        .replace(engram_core::SNIPPET_OPEN, "[")
        .replace(engram_core::SNIPPET_CLOSE, "]");
}

pub(crate) fn ok_json<T: Serialize>(v: &T) -> Result<CallToolResult, ErrorData> {
    let mut value =
        serde_json::to_value(v).map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    tidy(&mut value);
    let text = serde_json::to_string_pretty(&value)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// Keys whose integer values are unix seconds. Rendered as ISO instants in
/// tool replies so an assistant never converts epoch numbers itself — and can
/// paste any date it was shown straight back into `after`/`before` (the
/// temporal grammar reads the same ISO form).
pub(crate) const TIMESTAMP_KEYS: [&str; 16] = [
    "created_at",
    "confirmed_at",
    "approved_at",
    "demoted_at",
    "valid_from",
    "valid_until",
    "last_seen",
    "timestamp",
    "ts",
    "first_at",
    "last_at",
    "started_at",
    "ended_at",
    "touched_at",
    // Tasks.
    "updated_at",
    "closed_at",
];

/// Prune nulls, empty arrays and empty objects, and render known timestamp keys
/// as ISO instants: every reply is context the assistant pays for. MCP replies
/// only; the pane and exports keep raw shapes.
pub(crate) fn tidy(v: &mut serde_json::Value) {
    use serde_json::Value;
    // Sanity band so a small integer that merely shares a key name (a count,
    // a turn number) is never mistaken for an instant: 2000-01-01..2100.
    const EPOCH_BAND: std::ops::Range<i64> = 946_684_800..4_102_444_800;
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if TIMESTAMP_KEYS.contains(&k.as_str())
                    && let Some(ts) = val.as_i64()
                    && EPOCH_BAND.contains(&ts)
                {
                    *val = Value::String(engram_core::timespec::iso_instant(ts));
                } else if k == "fields" && val.is_object() {
                    // Custom-field values are user-shaped data, not API
                    // metadata: a numeric field that happens to be named
                    // like a timestamp key must come back exactly as
                    // stored. Only null pruning applies inside.
                    if let Value::Object(fields) = val {
                        fields.retain(|_, fv| !fv.is_null());
                    }
                } else {
                    tidy(val);
                }
            }
            map.retain(|_, val| {
                !(val.is_null()
                    || val.as_array().is_some_and(std::vec::Vec::is_empty)
                    || val.as_object().is_some_and(serde_json::Map::is_empty))
            });
        }
        Value::Array(items) => {
            for item in items {
                tidy(item);
            }
        }
        _ => {}
    }
}

pub(crate) const HIERARCHY_MAX_DEPTH: usize = 3;
/// Found bridges a `get_node` carries, and the search hits that
/// carry one each (snippet/full only): the economical shape — a bridge is a
/// pointer worth following, not context to read.
pub(crate) const NODE_BRIDGES: usize = 3;
pub(crate) const SEARCH_BRIDGE_HITS: usize = 3;
pub(crate) const HIERARCHY_MAX_BREADTH: usize = 8;

/// Recursive reasoning hierarchy around a node. `up` follows outgoing edges
/// (parents: what this node stands on / is about); `!up` follows incoming
/// (children: what answers, builds on, or contradicts it). Depth and breadth
/// are capped and cycles cut so the payload stays context-window friendly.
pub(crate) fn hierarchy(
    engine: &engram_core::Engine,
    id: &str,
    depth: usize,
    up: bool,
    seen: &mut std::collections::HashSet<String>,
) -> Vec<serde_json::Value> {
    if depth == 0 {
        return Vec::new();
    }
    let edges = if up {
        engine.edges_out(id)
    } else {
        engine.edges_in(id)
    }
    .unwrap_or_default();
    let mut out = Vec::new();
    for e in edges.into_iter().take(HIERARCHY_MAX_BREADTH) {
        let other = if up { &e.to_id } else { &e.from_id };
        if !seen.insert(other.clone()) {
            continue;
        }
        let Ok(Some(n)) = engine.get_node(other) else {
            continue;
        };
        let deeper = hierarchy(engine, other, depth - 1, up, seen);
        let mut item = json!({
            "edge": e.edge_type.as_str(),
            "node": {
                "id": n.id,
                "type": n.node_type.as_str(),
                "title": n.title,
                "status": n.status.map(engram_core::NodeStatus::as_str),
                "trust": (n.trust * 100.0).round() / 100.0,
                "stale": n.stale,
                "archived": n.valid_until.is_some(),
            }
        });
        if !deeper.is_empty() {
            item[if up { "parents" } else { "children" }] = json!(deeper);
        }
        out.push(item);
    }
    out
}

/// Task refusals teach (bad rule, nested group, unknown ref) — they are the
/// caller's to fix, so they surface as invalid params, never internal.
pub(crate) fn task_err(e: Error) -> ErrorData {
    match e {
        Error::Config(s) => ErrorData::invalid_params(s, None),
        e => map_err(e),
    }
}

pub(crate) fn map_err(e: Error) -> ErrorData {
    match e {
        Error::NotFound(s) => ErrorData::invalid_params(format!("not found: {s}"), None),
        e @ (Error::Parse { .. } | Error::Pinned(_) | Error::Project(_)) => {
            ErrorData::invalid_params(e.to_string(), None)
        }
        e => ErrorData::internal_error(e.to_string(), None),
    }
}

pub(crate) fn node_types(v: &[String]) -> Result<Vec<NodeType>, ErrorData> {
    v.iter()
        .map(|s| NodeType::parse(s))
        .collect::<engram_core::Result<_>>()
        .map_err(map_err)
}

pub(crate) fn edge_types(v: &[String]) -> Result<Vec<EdgeType>, ErrorData> {
    v.iter()
        .map(|s| EdgeType::parse(s))
        .collect::<engram_core::Result<_>>()
        .map_err(map_err)
}

#[cfg(test)]
mod tests {
    use super::tidy;
    use serde_json::json;

    #[test]
    pub(crate) fn tidy_prunes_noise_and_renders_instants() {
        let mut v = json!({
            "id": "n1",
            "status": null,
            "tags": [],
            "props": {},
            "created_at": 1787409403_i64,
            "turn": 55,
            "score": 0.73,
            "edges": [{"strength": null, "valid_from": 1787409403_i64, "note": "kept"}],
            "fields": {"timestamp": 1787409403_i64, "ts": 42, "gone": null},
        });
        tidy(&mut v);
        let o = v.as_object().unwrap();
        assert!(!o.contains_key("status"), "nulls are pruned");
        assert!(!o.contains_key("tags"), "empty arrays are pruned");
        assert!(!o.contains_key("props"), "empty objects are pruned");
        assert_eq!(o["created_at"], "2026-08-22T14:36:43Z");
        assert_eq!(o["turn"], 55, "small ints never masquerade as instants");
        assert_eq!(o["score"], 0.73);
        let e = v["edges"][0].as_object().unwrap();
        assert!(!e.contains_key("strength"), "pruning recurses");
        assert_eq!(e["valid_from"], "2026-08-22T14:36:43Z");
        assert_eq!(e["note"], "kept");
        // Custom-field values are user data: a numeric field that happens to
        // share a timestamp key name must come back exactly as stored.
        let f = v["fields"].as_object().unwrap();
        assert_eq!(
            f["timestamp"], 1787409403_i64,
            "never ISO-ified inside fields"
        );
        assert_eq!(f["ts"], 42);
        assert!(!f.contains_key("gone"), "null values still prune");
    }
}
