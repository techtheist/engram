//! Supersession, timelines and code-ref drift.

use super::*;

#[test]
fn a_replaces_edge_retires_the_node_it_replaces() {
    let e = engine();
    let old = e
        .add_node(new_node(NodeType::Decision, "deploy via ftp", ""))
        .unwrap();
    let new = e
        .add_node(new_node(NodeType::Decision, "deploy via ssh", ""))
        .unwrap();
    let mk = |verb: EdgeType, from: &str, to: &str| NewEdge {
        edge_type: verb,
        from_id: from.to_string(),
        to_id: to.to_string(),
        source: Source::Agent,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    };

    // A plain `link` — no suspect verdict in sight — still retires the older.
    e.add_edge(mk(EdgeType::Replaces, &new.id, &old.id))
        .unwrap();
    assert!(
        e.get_node(&old.id).unwrap().unwrap().valid_until.is_some(),
        "the replaced node leaves the canon"
    );
    assert!(
        e.get_node(&new.id).unwrap().unwrap().valid_until.is_none(),
        "the replacement is untouched"
    );

    // Retyping INTO the supersession verb retires just the same.
    let a = e
        .add_node(new_node(NodeType::Insight, "bundler is webpack", ""))
        .unwrap();
    let b = e
        .add_node(new_node(NodeType::Insight, "bundler is vite", ""))
        .unwrap();
    let edge = e.add_edge(mk(EdgeType::BuildsOn, &b.id, &a.id)).unwrap();
    assert!(e.get_node(&a.id).unwrap().unwrap().valid_until.is_none());
    e.update_edge(
        &edge.id,
        EdgePatch {
            edge_type: Some(EdgeType::Replaces),
            ..EdgePatch::default()
        },
    )
    .unwrap();
    assert!(e.get_node(&a.id).unwrap().unwrap().valid_until.is_some());

    // A pin is the user's "never fade": the automatic path steps aside.
    let pinned = e
        .add_node(new_node(NodeType::Principle, "never log secrets", ""))
        .unwrap();
    let later = e
        .add_node(new_node(NodeType::Principle, "redact secrets in logs", ""))
        .unwrap();
    e.set_trust_override(&pinned.id, Some(1.0)).unwrap();
    e.add_edge(mk(EdgeType::Replaces, &later.id, &pinned.id))
        .unwrap();
    assert!(
        e.get_node(&pinned.id)
            .unwrap()
            .unwrap()
            .valid_until
            .is_none(),
        "pinned knowledge is never auto-archived"
    );
}

#[test]
fn validation_retires_nodes_replaced_before_the_rule_existed() {
    let e = engine();
    let old = e
        .add_node(new_node(NodeType::Decision, "bundle with rollup", ""))
        .unwrap();
    let new = e
        .add_node(new_node(NodeType::Decision, "bundle with vite", ""))
        .unwrap();
    // Straight to the store, bypassing the engine hook: this is exactly the
    // shape of a graph written before supersession retired its target.
    e.store()
        .add_edge(NewEdge {
            edge_type: EdgeType::Replaces,
            from_id: new.id.clone(),
            to_id: old.id.clone(),
            source: Source::Agent,
            note: None,
            confidence: None,
            strength: None,
            status: None,
        })
        .unwrap();
    assert!(e.get_node(&old.id).unwrap().unwrap().valid_until.is_none());

    let note = e.validate_graph().unwrap();
    assert!(note.contains("1 superseded"), "got: {note}");
    assert!(e.get_node(&old.id).unwrap().unwrap().valid_until.is_some());
    // Idempotent: the second pass finds nothing left to retire.
    assert!(e.validate_graph().unwrap().contains("0 superseded"));
}

// ---- timeline -------------------------------------------------------

#[test]
fn timeline_walks_the_replaces_chain_oldest_first() {
    let e = engine();
    let a = e
        .add_node(new_node(NodeType::Decision, "auth v1", ""))
        .unwrap();
    let b = e
        .add_node(new_node(NodeType::Decision, "auth v2", ""))
        .unwrap();
    let c = e
        .add_node(new_node(NodeType::Decision, "auth v3", ""))
        .unwrap();
    let mk = |from: &str, to: &str, note: &str| NewEdge {
        edge_type: EdgeType::Replaces,
        from_id: from.to_string(),
        to_id: to.to_string(),
        source: Source::Agent,
        note: Some(note.to_string()),
        confidence: None,
        strength: None,
        status: None,
    };
    e.add_edge(mk(&b.id, &a.id, "sessions over JWT")).unwrap();
    e.add_edge(mk(&c.id, &b.id, "moved to OAuth")).unwrap();

    // Asking from the middle still yields the whole chain, oldest first.
    let chain = e.timeline(&b.id).unwrap();
    assert_eq!(
        chain.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
        ["auth v1", "auth v2", "auth v3"]
    );
    assert_eq!(chain[0].replaced_note.as_deref(), Some("sessions over JWT"));
    assert_eq!(chain[1].replaced_note.as_deref(), Some("moved to OAuth"));
    assert_eq!(chain[2].replaced_note, None);

    // A chainless node is a single-entry timeline; unknown ids are NotFound.
    let lone = e
        .add_node(new_node(NodeType::Insight, "loner", ""))
        .unwrap();
    assert_eq!(e.timeline(&lone.id).unwrap().len(), 1);
    assert!(e.timeline("missing").is_err());
}

// ---- verified code refs --------------------------------------------

#[test]
fn scan_code_refs_flags_missing_paths_only() {
    let e = engine();
    let root = std::env::temp_dir().join("engram-drift-test");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/real.rs"), "x").unwrap();

    // Healthy: an existing path plus a free-text label (never checkable).
    e.add_node(NewNode {
        code_refs: vec!["src/real.rs".into(), "auth flow".into()],
        ..new_node(NodeType::Decision, "refs intact", "")
    })
    .unwrap();

    // Drifted: one existing ref, one that points nowhere.
    let drifted = e
        .add_node(NewNode {
            code_refs: vec!["src/real.rs".into(), "src/gone.rs".into()],
            ..new_node(NodeType::Caution, "refs moved", "")
        })
        .unwrap();

    // Archived nodes are history — they never drift.
    let archived = e
        .add_node(NewNode {
            code_refs: vec!["src/gone.rs".into()],
            ..new_node(NodeType::Insight, "archived refs", "")
        })
        .unwrap();
    e.update_node(
        &archived.id,
        NodePatch {
            valid_until: Some(now()),
            ..Default::default()
        },
    )
    .unwrap();

    let report = e.scan_code_refs(&root).unwrap();
    assert_eq!(report.len(), 1, "only the active node with a missing path");
    assert_eq!(report[0].id, drifted.id);
    assert_eq!(report[0].missing, vec!["src/gone.rs".to_string()]);
}
