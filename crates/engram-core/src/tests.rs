use crate::*;

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn new_node(t: NodeType, title: &str, body: &str) -> NewNode {
    NewNode {
        node_type: t,
        title: title.to_string(),
        body: Some(body.to_string()),
        durability: Durability::Stable,
        source: Source::Claude,
        session_id: Some("s1".to_string()),
        status: None,
        code_refs: vec![],
    }
}

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
    ] {
        assert_eq!(NodeType::parse(t.as_str()).unwrap(), t);
    }
    assert!(NodeType::parse("Concept").is_err());
}

#[test]
fn edge_type_strings() {
    assert_eq!(EdgeType::BuildsOn.as_str(), "builds-on");
    assert_eq!(EdgeType::ConflictsWith.as_str(), "conflicts-with");
    assert_eq!(EdgeType::parse("about").unwrap(), EdgeType::About);
    assert!(EdgeType::parse("relates_to").is_err());
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
        source: Source::Claude,
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
        source: Source::Claude,
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
        hits[0].snippet.contains('['),
        "snippet should mark the match: {}",
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

fn engine() -> Engine {
    Engine::new(
        Store::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    )
}

#[test]
fn vector_knn_finds_nearest() {
    let s = store();
    let a = s
        .add_node(new_node(NodeType::Decision, "alpha", ""))
        .unwrap();
    let b = s
        .add_node(new_node(NodeType::Decision, "beta", ""))
        .unwrap();
    let e = FakeEmbedder::default();
    s.upsert_embedding(&a.id, &e.embed_one("alpha").unwrap())
        .unwrap();
    s.upsert_embedding(&b.id, &e.embed_one("beta").unwrap())
        .unwrap();

    let q = e.embed_one("alpha").unwrap();
    let hits = s.search_hybrid("", Some(&q), &[], 2).unwrap();
    assert_eq!(
        hits.first().unwrap().id,
        a.id,
        "nearest vector should rank first"
    );
}

#[test]
fn engine_embeds_on_add_and_hybrid_search_works() {
    let e = engine();
    e.add_node(new_node(
        NodeType::Decision,
        "Adopt SQLite WAL mode",
        "concurrent reads",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Insight,
        "Vue Flow renders the graph",
        "the pane",
    ))
    .unwrap();

    // keyword path
    let hits = e.search("sqlite", &[], 5).unwrap();
    assert_eq!(hits[0].title, "Adopt SQLite WAL mode");

    // vector recall: a query with no exact token match still returns candidates
    let hits = e.search("database engine choices", &[], 5).unwrap();
    assert!(
        !hits.is_empty(),
        "vector half should surface candidates without an FTS match"
    );
}

#[test]
fn engine_reembeds_on_text_update() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Decision, "kanban board", ""))
        .unwrap();
    assert_eq!(e.search("kanban", &[], 5).unwrap()[0].id, n.id);

    e.update_node(
        &n.id,
        NodePatch {
            title: Some("scrum board".into()),
            ..Default::default()
        },
    )
    .unwrap();
    // FTS reflects the new title
    let hits = e.search("scrum", &[], 5).unwrap();
    assert_eq!(hits[0].id, n.id);
}

#[test]
fn trust_boost_prefers_user_source() {
    let e = engine();
    let mut claude = new_node(NodeType::Decision, "shared title here", "shared body text");
    claude.source = Source::Claude;
    claude.durability = Durability::Episodic;
    let c = e.add_node(claude).unwrap();

    let mut user = new_node(NodeType::Decision, "shared title here", "shared body text");
    user.source = Source::User;
    user.durability = Durability::Episodic;
    let u = e.add_node(user).unwrap();

    let hits = e.search("shared title", &[], 5).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    let upos = ids.iter().position(|&i| i == u.id).unwrap();
    let cpos = ids.iter().position(|&i| i == c.id).unwrap();
    assert!(
        upos < cpos,
        "user-sourced node should outrank identical claude node"
    );
}

#[test]
fn deleting_node_removes_embedding() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Decision, "temporary", "to be deleted"))
        .unwrap();
    assert!(!e.search("temporary", &[], 5).unwrap().is_empty());
    assert!(e.delete_node(&n.id).unwrap());
    let hits = e.search("temporary", &[], 5).unwrap();
    assert!(hits.iter().all(|h| h.id != n.id));
}

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
        source: Source::Claude,
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

#[test]
fn trust_curves_follow_the_three_timestamps() {
    use crate::policy::{self, trust};
    let day = 24 * 60 * 60;
    let t0 = 1_000_000i64;

    // created-only: 50% now, linear to the floor at half a year
    assert!((trust(t0, None, None, t0) - policy::TRUST_UNSEEN_START).abs() < 1e-9);
    let half_window = t0 + policy::PROVISIONAL_TRUST_WINDOW_SECS / 2;
    let mid = trust(t0, None, None, half_window);
    assert!((mid - (policy::TRUST_UNSEEN_START + policy::TRUST_FLOOR) / 2.0).abs() < 1e-6);
    assert!(
        (trust(
            t0,
            None,
            None,
            t0 + policy::PROVISIONAL_TRUST_WINDOW_SECS + day
        ) - policy::TRUST_FLOOR)
            .abs()
            < 1e-9
    );

    // seen: restarts at 60% from last_seen, beats created-only
    let seen = trust(t0, Some(t0 + 100 * day), None, t0 + 100 * day);
    assert!((seen - policy::TRUST_SEEN_START).abs() < 1e-9);
    assert!(seen > trust(t0, None, None, t0 + 100 * day));

    // approved: 100% at approval, floor 20% past a year, wins over last_seen
    assert!((trust(t0, Some(t0), Some(t0), t0) - policy::TRUST_APPROVED_START).abs() < 1e-9);
    let old_approval = trust(
        t0,
        None,
        Some(t0),
        t0 + policy::APPROVED_TRUST_WINDOW_SECS + day,
    );
    assert!((old_approval - policy::TRUST_APPROVED_FLOOR).abs() < 1e-9);

    // staleness threshold
    assert!(policy::is_stale(policy::STALE_TRUST - 0.01));
    assert!(!policy::is_stale(policy::STALE_TRUST));
}

#[test]
fn user_nodes_are_approved_on_creation_and_approve_restores_trust() {
    let e = engine();
    let mut u = new_node(NodeType::Principle, "user truth", "");
    u.source = Source::User;
    let user = e.add_node(u).unwrap();
    assert!(
        user.approved_at.is_some(),
        "user knowledge approved by construction"
    );
    assert!(user.trust > 0.99);

    let claude = e
        .add_node(new_node(NodeType::Insight, "hunch", ""))
        .unwrap();
    assert!(claude.approved_at.is_none());
    assert!((claude.trust - crate::policy::TRUST_UNSEEN_START).abs() < 1e-6);

    let approved = e.approve(&claude.id).unwrap();
    assert!(approved.approved_at.is_some());
    assert!(approved.trust > 0.99);
    assert!(!approved.stale);
}

#[test]
fn search_and_reconfirm_stamp_last_seen() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Decision, "sqlite storage decision", ""))
        .unwrap();
    assert!(n.last_seen.is_none(), "fresh node has never been seen");

    let hits = e.search("sqlite storage decision", &[], 5).unwrap();
    assert!(hits.iter().any(|h| h.id == n.id));
    let seen = e.get_node(&n.id).unwrap().unwrap();
    assert!(
        seen.last_seen.is_some(),
        "search surfacing stamps last_seen"
    );

    let reconfirmed = e.reconfirm(&n.id).unwrap();
    assert!(reconfirmed.last_seen >= seen.last_seen);
}

#[test]
fn recency_factor_prefers_newer() {
    use crate::store::recency_factor_for_tests as rf;
    let day = 24 * 60 * 60;
    assert!(rf(0) > rf(30 * day), "newer beats older");
    assert!(rf(0) <= 1.0 + crate::policy::SEARCH_RECENCY_BOOST + 1e-9);
    assert!(rf(3650 * day) >= 1.0, "bonus never penalizes below 1.0");
    // Half-life: at 30 days the bonus is half the ceiling.
    let bonus_at_half_life = rf(crate::policy::SEARCH_RECENCY_HALF_LIFE_SECS) - 1.0;
    assert!((bonus_at_half_life - crate::policy::SEARCH_RECENCY_BOOST / 2.0).abs() < 1e-9);
}

#[test]
fn node_json_uses_canonical_strings() {
    let s = store();
    let n = s
        .add_node(new_node(NodeType::Caution, "watch out", ""))
        .unwrap();
    let v: serde_json::Value = serde_json::to_value(&n).unwrap();
    assert_eq!(v["type"], "Caution");
    assert_eq!(v["durability"], "stable");
    assert_eq!(v["source"], "claude");
}

// ---- retrieval upgrade / brief / checked writes / edge repair -------------

fn link(s: &Store, t: EdgeType, from: &str, to: &str) -> Edge {
    s.add_edge(NewEdge {
        edge_type: t,
        from_id: from.to_string(),
        to_id: to.to_string(),
        source: Source::Claude,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap()
}

#[test]
fn fts_or_semantics_keep_recall_on_multiword_queries() {
    let e = engine();
    e.add_node(new_node(NodeType::Decision, "Adopt SQLite WAL mode", ""))
        .unwrap();
    // one matching term + one garbage term: AND would return nothing
    let hits = e.search("sqlite zzznonexistent", &[], 5).unwrap();
    assert_eq!(hits.first().unwrap().title, "Adopt SQLite WAL mode");
}

#[test]
fn unrelated_query_returns_no_hits() {
    let e = engine();
    e.add_node(new_node(NodeType::Decision, "Adopt SQLite WAL mode", ""))
        .unwrap();
    e.add_node(new_node(NodeType::Caution, "never store secrets", ""))
        .unwrap();
    // no keyword overlap, no byte overlap for the fake embedder: below every cutoff
    let hits = e.search("zzzz qqqq xxxx", &[], 5).unwrap();
    assert!(
        hits.is_empty(),
        "unrelated query must return nothing, got {hits:?}"
    );
}

#[test]
fn search_hits_carry_conflict_first_neighbors() {
    let e = engine();
    let a = e
        .add_node(new_node(NodeType::Decision, "store data in sqlite", ""))
        .unwrap();
    let b = e
        .add_node(new_node(NodeType::Insight, "sqlite is too slow here", ""))
        .unwrap();
    let anchor = e
        .add_node(new_node(NodeType::Anchor, "storage layer", ""))
        .unwrap();
    link(e.store(), EdgeType::About, &a.id, &anchor.id);
    link(e.store(), EdgeType::ConflictsWith, &b.id, &a.id);

    let hits = e.search("store data in sqlite", &[], 1).unwrap();
    let hit = hits.first().unwrap();
    assert_eq!(hit.id, a.id);
    assert!(!hit.neighbors.is_empty(), "1-hop neighbors ride along");
    assert_eq!(
        hit.neighbors[0].edge_type,
        EdgeType::ConflictsWith,
        "conflicts order first"
    );
    assert_eq!(hit.neighbors[0].id, b.id);
    assert_eq!(hit.neighbors[0].direction, "in");
}

#[test]
fn add_node_checked_short_circuits_same_type_duplicates() {
    let e = engine();
    let first = match e
        .add_node_checked(new_node(
            NodeType::Decision,
            "Adopt SQLite WAL mode",
            "concurrent reads",
        ))
        .unwrap()
    {
        WriteOutcome::Created { node, .. } => node,
        WriteOutcome::Matched { .. } => panic!("first write must create"),
    };
    match e
        .add_node_checked(new_node(
            NodeType::Decision,
            "Adopt SQLite WAL mode",
            "concurrent reads",
        ))
        .unwrap()
    {
        WriteOutcome::Matched { node, similarity } => {
            assert_eq!(node.id, first.id);
            assert!(similarity >= crate::policy::DUPLICATE_SIMILARITY);
        }
        WriteOutcome::Created { .. } => panic!("identical note must match, not duplicate"),
    }
    // a different type with the same text is not a duplicate
    match e
        .add_node_checked(new_node(
            NodeType::Insight,
            "Adopt SQLite WAL mode",
            "concurrent reads",
        ))
        .unwrap()
    {
        WriteOutcome::Created { .. } => {}
        WriteOutcome::Matched { .. } => panic!("cross-type text overlap must not match"),
    }
}

#[test]
fn writes_warn_near_conflicted_and_superseded_nodes() {
    let e = engine();
    let a = e
        .add_node(new_node(
            NodeType::Decision,
            "cache results in redis",
            "for speed",
        ))
        .unwrap();
    let b = e
        .add_node(new_node(
            NodeType::Insight,
            "redis cache is stale-prone",
            "",
        ))
        .unwrap();
    link(e.store(), EdgeType::ConflictsWith, &b.id, &a.id);

    let outcome = e
        .add_node_checked(new_node(
            NodeType::Caution,
            "cache results in redis",
            "for speed",
        ))
        .unwrap();
    let WriteOutcome::Created { warnings, .. } = outcome else {
        panic!("different type must create")
    };
    assert!(
        warnings
            .iter()
            .any(|w| w.id == a.id && w.reason == "in-active-conflict"),
        "writing near a conflicted node must warn: {warnings:?}"
    );

    // supersede a node, then write near it
    e.update_node(
        &a.id,
        NodePatch {
            valid_until: Some(now()),
            ..Default::default()
        },
    )
    .unwrap();
    // a type with no prior nodes, so the fake embedder can't same-type match
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Resolution,
            "cache results in redis",
            "for speed",
        ))
        .unwrap();
    let WriteOutcome::Created { warnings, .. } = outcome else {
        panic!("superseded node must not block creation")
    };
    assert!(
        warnings
            .iter()
            .any(|w| w.id == a.id && w.reason == "superseded"),
        "writing near a superseded node must warn: {warnings:?}"
    );
}

#[test]
fn update_edge_and_delete_edge() {
    let s = store();
    let a = s.add_node(new_node(NodeType::Decision, "A", "")).unwrap();
    let b = s.add_node(new_node(NodeType::Decision, "B", "")).unwrap();
    let edge = link(&s, EdgeType::ConflictsWith, &a.id, &b.id);

    let updated = s
        .update_edge(
            &edge.id,
            EdgePatch {
                status: Some(EdgeStatus::Resolved),
                note: Some("settled".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.status, Some(EdgeStatus::Resolved));
    assert_eq!(updated.note.as_deref(), Some("settled"));
    assert!(!s.has_active_conflict(&a.id).unwrap());

    assert!(s.delete_edge(&edge.id).unwrap());
    assert!(s.get_edge(&edge.id).unwrap().is_none());
    assert!(!s.delete_edge(&edge.id).unwrap());
}

#[test]
fn brief_on_empty_graph_teaches_cold_start_seeding() {
    let e = engine();
    let brief = e.brief(12000).unwrap();
    assert!(
        brief.contains("cold start"),
        "empty brief must instruct seeding: {brief}"
    );

    e.add_node(new_node(NodeType::Decision, "backend in rust", ""))
        .unwrap();
    let brief = e.brief(12000).unwrap();
    assert!(
        !brief.contains("cold start"),
        "populated brief must not mention cold start"
    );
}

#[test]
fn brief_digests_the_canon_and_respects_budget() {
    let e = engine();
    let p = e
        .add_node(new_node(NodeType::Principle, "local first always", ""))
        .unwrap();
    let d = e
        .add_node(new_node(
            NodeType::Decision,
            "backend in rust",
            "rmcp and rusqlite",
        ))
        .unwrap();
    let mut prob = new_node(NodeType::Problem, "flaky embedding download", "");
    prob.status = Some(NodeStatus::Open);
    prob.durability = Durability::Episodic;
    let prob = e.add_node(prob).unwrap();
    let i = e
        .add_node(new_node(NodeType::Insight, "graph must stay small", ""))
        .unwrap();
    link(e.store(), EdgeType::ConflictsWith, &i.id, &d.id);

    // a node whose type has no canon section — surfaces only under "Recently added"
    let extra = e
        .add_node(new_node(NodeType::Resolution, "wired the sse channel", ""))
        .unwrap();

    let text = e.brief(6000).unwrap();
    assert!(text.contains("## Unresolved conflicts"));
    assert!(text.contains("## Open problems & intents"));
    assert!(text.contains("local first always"));
    assert!(text.contains("backend in rust"));
    assert!(text.contains(&prob.title));
    // ids appear only in "Recently added" — everything else is re-findable via search
    assert!(!text.contains(&p.id), "canon sections must not carry ids");
    assert!(!text.contains(&prob.id), "worklist must not carry ids");
    assert!(
        text.contains(&extra.id),
        "recently-added lines keep their id"
    );

    let small = e.brief(120).unwrap();
    assert!(
        small.len() <= 120,
        "budget is a hard cap, got {}",
        small.len()
    );
}

#[test]
fn brief_inclusion_stamps_last_seen() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Principle, "keep it minimal", ""))
        .unwrap();
    assert!(n.last_seen.is_none());

    e.brief(6000).unwrap();
    let seen = e.get_node(&n.id).unwrap().unwrap();
    assert!(
        seen.last_seen.is_some(),
        "brief inclusion counts as being surfaced"
    );
}

#[test]
fn legacy_uuid_ids_shrink_with_edges_and_embeddings_intact() {
    let s = store();
    let uuid_a = "aaaaaaaa-1111-2222-3333-444444444444".to_string();
    let uuid_b = "bbbbbbbb-1111-2222-3333-444444444444".to_string();
    let base = Node {
        id: uuid_a.clone(),
        node_type: NodeType::Decision,
        title: "legacy node a".into(),
        body: None,
        durability: Durability::Stable,
        source: Source::Claude,
        session_id: None,
        created_at: 1,
        valid_from: Some(1),
        valid_until: None,
        status: None,
        last_seen: None,
        approved_at: None,
        trust: 0.0,
        stale: false,
        code_refs: vec![],
    };
    let b_node = Node {
        id: uuid_b.clone(),
        title: "legacy node b".into(),
        ..base.clone()
    };
    let edge = Edge {
        id: "cccccccc-1111-2222-3333-444444444444".into(),
        edge_type: EdgeType::Because,
        from_id: uuid_a.clone(),
        to_id: uuid_b.clone(),
        source: Source::Claude,
        created_at: 1,
        confidence: None,
        strength: None,
        note: None,
        valid_from: None,
        valid_until: None,
        status: None,
    };
    s.import_raw(&[base, b_node], &[edge]).unwrap();
    let emb = FakeEmbedder::default().embed_one("legacy node a").unwrap();
    s.upsert_embedding(&uuid_a, &emb).unwrap();

    s.shorten_legacy_ids().unwrap();

    let nodes = s.all_nodes().unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|n| n.id.len() == 12), "{nodes:?}");
    let edges = s.all_edges().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].id.len(), 12);
    let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    assert!(
        ids.contains(&edges[0].from_id.as_str()),
        "endpoints rewritten"
    );
    assert!(ids.contains(&edges[0].to_id.as_str()));

    // the moved embedding still resolves to the renamed node
    let hits = s.search_vec(&emb, 1).unwrap();
    let a_new = nodes.iter().find(|n| n.title == "legacy node a").unwrap();
    assert_eq!(hits[0].0, a_new.id);

    // idempotent
    s.shorten_legacy_ids().unwrap();
    assert_eq!(s.all_nodes().unwrap().len(), 2);
}

// ---- conflict scan (PLAN §7) ------------------------------------------------

fn backdate(e: &Engine, id: &str, days: i64) {
    let ts = now() - days * 24 * 60 * 60;
    e.store()
        .conn()
        .execute(
            "UPDATE nodes SET created_at=?1, valid_from=?1 WHERE id=?2",
            rusqlite::params![ts, id],
        )
        .unwrap();
}

#[test]
fn scan_queues_unlinked_lookalikes_once() {
    let e = engine();
    let a = e
        .add_node(new_node(
            NodeType::Decision,
            "cache invalidation policy",
            "ttl based",
        ))
        .unwrap();
    let b = e
        .add_node(new_node(
            NodeType::Insight,
            "cache invalidation policy",
            "ttl based",
        ))
        .unwrap();

    // add_node (unchecked) records nothing; the sweep finds the pair once.
    assert_eq!(e.scan_conflicts().unwrap(), 1);
    let pending = e.suspects().unwrap();
    assert_eq!(pending.len(), 1);
    let s = &pending[0];
    assert!(s.similarity >= policy::CONFLICT_SUSPECT_SIMILARITY);
    // Same-second creations make "newer" ambiguous — just require the pair.
    let pair = [s.a.id.as_str(), s.b.id.as_str()];
    assert!(pair.contains(&a.id.as_str()) && pair.contains(&b.id.as_str()));

    // a raised pair is never re-raised
    assert_eq!(e.scan_conflicts().unwrap(), 0);
}

#[test]
fn checked_write_queues_suspects_automatically() {
    let e = engine();
    e.add_node(new_node(
        NodeType::Decision,
        "retry with exponential backoff",
        "",
    ))
    .unwrap();
    // Different type dodges the same-type duplicate short-circuit but still
    // lands within suspect range.
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Caution,
            "retry with exponential backoff",
            "",
        ))
        .unwrap();
    assert!(matches!(outcome, WriteOutcome::Created { .. }));
    assert_eq!(e.suspects().unwrap().len(), 1);
}

#[test]
fn resolve_conflict_creates_edge_and_suppresses_pair() {
    let e = engine();
    e.add_node(new_node(NodeType::Decision, "store sessions in redis", ""))
        .unwrap();
    e.add_node(new_node(NodeType::Decision, "store sessions in redis!", ""))
        .unwrap();
    e.scan_conflicts().unwrap();
    let s = e.suspects().unwrap().remove(0);

    let edge = e
        .resolve_suspect(&s.id, SuspectVerdict::Conflict, Source::User)
        .unwrap()
        .expect("edge created");
    assert_eq!(edge.edge_type, EdgeType::ConflictsWith);
    assert_eq!(
        (edge.from_id.as_str(), edge.to_id.as_str()),
        (s.a.id.as_str(), s.b.id.as_str())
    );
    assert!(e.suspects().unwrap().is_empty());
    // judged + now linked: the sweep stays quiet
    assert_eq!(e.scan_conflicts().unwrap(), 0);
    // idempotent on a judged suspect
    assert!(
        e.resolve_suspect(&s.id, SuspectVerdict::Dismiss, Source::User)
            .unwrap()
            .is_none()
    );
}

#[test]
fn resolve_replaces_archives_the_older_node() {
    let e = engine();
    let old = e
        .add_node(new_node(NodeType::Decision, "deploy via ftp upload", ""))
        .unwrap();
    backdate(&e, &old.id, 10);
    e.add_node(new_node(NodeType::Decision, "deploy via ftp uploads", ""))
        .unwrap();
    e.scan_conflicts().unwrap();
    let s = e.suspects().unwrap().remove(0);
    assert_eq!(s.b.id, old.id);

    let edge = e
        .resolve_suspect(&s.id, SuspectVerdict::Replaces, Source::Claude)
        .unwrap()
        .unwrap();
    assert_eq!(edge.edge_type, EdgeType::Replaces);
    let archived = e.get_node(&old.id).unwrap().unwrap();
    assert!(archived.valid_until.is_some(), "older node is superseded");
}

#[test]
fn dismissed_pairs_stay_dismissed() {
    let e = engine();
    e.add_node(new_node(NodeType::Insight, "sqlite wal mode rocks", ""))
        .unwrap();
    e.add_node(new_node(NodeType::Insight, "sqlite wal mode rocks?", ""))
        .unwrap();
    e.scan_conflicts().unwrap();
    let s = e.suspects().unwrap().remove(0);
    assert!(
        e.resolve_suspect(&s.id, SuspectVerdict::Dismiss, Source::User)
            .unwrap()
            .is_none()
    );
    assert!(e.suspects().unwrap().is_empty());
    assert_eq!(e.scan_conflicts().unwrap(), 0);
}

#[test]
fn anchors_and_linked_pairs_are_not_suspects() {
    let e = engine();
    let a1 = e
        .add_node(new_node(NodeType::Anchor, "auth flow", ""))
        .unwrap();
    let a2 = e
        .add_node(new_node(NodeType::Anchor, "auth flow!", ""))
        .unwrap();
    assert_eq!(e.scan_conflicts().unwrap(), 0, "anchors never suspect");

    let d1 = e
        .add_node(new_node(NodeType::Decision, "jwt in http-only cookie", ""))
        .unwrap();
    let d2 = e
        .add_node(new_node(NodeType::Decision, "jwt in http-only cookies", ""))
        .unwrap();
    e.add_edge(NewEdge {
        edge_type: EdgeType::BuildsOn,
        from_id: d2.id.clone(),
        to_id: d1.id.clone(),
        source: Source::User,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();
    assert_eq!(e.scan_conflicts().unwrap(), 0, "linked pairs never suspect");
    let _ = (a1, a2);
}

// ---- decay (PLAN §6B) --------------------------------------------------------

fn episodic(t: NodeType, title: &str) -> NewNode {
    NewNode {
        durability: Durability::Episodic,
        ..new_node(t, title, "")
    }
}

#[test]
fn decay_archives_only_stale_unapproved_claude_episodic_nodes() {
    let e = engine();
    let doomed = e
        .add_node(episodic(NodeType::Insight, "temp build workaround"))
        .unwrap();
    let stable = e
        .add_node(new_node(NodeType::Principle, "local first", ""))
        .unwrap();
    let approved = e
        .add_node(episodic(NodeType::Insight, "approved but old"))
        .unwrap();
    e.approve(&approved.id).unwrap();
    let fresh = e
        .add_node(episodic(NodeType::Insight, "fresh insight"))
        .unwrap();

    // 100 days: past stale crossing (~75d unseen) + the 14-day TTL.
    for id in [&doomed.id, &stable.id, &approved.id] {
        backdate(&e, id, 100);
    }
    // Approval survives the backdate (approve() stamped now).

    let preview = e.decay(policy::DECAY_TTL_DAYS, true).unwrap();
    assert_eq!(preview, vec![doomed.id.clone()]);
    assert!(
        e.get_node(&doomed.id)
            .unwrap()
            .unwrap()
            .valid_until
            .is_none(),
        "dry run mutates nothing"
    );

    let archived = e.decay(policy::DECAY_TTL_DAYS, false).unwrap();
    assert_eq!(archived, vec![doomed.id.clone()]);
    assert!(
        e.get_node(&doomed.id)
            .unwrap()
            .unwrap()
            .valid_until
            .is_some()
    );
    for id in [&stable.id, &approved.id, &fresh.id] {
        assert!(e.get_node(id).unwrap().unwrap().valid_until.is_none());
    }
}

#[test]
fn reclassification_via_node_patch() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Insight, "actually a decision", ""))
        .unwrap();
    let updated = e
        .update_node(
            &n.id,
            NodePatch {
                node_type: Some(NodeType::Decision),
                ..NodePatch::default()
            },
        )
        .unwrap();
    assert_eq!(updated.node_type, NodeType::Decision);
}
