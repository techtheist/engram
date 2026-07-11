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
        tags: vec![],
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
    assert!(text.contains("## Recently added"));
    assert!(text.contains("local first always"));
    assert!(text.contains("backend in rust"));
    assert!(text.contains(&prob.title));
    // every record carries its node id (the brief doubles as a lookup table),
    // and each node surfaces exactly once — the first section claims it.
    for n in [&p, &d, &prob, &i, &extra] {
        assert_eq!(
            text.matches(n.id.as_str()).count(),
            1,
            "\"{}\" must appear exactly once with its id: {text}",
            n.title
        );
    }

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
        tags: vec![],
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

// ---- tags (PLAN §10) -------------------------------------------------------

#[test]
fn tags_normalize_dedupe_and_roundtrip() {
    let s = store();
    let n = s
        .add_node(NewNode {
            tags: vec![
                "Phase 1".into(),
                "phase-1".into(),
                "  UI  ".into(),
                "".into(),
            ],
            ..new_node(NodeType::Decision, "tagged", "body")
        })
        .unwrap();
    assert_eq!(n.tags, vec!["phase-1", "ui"]);
    assert_eq!(s.get_node(&n.id).unwrap().unwrap().tags, n.tags);
}

#[test]
fn node_patch_replaces_tags_and_none_keeps_them() {
    let s = store();
    let n = s
        .add_node(NewNode {
            tags: vec!["phase-1".into()],
            ..new_node(NodeType::Decision, "tagged", "")
        })
        .unwrap();
    let updated = s
        .update_node(
            &n.id,
            NodePatch {
                tags: Some(vec!["Phase 2".into()]),
                ..NodePatch::default()
            },
        )
        .unwrap();
    assert_eq!(updated.tags, vec!["phase-2"]);
    let untouched = s.update_node(&n.id, NodePatch::default()).unwrap();
    assert_eq!(untouched.tags, vec!["phase-2"]);
}

#[test]
fn tag_stats_count_and_skip_archived() {
    let s = store();
    let a = s
        .add_node(NewNode {
            tags: vec!["alpha".into()],
            ..new_node(NodeType::Decision, "first", "")
        })
        .unwrap();
    let b = s
        .add_node(NewNode {
            tags: vec!["alpha".into(), "beta".into()],
            ..new_node(NodeType::Insight, "second", "")
        })
        .unwrap();

    let stats = s.tag_stats(10).unwrap();
    assert_eq!(stats.len(), 2);
    let alpha = stats.iter().find(|t| t.tag == "alpha").unwrap();
    assert_eq!(alpha.count, 2);

    // Archiving the only carrier of "beta" drops it from the vocabulary.
    s.archive_nodes(std::slice::from_ref(&b.id), now()).unwrap();
    let stats = s.tag_stats(10).unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].tag, "alpha");
    let _ = a;
}

#[test]
fn fts_finds_nodes_by_tag() {
    let s = store();
    let n = s
        .add_node(NewNode {
            tags: vec!["observability".into()],
            ..new_node(NodeType::Decision, "storage layer", "sqlite wal")
        })
        .unwrap();
    let hits = s.search_fts("observability", &[], 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, n.id);
}

#[test]
fn update_edge_retypes() {
    let s = store();
    let a = s.add_node(new_node(NodeType::Insight, "a", "")).unwrap();
    let b = s.add_node(new_node(NodeType::Decision, "b", "")).unwrap();
    let e = s
        .add_edge(NewEdge {
            edge_type: EdgeType::About,
            from_id: a.id.clone(),
            to_id: b.id.clone(),
            source: Source::User,
            note: None,
            confidence: None,
            strength: None,
            status: None,
        })
        .unwrap();
    let patched = s
        .update_edge(
            &e.id,
            EdgePatch {
                edge_type: Some(EdgeType::Because),
                ..EdgePatch::default()
            },
        )
        .unwrap();
    assert_eq!(patched.edge_type, EdgeType::Because);
}

#[test]
fn legacy_db_gains_tags_column_and_fts_rebuild() {
    let path = std::env::temp_dir().join(format!("engram-test-{}.db", id::new_id()));

    // A database from before tags existed: no tags column, two-column FTS.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (
               id TEXT PRIMARY KEY, type TEXT NOT NULL, title TEXT NOT NULL, body TEXT,
               durability TEXT NOT NULL, source TEXT NOT NULL, session_id TEXT,
               created_at INTEGER NOT NULL, valid_from INTEGER, valid_until INTEGER,
               status TEXT, code_refs TEXT, last_seen INTEGER, approved_at INTEGER
             );
             CREATE VIRTUAL TABLE nodes_fts
               USING fts5(title, body, content='nodes', content_rowid='rowid');
             CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
               INSERT INTO nodes_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
             END;
             INSERT INTO nodes (id, type, title, body, durability, source, created_at, code_refs)
               VALUES ('aaaaaaaaaaaa', 'Decision', 'legacy node', 'old body', 'stable', 'user',
                       1, '[]');",
        )
        .unwrap();
    }

    let s = Store::open(&path).unwrap();
    let legacy = s.get_node("aaaaaaaaaaaa").unwrap().unwrap();
    assert!(legacy.tags.is_empty(), "pre-tags rows read as untagged");

    // The rebuilt FTS + triggers index tags on the migrated database.
    s.update_node(
        "aaaaaaaaaaaa",
        NodePatch {
            tags: Some(vec!["migrated".into()]),
            ..NodePatch::default()
        },
    )
    .unwrap();
    let hits = s.search_fts("migrated", &[], 10).unwrap();
    assert_eq!(hits.len(), 1, "tag is FTS-searchable after migration");

    drop(s);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn brief_leads_with_recent_tags() {
    let e = engine();
    e.add_node(NewNode {
        tags: vec!["phase-1".into()],
        ..new_node(NodeType::Decision, "tagged decision", "")
    })
    .unwrap();
    let brief = e.brief(2000).unwrap();
    let idx = brief.find("Recent tags").expect("brief names recent tags");
    assert!(
        idx < 100,
        "tags line sits at the top, before budget-cut sections: {brief}"
    );
    assert!(brief.contains("phase-1"));
}

#[test]
fn brief_caps_open_worklist_with_exact_overflow() {
    let e = engine();
    for i in 0..24 {
        e.add_node(NewNode {
            status: Some(NodeStatus::Open),
            durability: Durability::Volatile,
            ..new_node(NodeType::Intent, &format!("todo item number {i}"), "")
        })
        .unwrap();
    }
    let brief = e.brief(50_000).unwrap();
    // "Recently added" comes first and claims the 7 newest; the worklist
    // shows its cap of 10 from the rest and counts the overflow exactly.
    let recent = brief.find("## Recently added").expect("recent section");
    let open = brief.find("## Open problems & intents").expect("worklist");
    assert!(recent < open, "recent must precede the worklist: {brief}");
    assert_eq!(brief.matches("- todo item number").count(), 17);
    assert!(
        brief.contains("…and 7 more — `list_open` has the full worklist."),
        "got: {brief}"
    );
}

#[test]
fn brief_counts_canon_overflow() {
    let e = engine();
    for i in 0..15 {
        e.add_node(new_node(
            NodeType::Decision,
            &format!("decision number {i}"),
            "",
        ))
        .unwrap();
    }
    let brief = e.brief(50_000).unwrap();
    // 7 newest land in "Recently added"; the Decisions section fills its cap
    // of 7 with the others and the overflow counts only what's truly unseen.
    assert_eq!(brief.matches("- decision number").count(), 14);
    assert!(
        brief.contains("…1 more Decisions — `search` reaches them."),
        "got: {brief}"
    );
}

#[test]
fn brief_line_excerpts_stay_compact() {
    let e = engine();
    let long_body = "word ".repeat(120); // ~600 chars, must be cut
    e.add_node(new_node(NodeType::Decision, "verbose decision", &long_body))
        .unwrap();
    e.add_node(new_node(NodeType::Caution, "verbose caution", &long_body))
        .unwrap();
    // enough newer nodes that both fall out of "Recently added" and render
    // in their canon sections
    for i in 0..7 {
        e.add_node(new_node(NodeType::Insight, &format!("filler {i}"), ""))
            .unwrap();
    }
    let brief = e.brief(50_000).unwrap();
    let line = |title: &str| {
        let prefix = format!("- {title}");
        brief
            .lines()
            .find(|l| l.starts_with(&prefix))
            .unwrap_or_else(|| panic!("line for {title} present: {brief}"))
            .to_string()
    };
    let caution = line("verbose caution");
    assert!(
        caution.chars().count() < 210,
        "title + id + ~140-char excerpt, got {} chars: {caution}",
        caution.chars().count()
    );
    assert!(caution.ends_with('…'), "cut excerpts end with an ellipsis");
    let decision = line("verbose decision");
    assert!(
        decision.chars().count() < 150,
        "decisions preview less body (~80 chars), got {} chars: {decision}",
        decision.chars().count()
    );
    assert!(
        decision.chars().count() < caution.chars().count(),
        "decision excerpts are shorter than the default"
    );
}

// ---- timeline (PLAN §10) -------------------------------------------------------

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
        source: Source::Claude,
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

// ---- verified code refs (PLAN §10) --------------------------------------------

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

// ---- audit journal (PLAN §10) ------------------------------------------------

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
            source: Source::Claude,
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

#[test]
fn embed_composition_upgrade_reembeds_only_with_real_vectors() {
    let db = std::env::temp_dir().join(format!("engram-compose-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);

    // FakeEmbedder math with the real-embedder contract — exercises the
    // upgrade path without ONNX.
    struct NotFake(FakeEmbedder);
    impl Embedder for NotFake {
        fn dim(&self) -> usize {
            self.0.dim()
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.0.embed(texts)
        }
    }

    // Seed a node, then rewind the version marker to legacy.
    let e = Engine::new(Store::open(&db).unwrap(), Box::new(FakeEmbedder::default()));
    let mut n = new_node(NodeType::Decision, "composed", "");
    n.code_refs = vec!["crates/engram-core/src/rag.rs".into()];
    e.add_node(n).unwrap();
    e.store().set_embed_version(0).unwrap();

    // A fake embedder must refuse to touch an existing graph's vectors.
    assert_eq!(e.ensure_embed_composition().unwrap(), 0);
    assert_eq!(e.store().embed_version().unwrap(), 0);
    drop(e);

    // A real one upgrades every node and stamps the version.
    let e = Engine::new(
        Store::open(&db).unwrap(),
        Box::new(NotFake(FakeEmbedder::default())),
    );
    assert_eq!(e.ensure_embed_composition().unwrap(), 1);
    assert_eq!(e.store().embed_version().unwrap(), EMBED_COMPOSITION);
    drop(e);
    let _ = std::fs::remove_file(&db);
}
