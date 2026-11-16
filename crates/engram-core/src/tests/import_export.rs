//! Export and import.

use super::*;

#[test]
fn export_import_roundtrip() {
    let e = engine();
    let a = e
        .add_node(new_node(
            NodeType::Decision,
            "Use Rust backend",
            "rmcp + rusqlite",
        ))
        .unwrap();
    let b = e
        .add_node(new_node(
            NodeType::Principle,
            "Local-first",
            "own your data",
        ))
        .unwrap();
    e.add_edge(NewEdge {
        edge_type: EdgeType::Because,
        from_id: a.id.clone(),
        to_id: b.id.clone(),
        source: Source::Agent,
        note: Some("justified by".into()),
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();

    let snapshot = e.export().unwrap();
    assert_eq!(snapshot.version, 1);
    assert_eq!(snapshot.nodes.len(), 2);
    assert_eq!(snapshot.edges.len(), 1);

    // round-trip through JSON, like the HTTP/CLI boundary
    let json = serde_json::to_string(&snapshot).unwrap();
    let parsed: ExportGraph = serde_json::from_str(&json).unwrap();

    // import into a fresh, empty graph
    let fresh = engine();
    let summary = fresh.import(parsed).unwrap();
    assert_eq!(summary.nodes, 2);
    assert_eq!(summary.edges, 1);

    // ids + content preserved exactly (trust is computed from "now" on every
    // read, so normalize it away — the timestamps it derives from ARE compared)
    let normalize = |mut n: Node| {
        n.trust = 0.0;
        n.stale = false;
        n
    };
    assert_eq!(
        normalize(fresh.get_node(&a.id).unwrap().unwrap()),
        normalize(a.clone())
    );
    assert_eq!(fresh.edges_out(&a.id).unwrap().len(), 1);
    // embeddings regenerated on import → search works
    assert_eq!(fresh.search("rust", &[], 5).unwrap()[0].id, a.id);
}

#[test]
fn import_is_idempotent() {
    let e = engine();
    e.add_node(new_node(NodeType::Decision, "X", "y")).unwrap();
    let snap = e.export().unwrap();

    let fresh = engine();
    fresh.import(snap.clone()).unwrap();
    fresh.import(snap).unwrap(); // re-import must not duplicate
    assert_eq!(fresh.graph().unwrap().0.len(), 1);
}

#[test]
fn import_rejects_future_version() {
    let e = engine();
    let mut snap = e.export().unwrap();
    snap.version = 999;
    assert!(e.import(snap).is_err());
}

#[test]
fn export_order_is_stable() {
    let e = engine();
    for t in ["a", "b", "c"] {
        e.add_node(new_node(NodeType::Insight, t, "")).unwrap();
    }
    let first = serde_json::to_string(&e.export().unwrap()).unwrap();
    let second = serde_json::to_string(&e.export().unwrap()).unwrap();
    assert_eq!(
        first, second,
        "re-export of an unchanged graph must be byte-identical"
    );
}
