//! Store basics: node and edge roundtrips, FTS, redaction.

use super::*;

#[test]
fn node_type_roundtrip() {
    for t in [
        NodeType::Principle,
        NodeType::Decision,
        NodeType::Caution,
        NodeType::Problem,
        NodeType::Resolution,
        NodeType::Insight,
        NodeType::Intent,
        NodeType::Anchor,
        NodeType::Tombstone,
    ] {
        assert_eq!(NodeType::parse(t.as_str()).unwrap(), t);
    }
    // Ontology-as-data: parse checks only shape — a name the
    // shipped set doesn't know is still parseable; whether it EXISTS is the
    // engine's config-driven write-time check.
    assert_eq!(NodeType::parse("Concept").unwrap().as_str(), "Concept");
    assert!(NodeType::parse("").is_err());
    assert!(NodeType::parse("   ").is_err());
}

#[test]
fn edge_type_strings() {
    assert_eq!(EdgeType::BuildsOn.as_str(), "builds-on");
    assert_eq!(EdgeType::ConflictsWith.as_str(), "conflicts-with");
    assert_eq!(EdgeType::parse("about").unwrap(), EdgeType::About);
    // Shape-only, like NodeType: unknown verbs parse; the engine's write
    // boundary rejects verbs the graph's ontology doesn't declare.
    assert_eq!(
        EdgeType::parse("relates_to").unwrap().as_str(),
        "relates_to"
    );
    assert!(EdgeType::parse("").is_err());
}

#[test]
fn add_and_get_node() {
    let s = store();
    let n = s
        .add_node(new_node(NodeType::Decision, "Use Rust", "for the backend"))
        .unwrap();
    assert_eq!(n.node_type, NodeType::Decision);
    assert_eq!(n.valid_from, Some(n.created_at));
    assert!(n.valid_until.is_none());

    let got = s.get_node(&n.id).unwrap().unwrap();
    assert_eq!(got, n);
    assert!(s.get_node("missing").unwrap().is_none());
}

#[test]
fn code_refs_roundtrip() {
    let s = store();
    let mut nn = new_node(NodeType::Anchor, "auth flow", "");
    nn.code_refs = vec!["src/auth".into(), "login handler".into()];
    let n = s.add_node(nn).unwrap();
    let got = s.get_node(&n.id).unwrap().unwrap();
    assert_eq!(got.code_refs, vec!["src/auth", "login handler"]);
}

#[test]
fn update_node_patches_only_given_fields() {
    let s = store();
    let n = s
        .add_node(new_node(NodeType::Problem, "flaky test", "intermittent"))
        .unwrap();
    let patch = NodePatch {
        status: Some(NodeStatus::Resolved),
        ..Default::default()
    };
    let updated = s.update_node(&n.id, patch).unwrap();
    assert_eq!(updated.status, Some(NodeStatus::Resolved));
    assert!(updated.last_seen.is_some(), "update stamps last_seen");
    assert_eq!(updated.title, "flaky test"); // untouched
    assert_eq!(updated.body.as_deref(), Some("intermittent"));
}

#[test]
fn delete_node_cascades_edges() {
    let s = store();
    let a = s.add_node(new_node(NodeType::Decision, "A", "")).unwrap();
    let b = s.add_node(new_node(NodeType::Principle, "B", "")).unwrap();
    s.add_edge(NewEdge {
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

    assert_eq!(s.edges_out(&a.id).unwrap().len(), 1);
    assert_eq!(s.edges_in(&b.id).unwrap().len(), 1);

    assert!(s.delete_node(&a.id).unwrap());
    assert!(s.get_node(&a.id).unwrap().is_none());
    // edge cascaded away even though b still exists
    assert_eq!(s.edges_in(&b.id).unwrap().len(), 0);
    assert!(s.get_node(&b.id).unwrap().is_some());
}

#[test]
fn foreign_key_blocks_dangling_edge() {
    let s = store();
    let a = s.add_node(new_node(NodeType::Decision, "A", "")).unwrap();
    let res = s.add_edge(NewEdge {
        edge_type: EdgeType::Because,
        from_id: a.id.clone(),
        to_id: "nonexistent".into(),
        source: Source::Agent,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    });
    assert!(res.is_err());
}

#[test]
fn fts_search_finds_and_ranks() {
    let s = store();
    s.add_node(new_node(
        NodeType::Decision,
        "Adopt SQLite WAL mode",
        "concurrent reads",
    ))
    .unwrap();
    s.add_node(new_node(
        NodeType::Insight,
        "Vue Flow renders the graph",
        "frontend pane",
    ))
    .unwrap();

    let hits = s.search_fts("sqlite", &[], 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "Adopt SQLite WAL mode");
    assert!(
        hits[0].snippet.contains(crate::SNIPPET_OPEN),
        "snippet should mark the match with the highlight sentinel: {}",
        hits[0].snippet
    );

    // body-only match
    let hits = s.search_fts("frontend", &[], 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_type, NodeType::Insight);
}

#[test]
fn fts_search_type_filter_and_punctuation_safe() {
    let s = store();
    s.add_node(new_node(NodeType::Decision, "graph store", ""))
        .unwrap();
    s.add_node(new_node(NodeType::Insight, "graph pane", ""))
        .unwrap();

    let only_decisions = s.search_fts("graph", &[NodeType::Decision], 8).unwrap();
    assert_eq!(only_decisions.len(), 1);
    assert_eq!(only_decisions[0].node_type, NodeType::Decision);

    // punctuation must not blow up the MATCH parser
    assert!(s.search_fts("graph (store) AND \"x\"", &[], 8).is_ok());
    assert!(s.search_fts("   ", &[], 8).unwrap().is_empty());
}

#[test]
fn fts_reflects_update_and_delete() {
    let s = store();
    let n = s
        .add_node(new_node(NodeType::Decision, "kanban board", ""))
        .unwrap();
    assert_eq!(s.search_fts("kanban", &[], 8).unwrap().len(), 1);

    s.update_node(
        &n.id,
        NodePatch {
            title: Some("scrum board".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(s.search_fts("kanban", &[], 8).unwrap().len(), 0);
    assert_eq!(s.search_fts("scrum", &[], 8).unwrap().len(), 1);

    s.delete_node(&n.id).unwrap();
    assert_eq!(s.search_fts("scrum", &[], 8).unwrap().len(), 0);
}

#[test]
fn list_open_returns_open_problems_and_intents() {
    let s = store();
    let mut p = new_node(NodeType::Problem, "open bug", "");
    p.status = Some(NodeStatus::Open);
    let open = s.add_node(p).unwrap();

    let mut p2 = new_node(NodeType::Problem, "fixed bug", "");
    p2.status = Some(NodeStatus::Resolved);
    s.add_node(p2).unwrap();

    let mut i = new_node(NodeType::Intent, "do later", "");
    i.status = Some(NodeStatus::Open);
    s.add_node(i).unwrap();

    let open_nodes = s.list_open(&[]).unwrap();
    let ids: Vec<&str> = open_nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(open_nodes.len(), 2);
    assert!(ids.contains(&open.id.as_str()));

    let only_problems = s.list_open(&[NodeType::Problem]).unwrap();
    assert_eq!(only_problems.len(), 1);
}

#[test]
fn redaction_applies_on_write() {
    let s = store();
    let n = s
        .add_node(new_node(
            NodeType::Caution,
            "leak AKIAIOSFODNN7EXAMPLE",
            "token=abc123secretvalue99",
        ))
        .unwrap();
    let got = s.get_node(&n.id).unwrap().unwrap();
    assert!(!got.title.contains("AKIA"));
    assert!(got.title.contains("[REDACTED]"));
    assert!(!got.body.unwrap().contains("abc123secretvalue99"));
}
