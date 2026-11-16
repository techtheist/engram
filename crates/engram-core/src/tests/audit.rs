//! The audit journal.

use super::*;

#[test]
fn audit_journals_node_lifecycle_with_context() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Decision, "Use Rust", "for the backend"))
        .unwrap();
    e.update_node(
        &n.id,
        NodePatch {
            title: Some("Use Rust everywhere".into()),
            ..Default::default()
        },
    )
    .unwrap();
    e.approve(&n.id).unwrap();
    e.delete_node(&n.id).unwrap();

    let page = e.audit_log(None, None, 10).unwrap();
    assert_eq!(page.total, 4);
    let actions: Vec<&str> = page.entries.iter().map(|x| x.action.as_str()).collect();
    assert_eq!(actions, ["deleted", "approved", "updated", "created"]);

    let created = page.entries.last().unwrap();
    assert_eq!(created.entity, "node");
    assert_eq!(created.entity_id, n.id);
    assert_eq!(created.title.as_deref(), Some("Use Rust"));
    assert!(created.before.is_none());
    assert!(created.after.is_some());
    assert_eq!(created.origin, "library");
    assert_eq!(created.session_id.as_deref(), Some("s1"));
    assert!(created.cwd.is_some());
    assert!(created.pid.is_some());
    assert!(created.version.is_some());

    let updated = &page.entries[2];
    assert_eq!(updated.before.as_ref().unwrap()["title"], "Use Rust");
    assert_eq!(
        updated.after.as_ref().unwrap()["title"],
        "Use Rust everywhere"
    );

    let deleted = &page.entries[0];
    assert!(deleted.after.is_none());
    assert_eq!(deleted.title.as_deref(), Some("Use Rust everywhere"));
}

#[test]
fn audit_journals_edges_with_sentence_labels() {
    let e = engine();
    let a = e
        .add_node(new_node(NodeType::Decision, "keep sqlite", ""))
        .unwrap();
    let b = e
        .add_node(new_node(NodeType::Principle, "local first", ""))
        .unwrap();
    let edge = e
        .add_edge(NewEdge {
            edge_type: EdgeType::Because,
            from_id: a.id.clone(),
            to_id: b.id.clone(),
            source: Source::Agent,
            note: None,
            confidence: None,
            strength: None,
            status: None,
        })
        .unwrap();
    e.update_edge(
        &edge.id,
        EdgePatch {
            note: Some("checked".into()),
            ..Default::default()
        },
    )
    .unwrap();
    e.delete_edge(&edge.id).unwrap();

    // entity_id narrows the journal to this edge's history.
    let page = e.audit_log(None, Some(&edge.id), 10).unwrap();
    assert_eq!(page.total, 3);
    let actions: Vec<&str> = page.entries.iter().map(|x| x.action.as_str()).collect();
    assert_eq!(actions, ["deleted", "updated", "created"]);
    for entry in &page.entries {
        assert_eq!(entry.entity, "edge");
        let label = entry.title.as_deref().unwrap();
        assert!(
            label.contains("keep sqlite")
                && label.contains("because")
                && label.contains("local first"),
            "sentence-shaped label, got: {label}"
        );
    }
}

#[test]
fn audit_logs_supersede_and_decay_as_archived() {
    let e = engine();
    let superseded = e
        .add_node(new_node(NodeType::Decision, "old way", ""))
        .unwrap();
    e.update_node(
        &superseded.id,
        NodePatch {
            valid_until: Some(now()),
            ..Default::default()
        },
    )
    .unwrap();
    let page = e.audit_log(None, Some(&superseded.id), 1).unwrap();
    assert_eq!(page.entries[0].action, "archived");

    let doomed = e
        .add_node(episodic(NodeType::Insight, "temp workaround"))
        .unwrap();
    backdate(&e, &doomed.id, 100);
    e.decay(policy::DECAY_TTL_DAYS, false).unwrap();
    let page = e.audit_log(None, Some(&doomed.id), 1).unwrap();
    assert_eq!(page.entries[0].action, "archived");
    assert!(page.entries[0].before.is_some());
    assert!(page.entries[0].after.is_some());
}

#[test]
fn audit_page_keyset_pagination() {
    let e = engine();
    for i in 0..5 {
        e.add_node(new_node(NodeType::Decision, &format!("decision {i}"), ""))
            .unwrap();
    }
    let p1 = e.audit_log(None, None, 2).unwrap();
    assert_eq!(p1.total, 5);
    assert_eq!(p1.entries.len(), 2);
    assert!(p1.entries[0].seq > p1.entries[1].seq, "newest first");

    let cursor = p1.entries.last().unwrap().seq;
    let p2 = e.audit_log(Some(cursor), None, 10).unwrap();
    assert_eq!(p2.entries.len(), 3, "the rest, no overlap");
    assert!(p2.entries.iter().all(|x| x.seq < cursor));
    assert_eq!(p2.total, 5, "total is page-independent");
}

#[test]
fn audit_origin_stamp_and_session_fallback() {
    let mut e = engine();
    e.set_audit_origin(AuditOrigin::mcp("mcp-test".into()));

    let mut anonymous = new_node(NodeType::Insight, "origin check", "");
    anonymous.session_id = None;
    let n = e.add_node(anonymous).unwrap();
    let entry = &e.audit_log(None, Some(&n.id), 1).unwrap().entries[0];
    assert_eq!(entry.origin, "mcp");
    assert_eq!(
        entry.session_id.as_deref(),
        Some("mcp-test"),
        "falls back to the origin's session"
    );

    // A node that carries its own session id wins over the origin's.
    let n2 = e
        .add_node(new_node(NodeType::Insight, "session check", ""))
        .unwrap();
    let entry = &e.audit_log(None, Some(&n2.id), 1).unwrap().entries[0];
    assert_eq!(entry.session_id.as_deref(), Some("s1"));

    // But only for its creation: a later mutation from another session is
    // that session's action, not the creator's.
    e.set_audit_origin(AuditOrigin::mcp("mcp-later".into()));
    e.update_node(
        &n2.id,
        NodePatch {
            title: Some("session recheck".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let entry = &e.audit_log(None, Some(&n2.id), 1).unwrap().entries[0];
    assert_eq!(entry.action, "updated");
    assert_eq!(
        entry.session_id.as_deref(),
        Some("mcp-later"),
        "updates attribute the acting session, not the node's creator"
    );
}

#[test]
fn audit_import_writes_one_summary_row() {
    let e = engine();
    e.add_node(new_node(NodeType::Decision, "exported knowledge", ""))
        .unwrap();
    let snapshot = e.export().unwrap();

    let target = engine();
    target.import(snapshot).unwrap();
    let page = e.audit_log(None, None, 100).unwrap();
    let _ = page; // source journal untouched by the target's import
    let page = target.audit_log(None, None, 100).unwrap();
    let imported: Vec<_> = page
        .entries
        .iter()
        .filter(|x| x.action == "imported")
        .collect();
    assert_eq!(imported.len(), 1, "one summary row, not one per entity");
    assert_eq!(imported[0].entity, "graph");
    assert_eq!(imported[0].title.as_deref(), Some("1 nodes / 0 edges"));
}

#[test]
fn search_reaches_code_refs_and_tags() {
    let e = engine();
    let mut with_refs = new_node(NodeType::Decision, "Trust curves live in one module", "");
    with_refs.code_refs = vec!["crates/engram-core/src/policy.rs".into()];
    with_refs.tags = vec!["retrieval".into()];
    e.add_node(with_refs).unwrap();
    e.add_node(new_node(
        NodeType::Decision,
        "Unrelated pane layout choice",
        "",
    ))
    .unwrap();

    let hits = e.search("policy", &[], 5).unwrap();
    assert_eq!(
        hits.first().map(|h| h.title.as_str()),
        Some("Trust curves live in one module"),
        "a code_ref path token reaches the node"
    );
    let hits = e.search("retrieval", &[], 5).unwrap();
    assert!(
        hits.iter()
            .any(|h| h.title == "Trust curves live in one module"),
        "a tag token reaches the node"
    );
}
