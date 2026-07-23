use crate::*;

fn store() -> SqliteStore {
    SqliteStore::open_in_memory().unwrap()
}

/// The default policy — the numbers every pre-config test ran under.
fn dp() -> crate::config::PolicyConfig {
    crate::config::PolicyConfig::default()
}

fn new_node(t: NodeType, title: &str, body: &str) -> NewNode {
    NewNode {
        node_type: t,
        title: title.to_string(),
        body: Some(body.to_string()),
        created_at: None,
        durability: Durability::Stable,
        source: Source::Claude,
        session_id: Some("s1".to_string()),
        status: None,
        code_refs: vec![],
        tags: vec![],
        version: None,
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
    // Ontology-as-data (PLAN §7D): parse checks only shape — a name the
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
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    )
}

/// Deterministic stand-in for the precision layer: strongly prefers any
/// document containing "WINNER", scoring the rest at a large negative logit.
struct FavorWinner;

impl crate::rag::Reranker for FavorWinner {
    fn rank(&self, _query: &str, documents: &[String]) -> crate::Result<Vec<f32>> {
        Ok(documents
            .iter()
            .map(|d| if d.contains("WINNER") { 8.0 } else { -8.0 })
            .collect())
    }
}

#[test]
fn reranker_reorders_hits_and_touches_only_what_is_returned() {
    let mut e = engine();
    e.set_reranker(Box::new(FavorWinner));
    assert!(e.has_reranker());

    let filler_a = e
        .add_node(new_node(NodeType::Insight, "keyword filler alpha", ""))
        .unwrap();
    let filler_b = e
        .add_node(new_node(NodeType::Insight, "keyword filler beta", ""))
        .unwrap();
    let winner = e
        .add_node(new_node(NodeType::Insight, "keyword WINNER gamma", ""))
        .unwrap();

    let hits = e.search("keyword", &[], 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, winner.id, "cross-encoder verdict wins the tie");
    assert!(hits[0].score > 0.0);

    // Over-fetched candidates the reranker discarded are not "seen" — the
    // observability stamp covers only what the caller actually received.
    assert!(e.get_node(&winner.id).unwrap().unwrap().last_seen.is_some());
    for id in [&filler_a.id, &filler_b.id] {
        assert!(e.get_node(id).unwrap().unwrap().last_seen.is_none());
    }
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
    s.upsert_embeddings(&a.id, &[e.embed_one("alpha").unwrap()])
        .unwrap();
    s.upsert_embeddings(&b.id, &[e.embed_one("beta").unwrap()])
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

/// Baseline TrustInputs: unconfirmed, unapproved, undemoted, unpinned episodic.
fn ti(created_at: i64) -> policy::TrustInputs {
    policy::TrustInputs {
        created_at,
        confirmed_at: None,
        approved_at: None,
        demoted_at: None,
        trust_override: None,
        durability: Durability::Episodic,
        status: None,
    }
}

#[test]
fn trust_curves_follow_the_anchor_timestamps() {
    use crate::policy::{self, trust};
    let day = 24 * 60 * 60;
    let t0 = 1_000_000i64;

    // created-only episodic: 50% now, linear to the floor at half a year
    assert!((trust(&ti(t0), t0, &dp()) - policy::TRUST_UNSEEN_START).abs() < 1e-9);
    let half_window = t0 + policy::PROVISIONAL_TRUST_WINDOW_SECS / 2;
    let mid = trust(&ti(t0), half_window, &dp());
    assert!((mid - (policy::TRUST_UNSEEN_START + policy::TRUST_FLOOR) / 2.0).abs() < 1e-6);
    assert!(
        (trust(
            &ti(t0),
            t0 + policy::PROVISIONAL_TRUST_WINDOW_SECS + day,
            &dp()
        ) - policy::TRUST_FLOOR)
            .abs()
            < 1e-9
    );

    // confirmed: restarts at 60% from confirmed_at, beats created-only
    let confirmed = policy::TrustInputs {
        confirmed_at: Some(t0 + 100 * day),
        ..ti(t0)
    };
    assert!(
        (trust(&confirmed, t0 + 100 * day, &dp()) - policy::TRUST_CONFIRMED_START).abs() < 1e-9
    );
    assert!(trust(&confirmed, t0 + 100 * day, &dp()) > trust(&ti(t0), t0 + 100 * day, &dp()));

    // approved: 100% at approval, floor 20% past a year, wins over confirmed
    let approved = policy::TrustInputs {
        approved_at: Some(t0),
        confirmed_at: Some(t0),
        ..ti(t0)
    };
    assert!((trust(&approved, t0, &dp()) - policy::TRUST_APPROVED_START).abs() < 1e-9);
    let old = trust(
        &approved,
        t0 + policy::APPROVED_TRUST_WINDOW_SECS + day,
        &dp(),
    );
    assert!((old - policy::TRUST_APPROVED_FLOOR).abs() < 1e-9);

    // volatile rots on the short window
    let volatile = policy::TrustInputs {
        durability: Durability::Volatile,
        ..ti(t0)
    };
    let aged = t0 + policy::VOLATILE_TRUST_WINDOW_SECS + day;
    assert!((trust(&volatile, aged, &dp()) - policy::TRUST_FLOOR).abs() < 1e-9);
    assert!(
        trust(&ti(t0), aged, &dp()) > policy::TRUST_FLOOR,
        "episodic outlives volatile"
    );

    // staleness threshold
    assert!(policy::is_stale(policy::STALE_TRUST - 0.01, &dp()));
    assert!(!policy::is_stale(policy::STALE_TRUST, &dp()));
}

#[test]
fn stable_trust_holds_flat_until_evidence_demotes_it() {
    use crate::policy::{self, trust};
    let year = 365 * 24 * 60 * 60;
    let t0 = 1_000_000i64;
    let stable = policy::TrustInputs {
        durability: Durability::Stable,
        ..ti(t0)
    };

    // Time alone never moves stable knowledge — the redditor's rare
    // production constraint survives its quiet year at full anchor value.
    assert_eq!(
        trust(&stable, t0 + 3 * year, &dp()),
        policy::TRUST_UNSEEN_START
    );
    let approved_stable = policy::TrustInputs {
        approved_at: Some(t0),
        ..stable
    };
    assert_eq!(
        trust(&approved_stable, t0 + 3 * year, &dp()),
        policy::TRUST_APPROVED_START
    );

    // Contradicting evidence starts the ramp — from the event, not creation.
    let demoted = policy::TrustInputs {
        demoted_at: Some(t0 + 2 * year),
        ..stable
    };
    assert_eq!(
        trust(&demoted, t0 + 2 * year, &dp()),
        policy::TRUST_UNSEEN_START
    );
    assert!(
        trust(
            &demoted,
            t0 + 2 * year + policy::PROVISIONAL_TRUST_WINDOW_SECS,
            &dp()
        ) < 0.05
    );

    // Pin overrides everything, including demotion.
    let pinned = policy::TrustInputs {
        trust_override: Some(1.0),
        ..demoted
    };
    assert_eq!(trust(&pinned, t0 + 3 * year, &dp()), 1.0);
    // Constant overrides are clamped to 0..=1.
    let odd = policy::TrustInputs {
        trust_override: Some(1.7),
        ..stable
    };
    assert_eq!(trust(&odd, t0, &dp()), 1.0);

    // Open worklist items are never buried by age.
    let open = policy::TrustInputs {
        status: Some(NodeStatus::Open),
        ..ti(t0)
    };
    assert_eq!(
        trust(&open, t0 + 3 * year, &dp()),
        policy::TRUST_UNSEEN_START
    );
    assert!(
        policy::stale_since(&open, &dp()).is_none(),
        "open never decays out"
    );
    assert!(
        policy::stale_since(&stable, &dp()).is_none(),
        "stable never decays out"
    );
    assert!(
        policy::stale_since(&ti(t0), &dp()).is_some(),
        "plain episodic still crosses"
    );
}

/// The sandbox tester's adversarial scenario: a frequently retrieved false
/// note vs a rarely retrieved true constraint. Exposure must not preserve the
/// false one — retrieval never refreshes trust — while the stable constraint
/// holds without ever being surfaced.
#[test]
fn exposure_does_not_preserve_the_false_note() {
    let e = engine();
    let false_note = e
        .add_node(episodic(NodeType::Insight, "attractive but wrong claim"))
        .unwrap();
    let constraint = e
        .add_node(new_node(
            NodeType::Caution,
            "rare but true production constraint",
            "only matters during the yearly migration",
        ))
        .unwrap();

    // Age both past the episodic stale crossing + decay TTL.
    backdate(&e, &false_note.id, 100);
    backdate(&e, &constraint.id, 100);

    // Broad recurring searches keep surfacing the false note...
    for _ in 0..5 {
        let hits = e.search("attractive wrong claim", &[], 5).unwrap();
        assert!(hits.iter().any(|h| h.id == false_note.id));
    }
    let surfaced = e.get_node(&false_note.id).unwrap().unwrap();
    assert!(
        surfaced.last_seen.is_some(),
        "retrieval is still observable"
    );
    assert!(
        surfaced.confirmed_at.is_none(),
        "…but being findable confirms nothing"
    );
    assert!(surfaced.stale, "exposure did not keep the false note alive");

    // The decay pass takes the exposed false note and spares the quiet truth.
    let archived = e.decay(policy::DECAY_TTL_DAYS, false).unwrap();
    assert_eq!(archived, vec![false_note.id.clone()]);
    let kept = e.get_node(&constraint.id).unwrap().unwrap();
    assert!(kept.valid_until.is_none());
    assert_eq!(kept.trust, policy::TRUST_UNSEEN_START, "stable holds flat");
}

/// The sandbox tester's negative control, verbatim (v0.4.1 feedback thread):
/// two unapproved nodes with equal initial trust and age; retrieve A a
/// hundred times and never B; approve, update, or verify neither; advance
/// the clock — A may RANK higher for its query, but its TRUST must equal
/// B's. Retrieval frequency is telemetry, never evidence quality.
#[test]
fn retrieval_frequency_is_not_evidence() {
    let e = engine();
    let a = e
        .add_node(episodic(NodeType::Insight, "cache warming strategy alpha"))
        .unwrap();
    let b = e
        .add_node(episodic(
            NodeType::Insight,
            "database vacuum schedule omega",
        ))
        .unwrap();
    // Equal age, past enough of the ramp that a refreshed clock would show.
    backdate(&e, &a.id, 60);
    backdate(&e, &b.id, 60);

    // 100 retrievals of A (touch is the exact stamp every search hit and
    // brief inclusion applies), zero of B.
    for _ in 0..100 {
        e.store().touch(std::slice::from_ref(&a.id)).unwrap();
    }

    let a2 = e.get_node(&a.id).unwrap().unwrap();
    let b2 = e.get_node(&b.id).unwrap().unwrap();
    assert!(a2.last_seen.is_some(), "exposure stays observable");
    assert!(b2.last_seen.is_none());
    assert!(
        (a2.trust - b2.trust).abs() < 1e-6,
        "equal trust despite 100:0 exposure — got {} vs {}",
        a2.trust,
        b2.trust
    );
}

// ---- local cortex, logic layer (PLAN §7A) ---------------------------------

fn engine_with_nli() -> Engine {
    let mut e = engine();
    e.set_nli(Box::new(crate::nli::FakeNli));
    e
}

#[test]
fn write_time_suspects_carry_nli_hints() {
    let e = engine_with_nli();
    // FakeNli: both texts containing "contra" → contradiction hint.
    e.add_node(new_node(
        NodeType::Decision,
        "contra: sessions in redis",
        "",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Decision,
        "contra: sessions in redis!",
        "",
    ))
    .unwrap();
    e.scan_conflicts().unwrap();
    let s = e.suspects().unwrap().remove(0);
    assert_eq!(s.nli_label.as_deref(), Some("contradiction"));
    assert!(s.nli_score.unwrap() > 0.5, "hint carries its probability");

    // The brief surfaces the hint next to the pair.
    let brief = e.brief(8000).unwrap();
    assert!(brief.contains("hint: contradiction"), "got: {brief}");
}

#[test]
fn check_claim_buckets_supports_contradicts_silent() {
    let e = engine_with_nli();
    e.add_node(new_node(
        NodeType::Decision,
        "contra: we store sessions in cookies",
        "",
    ))
    .unwrap();
    e.add_node(new_node(NodeType::Insight, "the parser uses nom", ""))
        .unwrap();

    // FakeNli reads shared "contra" as contradiction.
    let report = e
        .check_claim("contra: sessions live in localStorage", 8)
        .unwrap();
    assert_eq!(report.contradicts.len(), 1, "{report:?}");
    assert!(report.contradicts[0].title.contains("cookies"));

    // Entailment: the claim is contained in a node's claim text.
    let report = e.check_claim("the parser uses nom", 8).unwrap();
    assert_eq!(report.supports.len(), 1, "{report:?}");

    // Without the NLI layer the check refuses instead of guessing.
    let bare = engine();
    assert!(bare.check_claim("anything", 8).is_err());
}

#[test]
fn audit_sweeps_queue_only_their_target_label() {
    let e = engine_with_nli();
    // A contradiction pair (shared "contra" marker) and a duplicate pair
    // (identical text, different type so the dupe guard doesn't collapse it).
    e.add_node(new_node(NodeType::Decision, "contra: deploy via ftp", ""))
        .unwrap();
    e.add_node(new_node(NodeType::Caution, "contra: deploy via ftp!", ""))
        .unwrap();
    e.add_node(new_node(
        NodeType::Decision,
        "release trains ship monthly",
        "",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Insight,
        "release trains ship monthly",
        "",
    ))
    .unwrap();

    let conflicts = e.audit_conflicts().unwrap();
    assert_eq!(
        conflicts.queued, 1,
        "only the contradiction pair: {conflicts:?}"
    );
    let duplicates = e.audit_duplicates().unwrap();
    assert_eq!(
        duplicates.queued, 1,
        "only the duplicate pair: {duplicates:?}"
    );
    assert!(!conflicts.truncated && !duplicates.truncated);

    let suspects = e.suspects().unwrap();
    assert_eq!(suspects.len(), 2);
    // Re-running queues nothing (raised pairs are never re-raised).
    assert_eq!(e.audit_conflicts().unwrap().queued, 0);
}

#[test]
fn writes_report_missing_code_refs_in_the_same_turn() {
    let mut e = engine();
    e.set_repo_root(std::env::current_dir().unwrap());
    let outcome = e
        .add_node_checked(NewNode {
            code_refs: vec!["Cargo.toml".into(), "src/vanished.rs".into()],
            ..new_node(NodeType::Decision, "refs checked at write time", "")
        })
        .unwrap();
    let WriteOutcome::Created {
        node, missing_refs, ..
    } = outcome
    else {
        panic!("expected creation");
    };
    assert_eq!(
        missing_refs,
        vec!["src/vanished.rs"],
        "caught at write time"
    );

    // Repairing through the checked update reports the fix the same way.
    let repaired = e
        .update_node_checked(
            &node.id,
            NodePatch {
                code_refs: Some(vec!["Cargo.toml".into()]),
                ..NodePatch::default()
            },
        )
        .unwrap();
    assert!(repaired.missing_refs.is_empty());
}

#[test]
fn negated_duplicate_gets_a_contradiction_hint_on_match() {
    let e = engine_with_nli();
    e.add_node(new_node(
        NodeType::Decision,
        "contra: use tabs for indentation",
        "",
    ))
    .unwrap();
    // Same type + near-identical text trips the dupe guard; FakeNli reads the
    // shared "contra" marker as contradiction — the negated-duplicate case.
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Decision,
            "contra: use tabs for indentation",
            "",
        ))
        .unwrap();
    let WriteOutcome::Matched {
        nli_label,
        nli_score,
        ..
    } = outcome
    else {
        panic!("expected the dupe guard");
    };
    assert_eq!(nli_label.as_deref(), Some("contradiction"));
    assert!(nli_score.unwrap() > 0.5);
}

#[test]
fn audit_answered_nominates_but_never_resolves() {
    let e = engine_with_nli();
    let problem = e
        .add_node(NewNode {
            status: Some(NodeStatus::Open),
            ..new_node(NodeType::Problem, "ci cache misses on macos", "")
        })
        .unwrap();
    // FakeNli entailment: candidate claim contains the problem claim.
    e.add_node(new_node(
        NodeType::Resolution,
        "ci cache misses on macos: fixed by keyed restore paths",
        "",
    ))
    .unwrap();

    let hints = e.audit_answered().unwrap();
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert_eq!(hints[0].problem.id, problem.id);
    assert!(hints[0].entailment > 0.5);
    // Nomination only: the problem is still open.
    let still = e.get_node(&problem.id).unwrap().unwrap();
    assert_eq!(still.status, Some(NodeStatus::Open));
}

#[test]
fn deliberate_acts_confirm_and_clear_demotion() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Decision, "we use postgres", ""))
        .unwrap();
    assert!(n.confirmed_at.is_none());

    // Confirm still true (the pane's […] action) stamps the trust anchor.
    let confirmed = e.reconfirm(&n.id).unwrap();
    assert!(confirmed.confirmed_at.is_some());

    // Evidence demotes; a later deliberate update clears the demotion.
    e.store().demote(&n.id, now()).unwrap();
    let demoted = e.get_node(&n.id).unwrap().unwrap();
    assert!(demoted.demoted_at.is_some());
    let repaired = e
        .update_node(
            &n.id,
            NodePatch {
                body: Some("verified against prod 2026-07".into()),
                ..NodePatch::default()
            },
        )
        .unwrap();
    assert!(repaired.demoted_at.is_none(), "repair is re-validation");

    // Approval also clears demotion and restores the ceiling.
    e.store().demote(&n.id, now()).unwrap();
    let approved = e.approve(&n.id).unwrap();
    assert!(approved.demoted_at.is_none());
    assert!(approved.trust > 0.99);

    // Revoking drops back to the confirmed anchor and clears any pin.
    e.set_trust_override(&n.id, Some(1.0)).unwrap();
    let revoked = e.revoke_approval(&n.id).unwrap();
    assert!(revoked.approved_at.is_none());
    assert!(revoked.trust_override.is_none());
    assert!((revoked.trust - policy::TRUST_CONFIRMED_START).abs() < 1e-6);
}

#[test]
fn conflict_edges_demote_the_older_endpoint_but_never_pins() {
    let e = engine();
    let old = e
        .add_node(new_node(NodeType::Decision, "sessions live in redis", ""))
        .unwrap();
    backdate(&e, &old.id, 30);
    let newer = e
        .add_node(new_node(
            NodeType::Decision,
            "sessions live in postgres",
            "",
        ))
        .unwrap();

    e.add_edge(NewEdge {
        edge_type: EdgeType::ConflictsWith,
        from_id: newer.id.clone(),
        to_id: old.id.clone(),
        source: Source::Claude,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();
    let demoted = e.get_node(&old.id).unwrap().unwrap();
    assert!(demoted.demoted_at.is_some(), "older claim starts decaying");
    assert!(
        e.get_node(&newer.id).unwrap().unwrap().demoted_at.is_none(),
        "newer claim stands"
    );

    // A pinned node never demotes silently — a human said forever.
    let pinned = e
        .add_node(new_node(NodeType::Decision, "auth flows through oauth", ""))
        .unwrap();
    backdate(&e, &pinned.id, 30);
    e.set_trust_override(&pinned.id, Some(1.0)).unwrap();
    let challenger = e
        .add_node(new_node(NodeType::Decision, "auth flows through saml", ""))
        .unwrap();
    e.add_edge(NewEdge {
        edge_type: EdgeType::ConflictsWith,
        from_id: challenger.id.clone(),
        to_id: pinned.id.clone(),
        source: Source::Claude,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();
    assert!(
        e.get_node(&pinned.id)
            .unwrap()
            .unwrap()
            .demoted_at
            .is_none(),
        "evidence surfaces in review, never silently unpins"
    );
}

#[test]
fn pinned_nodes_never_decay_out_and_brief_marks_them() {
    let e = engine();
    let pinned = e
        .add_node(episodic(NodeType::Insight, "pinned scratch note"))
        .unwrap();
    e.set_trust_override(&pinned.id, Some(1.0)).unwrap();
    backdate(&e, &pinned.id, 400);

    assert!(e.decay(policy::DECAY_TTL_DAYS, true).unwrap().is_empty());
    let n = e.get_node(&pinned.id).unwrap().unwrap();
    assert_eq!(n.trust, 1.0);
    assert!(!n.stale);
    assert!(
        e.brief(8000).unwrap().contains("PINNED"),
        "the assistant sees the pin"
    );
}

#[test]
fn drift_scan_reports_but_never_demotes() {
    let e = engine();
    let n = e
        .add_node(NewNode {
            code_refs: vec!["src/vanished.rs".into()],
            ..new_node(NodeType::Decision, "refers to moved code", "")
        })
        .unwrap();
    let drifted = e.scan_code_refs(std::path::Path::new(".")).unwrap();
    assert_eq!(drifted.len(), 1);
    // The scan runs on every pane load against an environment-dependent
    // root — a wrong cwd must not be able to mass-demote the graph.
    let after = e.get_node(&n.id).unwrap().unwrap();
    assert!(after.demoted_at.is_none(), "drift is review, not evidence");
}

#[test]
fn withdrawing_conflict_evidence_withdraws_the_demotion() {
    let e = engine();
    let old = e
        .add_node(new_node(NodeType::Decision, "config lives in toml", ""))
        .unwrap();
    backdate(&e, &old.id, 30);
    let newer = e
        .add_node(new_node(NodeType::Decision, "config lives in yaml", ""))
        .unwrap();
    let conflict = |from: &str, to: &str| NewEdge {
        edge_type: EdgeType::ConflictsWith,
        from_id: from.to_string(),
        to_id: to.to_string(),
        source: Source::Claude,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    };

    // Dismissing the edge clears the demotion it caused.
    let edge = e.add_edge(conflict(&newer.id, &old.id)).unwrap();
    assert!(e.get_node(&old.id).unwrap().unwrap().demoted_at.is_some());
    e.update_edge(
        &edge.id,
        EdgePatch {
            status: Some(EdgeStatus::Dismissed),
            ..EdgePatch::default()
        },
    )
    .unwrap();
    assert!(
        e.get_node(&old.id).unwrap().unwrap().demoted_at.is_none(),
        "evidence withdrawn — the innocent node stops decaying"
    );

    // Deleting the edge clears it too.
    let edge = e.add_edge(conflict(&newer.id, &old.id)).unwrap();
    assert!(e.get_node(&old.id).unwrap().unwrap().demoted_at.is_some());
    e.delete_edge(&edge.id).unwrap();
    assert!(e.get_node(&old.id).unwrap().unwrap().demoted_at.is_none());

    // Retyping an existing edge TO conflicts-with demotes (the documented
    // mislink-repair path must carry the same evidence semantics as link).
    let mislink = e
        .add_edge(NewEdge {
            edge_type: EdgeType::BuildsOn,
            ..conflict(&newer.id, &old.id)
        })
        .unwrap();
    assert!(e.get_node(&old.id).unwrap().unwrap().demoted_at.is_none());
    e.update_edge(
        &mislink.id,
        EdgePatch {
            edge_type: Some(EdgeType::ConflictsWith),
            ..EdgePatch::default()
        },
    )
    .unwrap();
    assert!(
        e.get_node(&old.id).unwrap().unwrap().demoted_at.is_some(),
        "retype-to-conflict is evidence arriving"
    );
}

#[test]
fn claude_replaces_verdict_cannot_archive_a_pinned_node() {
    let e = engine();
    let pinned = e
        .add_node(new_node(NodeType::Decision, "ship binaries via github", ""))
        .unwrap();
    backdate(&e, &pinned.id, 10);
    e.set_trust_override(&pinned.id, Some(1.0)).unwrap();
    e.add_node(new_node(
        NodeType::Decision,
        "ship binaries via github!",
        "",
    ))
    .unwrap();
    e.scan_conflicts().unwrap();
    let s = e.suspects().unwrap().remove(0);
    assert_eq!(s.b.id, pinned.id);

    let err = e
        .resolve_suspect(&s.id, SuspectVerdict::Replaces, Source::Claude)
        .unwrap_err();
    assert!(matches!(err, crate::Error::Pinned(_)), "got: {err:?}");
    let still = e.get_node(&pinned.id).unwrap().unwrap();
    assert!(still.valid_until.is_none(), "the pin held");

    // The user's own verdict proceeds — a human unsays a human's pin.
    e.resolve_suspect(&s.id, SuspectVerdict::Replaces, Source::User)
        .unwrap();
    assert!(
        e.get_node(&pinned.id)
            .unwrap()
            .unwrap()
            .valid_until
            .is_some()
    );
}

#[test]
fn import_backfills_confirmed_at_like_the_migration() {
    let e = engine();
    // A pre-trust-v2 export: last_seen kept the node alive, confirmed_at
    // doesn't exist in the JSON (deserializes to None).
    let day = 24 * 60 * 60;
    let seen = now() - 10 * day;
    let node = Node {
        id: "aaaaaaaaaaaa".into(),
        node_type: NodeType::Insight,
        title: "healthy on v0.4.1".into(),
        body: None,
        durability: Durability::Episodic,
        source: Source::Claude,
        session_id: None,
        created_at: now() - 200 * day,
        valid_from: None,
        valid_until: None,
        status: None,
        last_seen: Some(seen),
        confirmed_at: None,
        approved_at: None,
        demoted_at: None,
        trust_override: None,
        trust: 0.0,
        stale: false,
        code_refs: vec![],
        tags: vec![],
        version: None,
    };
    e.import(ExportGraph {
        version: EXPORT_VERSION,
        nodes: vec![node],
        edges: vec![],
        config: None,
    })
    .unwrap();
    let restored = e.get_node("aaaaaaaaaaaa").unwrap().unwrap();
    assert_eq!(restored.confirmed_at, Some(seen), "anchor restored");
    assert!(!restored.stale, "a healthy backup restores healthy");
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

fn link(s: &dyn Store, t: EdgeType, from: &str, to: &str) -> Edge {
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
        WriteOutcome::Matched {
            node, similarity, ..
        } => {
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
        confirmed_at: None,
        approved_at: None,
        demoted_at: None,
        trust_override: None,
        trust: 0.0,
        stale: false,
        code_refs: vec![],
        tags: vec![],
        version: None,
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
    s.upsert_embeddings(&uuid_a, std::slice::from_ref(&emb))
        .unwrap();

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
    e.store().backdate_node(id, ts).unwrap();
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

    let s = SqliteStore::open(&path).unwrap();
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
    let e = Engine::new(
        SqliteStore::open(&db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
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
        SqliteStore::open(&db).unwrap(),
        Box::new(NotFake(FakeEmbedder::default())),
    );
    assert_eq!(e.ensure_embed_composition().unwrap(), 1);
    assert_eq!(e.store().embed_version().unwrap(), EMBED_COMPOSITION);
    drop(e);
    let _ = std::fs::remove_file(&db);
}

// ---- digestion tier 1: offline marker scan (PLAN §7B) --------------------

/// A fresh scan root per test — a shared fixed dir would leak files between
/// runs and break the count assertions.
fn digest_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("engram-digest-{label}-{}", id::new_id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn digest_scan_maps_markers_to_types() {
    let root = digest_root("markers");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {}\n// TODO(alice): wire the retry loop\n// FIXME broken on empty input */\n",
    )
    .unwrap();

    let scan = digest::scan(&root);
    assert_eq!(scan.candidates.len(), 2);
    assert!(!scan.truncated);

    let todo = &scan.candidates[0];
    assert_eq!(todo.marker, "TODO");
    assert_eq!(todo.suggested_type, NodeType::Intent);
    assert_eq!(todo.text, "wire the retry loop");
    assert_eq!(todo.file, "src/main.rs");
    assert_eq!(todo.line, 2);

    let fixme = &scan.candidates[1];
    assert_eq!(fixme.marker, "FIXME");
    assert_eq!(fixme.suggested_type, NodeType::Problem);
    // Comment closer stripped, text extracted without a colon separator.
    assert_eq!(fixme.text, "broken on empty input");
    assert_eq!(fixme.line, 3);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn digest_scan_respects_gitignore_and_trash_dirs() {
    let root = digest_root("ignore");
    std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
    std::fs::create_dir_all(root.join("vendor")).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("vendor/dep.js"), "// TODO ignored dependency\n").unwrap();
    std::fs::write(root.join("logs/app.log"), "TODO in a log line\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "// TODO: the real one\n").unwrap();

    let scan = digest::scan(&root);
    assert_eq!(scan.candidates.len(), 1);
    assert_eq!(scan.candidates[0].file, "src/lib.rs");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn digest_scan_skips_binary_huge_and_prose_todos() {
    let root = digest_root("robust");
    // NUL bytes = binary; counted as skipped, never an error.
    std::fs::write(root.join("blob.bin"), b"TODO\x00binary").unwrap();
    // Oversized files are generated/vendored, not hand-written markers.
    std::fs::write(root.join("big.js"), "x".repeat(1_000_001)).unwrap();
    // Lowercase "todo" in prose is not a work marker.
    std::fs::write(root.join("notes.md"), "my todo list\nmastodon api\n").unwrap();

    let scan = digest::scan(&root);
    assert!(scan.candidates.is_empty());
    assert_eq!(scan.files_skipped, 2);
    assert_eq!(scan.files_scanned, 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn digest_scan_redacts_marker_text() {
    let root = digest_root("redact");
    std::fs::write(
        root.join("cfg.rs"),
        "// TODO rotate password=hunter2-very-secret-value soon\n",
    )
    .unwrap();

    let scan = digest::scan(&root);
    assert_eq!(scan.candidates.len(), 1);
    let text = &scan.candidates[0].text;
    assert!(text.contains("[REDACTED]"), "got: {text}");
    assert!(!text.contains("hunter2"));

    let _ = std::fs::remove_dir_all(&root);
}

// ---- multi-project hub (PLAN §7C) -----------------------------------------

/// One test on purpose: it sandboxes ENGRAM_HOME (process-wide env), so the
/// whole federation story runs sequentially inside it — registry, lazy opens,
/// provenance, all-write refusal, the home brief section, and promotions.
#[test]
fn hub_federation_end_to_end() {
    let tmp = std::env::temp_dir().join(format!("engram-hub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    unsafe { std::env::set_var("ENGRAM_HOME", tmp.join("enghome")) };

    let factory: EngineFactory = Box::new(|db: &std::path::Path| {
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Io(e.to_string()))?;
        }
        Ok(Engine::new(
            SqliteStore::open(db)?,
            Box::new(FakeEmbedder::default()),
        ))
    });

    // A sibling project "beta" on the machine registry.
    let beta_root = tmp.join("beta");
    std::fs::create_dir_all(&beta_root).unwrap();
    let beta_db = beta_root.join(".engram/graph.db");
    let entry = registry::register(&beta_root, &beta_db).unwrap();
    assert_eq!(entry.name, "beta");
    // Re-registration keeps the id: upsert by root, never a duplicate.
    assert_eq!(
        registry::register(&beta_root, &beta_db).unwrap().id,
        entry.id
    );

    // The current project "alpha".
    let alpha_root = tmp.join("alpha");
    let alpha_db = alpha_root.join(".engram/graph.db");
    std::fs::create_dir_all(alpha_db.parent().unwrap()).unwrap();
    let alpha = registry::register(&alpha_root, &alpha_db).unwrap();
    let current = Engine::new(
        SqliteStore::open(&alpha_db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let hub = Hub::new(
        std::sync::Arc::new(std::sync::Mutex::new(current)),
        Some(alpha),
        Some(factory),
    );

    // Seed both graphs.
    hub.current_engine()
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Decision,
            "alpha uses tokio for its async runtime",
            "local canon",
        ))
        .unwrap();
    let beta_engine = hub.get("beta").unwrap();
    beta_engine
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Decision,
            "beta uses redb as its storage engine core",
            "sibling canon",
        ))
        .unwrap();

    // Cross-project read: the foreign hit carries provenance.
    let (hits, skipped) = hub.search_all("beta redb storage engine", &[], 8).unwrap();
    assert!(skipped.is_empty(), "{skipped:?}");
    let foreign = hits
        .iter()
        .find(|h| h.project.as_deref() == Some("beta"))
        .expect("the beta hit rides along with provenance");
    assert!(foreign.title.contains("redb"));

    // `all` never resolves to one engine — the refusal points at home.
    let err = hub
        .get("all")
        .err()
        .expect("all must not resolve")
        .to_string();
    assert!(err.contains("home"), "got: {err}");
    // An unknown selector names what exists.
    let err = hub.get("nope").err().expect("unknown selector").to_string();
    assert!(err.contains("beta"), "got: {err}");

    // The home graph: created on first access, briefed into every project.
    let home = hub.get("home").unwrap();
    home.lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Principle,
            "never store secrets anywhere",
            "user-level canon",
        ))
        .unwrap();
    let brief = hub.brief(16000).unwrap();
    assert!(brief.contains("## Home graph"), "got: {brief}");
    assert!(brief.contains("never store secrets"), "got: {brief}");
    // The roster names every other reachable graph, home last.
    assert!(
        brief.contains("## Other project graphs on this machine"),
        "got: {brief}"
    );
    assert!(brief.contains("beta, home"), "got: {brief}");

    // Promotions: the same Principle in alpha and beta nominates…
    hub.current_engine()
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Principle,
            "always run clippy with warnings denied",
            "shared habit",
        ))
        .unwrap();
    beta_engine
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Principle,
            "always run clippy with warnings denied",
            "shared habit",
        ))
        .unwrap();
    let (candidates, skipped) = hub.promotion_candidates().unwrap();
    assert!(skipped.is_empty(), "{skipped:?}");
    let cand = candidates
        .iter()
        .find(|c| c.node.title.contains("clippy"))
        .expect("recurring Principle nominates for promotion");
    assert_eq!(cand.matches[0].project, "beta");
    // …and a home copy suppresses the nomination (already promoted).
    home.lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Principle,
            "always run clippy with warnings denied",
            "promoted",
        ))
        .unwrap();
    let (candidates, _) = hub.promotion_candidates().unwrap();
    assert!(
        !candidates.iter().any(|c| c.node.title.contains("clippy")),
        "a home copy suppresses the nomination"
    );

    // projects() lists everything with the right flags.
    let projects = hub.projects();
    assert!(projects.iter().any(|p| p.current && p.name == "alpha"));
    assert!(projects.iter().any(|p| p.home));
    assert!(projects.iter().any(|p| p.name == "beta" && p.open));

    unsafe { std::env::remove_var("ENGRAM_HOME") };
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---- Store-trait conformance: both backends, one battery (PLAN §7C) --------
//
// The trait's contract is what the migration relies on: whatever passed on
// SQLite must pass unchanged on TepinDB. Backend-specific behavior (WAL,
// legacy-id shortening, PRAGMAs) stays in the per-backend tests above.

fn store_battery(s: &dyn Store, backend: &str) {
    // -- nodes: create / read / update semantics
    let mut spec = new_node(NodeType::Decision, "use sqlite WAL mode", "because readers");
    spec.tags = vec!["Phase 2".into(), "phase-2".into()];
    spec.code_refs = vec!["crates/engram-core/src/store.rs".into()];
    let a = s.add_node(spec).unwrap();
    assert_eq!(a.tags, vec!["phase-2"], "tags normalize + dedupe on write");
    assert!(a.valid_from.is_some());
    assert!(a.approved_at.is_none(), "claude nodes start unapproved");

    let user = {
        let mut n = new_node(NodeType::Principle, "user knows best", "");
        n.source = Source::User;
        s.add_node(n).unwrap()
    };
    assert!(
        user.approved_at.is_some(),
        "user nodes approved by construction"
    );

    let secret = s
        .add_node(new_node(
            NodeType::Insight,
            "key AKIAIOSFODNN7EXAMPLE leaked",
            "",
        ))
        .unwrap();
    assert!(
        !secret.title.contains("AKIA"),
        "redaction runs on {backend}"
    );

    let updated = s
        .update_node(
            &a.id,
            NodePatch {
                title: Some("use sqlite WAL journaling".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.title, "use sqlite WAL journaling");
    assert!(
        updated.confirmed_at.is_some(),
        "update stamps the trust anchor"
    );

    // -- approve / revoke / pin / demote / backdate
    let approved = s.approve(&a.id).unwrap();
    assert!(approved.approved_at.is_some());
    let revoked = s.revoke_approval(&a.id).unwrap();
    assert!(revoked.approved_at.is_none());
    let pinned = s.set_trust_override(&a.id, Some(2.0)).unwrap();
    assert_eq!(pinned.trust_override, Some(1.0), "pin clamps to 0..=1");
    assert!(
        !s.demote(&a.id, now()).unwrap(),
        "pinned nodes never demote"
    );
    s.set_trust_override(&a.id, None).unwrap();
    assert!(s.demote(&a.id, now()).unwrap());
    assert!(
        !s.demote(&a.id, now()).unwrap(),
        "second demotion is a no-op"
    );
    let cleared = s.clear_demotion(&a.id).unwrap();
    assert!(cleared.demoted_at.is_none());
    s.backdate_node(&a.id, 1000).unwrap();
    assert_eq!(s.get_node(&a.id).unwrap().unwrap().created_at, 1000);

    // -- edges
    let e1 = s
        .add_edge(NewEdge {
            edge_type: EdgeType::Because,
            from_id: a.id.clone(),
            to_id: user.id.clone(),
            source: Source::Claude,
            confidence: None,
            strength: None,
            note: Some("triple reads as a sentence".into()),
            status: None,
        })
        .unwrap();
    assert!(
        s.add_edge(NewEdge {
            edge_type: EdgeType::About,
            from_id: a.id.clone(),
            to_id: "missing-node".into(),
            source: Source::Claude,
            confidence: None,
            strength: None,
            note: None,
            status: None,
        })
        .is_err(),
        "dangling endpoints refused on {backend}"
    );
    assert_eq!(s.edges_out(&a.id).unwrap().len(), 1);
    assert_eq!(s.edges_in(&user.id).unwrap().len(), 1);
    assert!(s.pair_linked(&a.id, &user.id).unwrap());
    assert!(s.pair_linked(&user.id, &a.id).unwrap());
    assert!(!s.pair_linked(&a.id, &secret.id).unwrap());

    let conflict = s
        .add_edge(NewEdge {
            edge_type: EdgeType::ConflictsWith,
            from_id: secret.id.clone(),
            to_id: a.id.clone(),
            source: Source::Claude,
            confidence: None,
            strength: None,
            note: None,
            status: Some(EdgeStatus::Active),
        })
        .unwrap();
    assert!(s.has_active_conflict(&a.id).unwrap());
    assert_eq!(s.active_conflict_edges().unwrap().len(), 1);
    assert_eq!(s.nodes_in_active_conflicts().unwrap().len(), 2);
    let resolved = s
        .update_edge(
            &conflict.id,
            EdgePatch {
                status: Some(EdgeStatus::Resolved),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(resolved.status, Some(EdgeStatus::Resolved));
    assert!(!s.has_active_conflict(&a.id).unwrap());
    let neighbors = s.neighbors(&a.id, 5).unwrap();
    assert_eq!(neighbors.len(), 2);
    assert!(s.delete_edge(&conflict.id).unwrap());
    assert!(!s.delete_edge(&conflict.id).unwrap());
    assert_eq!(
        s.get_edge(&e1.id).unwrap().unwrap().note.as_deref(),
        Some("triple reads as a sentence")
    );

    // -- traversal
    let (t_nodes, t_edges) = s.traverse(&a.id, &[], 2).unwrap();
    assert_eq!(t_nodes.len(), 2);
    assert_eq!(t_edges.len(), 1);

    // -- keyword search: full-field composition + sentinel snippets
    let tagged = {
        let mut n = new_node(NodeType::Caution, "quirky behavior", "plain text body");
        n.tags = vec!["zanzibar-quirk".into()];
        n.code_refs = vec!["crates/engram-http/src/lib.rs".into()];
        s.add_node(n).unwrap()
    };
    let by_title = s.search_fts("journaling", &[], 10).unwrap();
    assert_eq!(by_title.len(), 1, "title match on {backend}");
    assert!(
        by_title[0].snippet.contains(SNIPPET_OPEN),
        "snippet marks matches"
    );
    let by_tag = s.search_fts("zanzibar", &[], 10).unwrap();
    assert_eq!(by_tag.len(), 1, "tags are indexed on {backend}");
    assert_eq!(by_tag[0].id, tagged.id);
    let typed = s
        .search_fts("journaling", &[NodeType::Insight], 10)
        .unwrap();
    assert!(typed.is_empty(), "type filter applies");

    // -- vectors: 384-wide like the default model
    let emb = FakeEmbedder::default();
    let va = emb.embed_one("use sqlite WAL journaling").unwrap();
    let vb = emb.embed_one("completely unrelated topic zebra").unwrap();
    s.upsert_embeddings(&a.id, std::slice::from_ref(&va))
        .unwrap();
    s.upsert_embeddings(&secret.id, std::slice::from_ref(&vb))
        .unwrap();
    let knn = s.search_vec(&va, 2).unwrap();
    assert_eq!(knn[0].0, a.id, "nearest neighbor first on {backend}");
    assert!(knn[0].1 < knn[1].1, "distances ascend");
    assert_eq!(s.embedding_of(&a.id).unwrap().unwrap().len(), 384);
    let hybrid = s.search_hybrid("journaling", Some(&va), &[], 5).unwrap();
    assert_eq!(hybrid[0].id, a.id);

    // -- archived nodes vanish from reads
    s.archive_nodes(std::slice::from_ref(&tagged.id), now())
        .unwrap();
    assert!(s.search_fts("zanzibar", &[], 10).unwrap().is_empty());
    assert!(
        s.get_node(&tagged.id)
            .unwrap()
            .unwrap()
            .valid_until
            .is_some(),
        "archive keeps history"
    );

    // -- suspects
    let sus = s
        .add_suspect(&a.id, &secret.id, 0.91, Some(("contradiction", 0.88, None)))
        .unwrap();
    assert!(
        s.suspect_between(&secret.id, &a.id).unwrap(),
        "either order"
    );
    let pending = s.suspects_pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].nli_label.as_deref(), Some("contradiction"));
    let judged = s
        .set_suspect_status(&sus.id, SuspectStatus::Dismissed)
        .unwrap();
    assert_eq!(judged.status, SuspectStatus::Dismissed);
    assert!(s.suspects_pending().unwrap().is_empty());

    // -- audit journal
    for i in 0..3 {
        s.add_audit(&AuditEntry {
            seq: 0,
            ts: 100 + i,
            action: "created".into(),
            entity: "node".into(),
            entity_id: if i == 2 { "other".into() } else { a.id.clone() },
            title: None,
            before: None,
            after: Some(serde_json::json!({"i": i})),
            origin: "library".into(),
            session_id: None,
            cwd: None,
            pid: None,
            version: None,
        })
        .unwrap();
    }
    let page = s.audit_page(None, None, 2).unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.entries.len(), 2);
    assert!(page.entries[0].seq > page.entries[1].seq, "newest first");
    let next = s.audit_page(Some(page.entries[1].seq), None, 10).unwrap();
    assert_eq!(next.entries.len(), 1);
    let filtered = s.audit_page(None, Some("other"), 10).unwrap();
    assert_eq!(filtered.total, 1);

    // -- worklists / brief reads
    let open = {
        let mut n = new_node(NodeType::Problem, "flaky test", "");
        n.status = Some(NodeStatus::Open);
        s.add_node(n).unwrap()
    };
    assert_eq!(s.list_open(&[]).unwrap().len(), 1);
    assert_eq!(s.count_by_type_active(&NodeType::Problem).unwrap(), 1);
    assert_eq!(
        s.nodes_by_type_active(&NodeType::Principle, 5).unwrap()[0].id,
        user.id
    );
    assert!(s.recent_nodes(2).unwrap().len() == 2);
    assert!(
        s.scannable_nodes()
            .unwrap()
            .iter()
            .all(|n| n.node_type != NodeType::Anchor)
    );

    // -- decay: an old, stale, claude-authored episodic note is a candidate
    let dusty = {
        let mut n = new_node(NodeType::Insight, "temporary observation", "");
        n.durability = Durability::Episodic;
        s.add_node(n).unwrap()
    };
    s.backdate_node(&dusty.id, now() - 400 * 24 * 3600).unwrap();
    let candidates = s.decay_candidates(14 * 24 * 3600, now()).unwrap();
    assert!(candidates.iter().any(|n| n.id == dusty.id));
    assert!(
        !candidates.iter().any(|n| n.id == open.id),
        "stable nodes never decay"
    );

    // -- tag stats (archived nodes excluded)
    let stats = s.tag_stats(10).unwrap();
    assert!(!stats.iter().any(|t| t.tag == "zanzibar-quirk"));

    // -- store metadata
    assert_eq!(s.embed_version().unwrap(), 0);
    s.set_embed_version(2).unwrap();
    assert_eq!(s.embed_version().unwrap(), 2);
    assert!(s.embed_model().unwrap().is_none());
    s.set_embed_model(&EmbedModelId {
        name: "bge-small-en-v1.5".into(),
        dim: 384,
    })
    .unwrap();
    assert_eq!(s.embed_model().unwrap().unwrap().dim, 384);
    let stats = s.stats().unwrap();
    assert_eq!(stats.backend, backend);
    assert!(stats.nodes >= 5);
    assert_eq!(stats.embedded, 2);
    assert!(s.health().unwrap().integrity_ok);

    // -- reset_vectors: the model-swap path — vectors gone, new width accepted
    s.reset_vectors(4).unwrap();
    assert!(
        s.embedding_of(&a.id).unwrap().is_none(),
        "reset drops vectors"
    );
    assert_eq!(
        s.get_node(&a.id).unwrap().unwrap().title,
        "use sqlite WAL journaling",
        "reset keeps documents"
    );
    s.upsert_embeddings(&a.id, &[vec![0.5, 0.5, 0.5, 0.5]])
        .unwrap();
    assert_eq!(s.embedding_of(&a.id).unwrap().unwrap().len(), 4);
    assert_eq!(s.search_vec(&[0.5, 0.5, 0.5, 0.5], 1).unwrap()[0].0, a.id);

    // -- hard delete cascades edges + suspects
    assert!(s.delete_node(&a.id).unwrap());
    assert!(s.get_node(&a.id).unwrap().is_none());
    assert!(s.edges_out(&a.id).unwrap().is_empty());
    assert!(s.edges_in(&user.id).unwrap().is_empty());
    assert!(!s.suspect_between(&a.id, &secret.id).unwrap());
    assert!(!s.delete_node(&a.id).unwrap());
}

#[test]
fn sqlite_store_conformance() {
    store_battery(&SqliteStore::open_in_memory().unwrap(), "sqlite");
}

#[test]
fn tepin_store_conformance() {
    store_battery(&TepinStore::open_in_memory().unwrap(), "tepindb");
}

#[test]
fn tepin_store_survives_reopen_and_rebuild_on_disk() {
    let dir = std::env::temp_dir().join(format!("engram-tepin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("graph.tepin");
    let _ = std::fs::remove_file(&path);
    assert!(is_tepin_path(&path));

    let id = {
        let s = TepinStore::open(&path).unwrap();
        let n = s
            .add_node(new_node(NodeType::Decision, "durable across reopen", ""))
            .unwrap();
        s.upsert_embeddings(
            &n.id,
            &[FakeEmbedder::default().embed_one("durable").unwrap()],
        )
        .unwrap();
        n.id
    };
    {
        let s = TepinStore::open(&path).unwrap();
        assert!(s.get_node(&id).unwrap().is_some());
        assert!(s.embedding_of(&id).unwrap().is_some());
        // The on-disk rebuild path: file swapped in place, handle stays live.
        s.reset_vectors(8).unwrap();
        assert!(s.embedding_of(&id).unwrap().is_none());
        assert!(s.get_node(&id).unwrap().is_some());
        s.upsert_embeddings(&id, &[vec![1.0; 8]]).unwrap();
    }
    let s = TepinStore::open(&path).unwrap();
    assert_eq!(s.embedding_of(&id).unwrap().unwrap().len(), 8);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_import_moves_a_graph_between_backends() {
    // The §7C step-5 migration vehicle in miniature: JSON out of SQLite,
    // into TepinDB, embeddings regenerated by the importing engine.
    let src = engine();
    let a = src
        .add_node(new_node(NodeType::Decision, "adopt tepindb", "driver swap"))
        .unwrap();
    let b = src
        .add_node(new_node(
            NodeType::Principle,
            "storage is an open-time choice",
            "",
        ))
        .unwrap();
    link(src.store(), EdgeType::Because, &a.id, &b.id);
    let graph = src.export().unwrap();

    let dst = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let summary = dst.import(graph).unwrap();
    assert_eq!(summary.nodes, 2);
    assert_eq!(summary.edges, 1);

    let hits = dst.search("tepindb driver", &[], 5).unwrap();
    assert_eq!(hits[0].id, a.id);
    assert!(
        hits[0].neighbors.iter().any(|n| n.id == b.id),
        "edges survive the backend move"
    );
    // Round-trip back out: the export is backend-independent.
    let back = dst.export().unwrap();
    assert_eq!(back.nodes.len(), 2);
    assert_eq!(back.edges.len(), 1);
}

#[test]
fn graph_config_roundtrips_on_both_backends() {
    let engines = [
        engine(),
        Engine::new(
            TepinStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ),
    ];
    for e in engines {
        assert_eq!(
            e.graph_config(),
            GraphConfig::default(),
            "a never-customized graph runs on the shipped defaults"
        );
        let mut cfg = GraphConfig::default();
        cfg.brief.recent.cap = 3;
        cfg.policy.volatile_window_days = 10;
        cfg.ontology.types[0].hue = 100;
        e.set_graph_config(&cfg).unwrap();
        assert_eq!(e.graph_config(), cfg);
        // A corrupt stored document reads as defaults — config must never
        // brick a store open.
        e.store().set_graph_config("{ not json").unwrap();
        assert_eq!(e.graph_config(), GraphConfig::default());
    }
}

#[test]
fn graph_config_validation_guards_hard_invariants() {
    let ok = GraphConfig::default();
    ok.validate().unwrap();

    let mut two_supersessions = ok.clone();
    two_supersessions.ontology.verbs[0].roles.supersession = true;
    assert!(two_supersessions.validate().is_err());

    let mut no_contradiction = ok.clone();
    no_contradiction
        .ontology
        .verbs
        .retain(|v| !v.roles.contradiction);
    assert!(no_contradiction.validate().is_err());

    let mut dup_type = ok.clone();
    dup_type.ontology.types[1].name = "principle".into();
    assert!(
        dup_type.validate().is_err(),
        "type names dedupe case-insensitively"
    );

    let mut spaced_verb = ok.clone();
    spaced_verb.ontology.verbs[0].name = "relates to".into();
    assert!(
        spaced_verb.validate().is_err(),
        "verbs stay sentence-shaped"
    );

    let mut unordered = ok.clone();
    unordered.policy.trust_confirmed = 0.4;
    assert!(
        unordered.validate().is_err(),
        "trust anchors keep their order"
    );

    // The engine refuses to store an invalid document.
    let mut bad_hue = ok.clone();
    bad_hue.ontology.types[0].hue = 360;
    let e = engine();
    assert!(e.set_graph_config(&bad_hue).is_err());
    assert_eq!(e.graph_config(), GraphConfig::default());
}

#[test]
fn export_embeds_custom_config_and_import_restores_it() {
    let src = engine();
    let bare = src.export().unwrap();
    assert!(bare.config.is_none(), "default graphs export bare");
    assert!(
        !serde_json::to_string(&bare).unwrap().contains("\"config\""),
        "pre-0.7 dumps and default dumps stay identical in shape"
    );

    let mut cfg = GraphConfig::default();
    cfg.ontology.preset = "custom".into();
    cfg.brief.ontology.show = true;
    src.set_graph_config(&cfg).unwrap();
    let dump = src.export().unwrap();
    assert_eq!(dump.config.as_ref(), Some(&cfg));

    let dst = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    dst.import(dump).unwrap();
    assert_eq!(dst.graph_config(), cfg, "the dump restores its ontology");
}

#[test]
fn migrate_to_tepin_publishes_only_a_complete_store() {
    let dir = std::env::temp_dir().join(format!("engram-migrate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("graph.db");
    let (a_id, b_id) = {
        let src = Engine::new(
            SqliteStore::open(&db).unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        let a = src
            .add_node(new_node(NodeType::Decision, "adopt tepindb", "driver swap"))
            .unwrap();
        let b = src
            .add_node(new_node(
                NodeType::Principle,
                "storage is an open-time choice",
                "",
            ))
            .unwrap();
        link(src.store(), EdgeType::Because, &a.id, &b.id);
        let mut cfg = GraphConfig::default();
        cfg.ontology.preset = "custom".into();
        src.set_graph_config(&cfg).unwrap();
        (a.id, b.id)
    };
    // A crashed earlier attempt's leftover is disposable, never fatal.
    std::fs::write(dir.join("graph.tepin.part"), b"junk").unwrap();

    let summary = migrate_to_tepin(
        &db,
        std::sync::Arc::new(FakeEmbedder::default()),
        AuditOrigin::cli(),
    )
    .unwrap();
    assert_eq!((summary.nodes, summary.edges), (2, 1));
    assert_eq!(summary.dst, dir.join("graph.tepin"));
    assert!(summary.dst.is_file());
    assert!(
        !dir.join("graph.tepin.part").exists(),
        "the build file is cleaned up after publishing"
    );
    assert!(db.is_file(), "the SQLite source stays as the backup");
    assert_eq!(
        resolve_db_path(&db),
        summary.dst,
        "every open now picks the migrated store"
    );

    let dst = Engine::new(
        TepinStore::open(&summary.dst).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let hits = dst.search("tepindb driver", &[], 5).unwrap();
    assert_eq!(hits[0].id, a_id);
    assert!(
        hits[0].neighbors.iter().any(|n| n.id == b_id),
        "edges survive the migration"
    );
    assert_eq!(
        dst.graph_config().ontology.preset,
        "custom",
        "the graph's configuration travels with the migration"
    );
    drop(dst);

    // An existing target refuses — the CLI's --force removes it first, and
    // the auto-migration can never race past resolve_db_path to get here.
    assert!(
        migrate_to_tepin(
            &db,
            std::sync::Arc::new(FakeEmbedder::default()),
            AuditOrigin::cli(),
        )
        .is_err()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_db_prefers_a_migrated_tepin_sibling() {
    let dir = std::env::temp_dir().join(format!("engram-resolve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("graph.db");
    assert_eq!(
        resolve_db_path(&db),
        dir.join("graph.tepin"),
        "a brand-new store is born tepin (v0.6.2 default)"
    );
    std::fs::write(&db, b"sqlite bytes").unwrap();
    assert_eq!(
        resolve_db_path(&db),
        db,
        "an existing graph.db keeps working"
    );
    std::fs::write(dir.join("graph.tepin"), b"x").unwrap();
    assert_eq!(
        resolve_db_path(&db),
        dir.join("graph.tepin"),
        "a migrated sibling wins without touching any wiring"
    );
    let explicit = dir.join("graph.tepin");
    assert_eq!(resolve_db_path(&explicit), explicit);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- embedding-model guard (PLAN §7A model selection) ----------------------

/// A real-shaped (non-fake) embedder with a chosen identity and width.
struct NamedEmbedder {
    model: &'static str,
    width: usize,
}

impl Embedder for NamedEmbedder {
    fn dim(&self) -> usize {
        self.width
    }
    fn name(&self) -> &str {
        self.model
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.1f32; self.width];
                for &b in t.as_bytes() {
                    v[b as usize % self.width] += 1.0;
                }
                v
            })
            .collect())
    }
}

#[test]
fn embed_model_guard_rebuilds_vectors_on_model_swap() {
    let mut e = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(NamedEmbedder {
            model: "model-a",
            width: 8,
        }),
    );
    // Non-default active model over a virgin store: vector storage reshapes
    // before any write, identity is stamped.
    e.ensure_embed_model().unwrap();
    assert_eq!(e.store().embed_model().unwrap().unwrap().name, "model-a");
    let n = e
        .add_node(new_node(NodeType::Decision, "guarded vectors", ""))
        .unwrap();
    assert_eq!(e.store().embedding_of(&n.id).unwrap().unwrap().len(), 8);

    // Swap the model: the guard rebuilds and re-embeds the whole graph once.
    e.set_embedder(Box::new(NamedEmbedder {
        model: "model-b",
        width: 4,
    }));
    assert_eq!(e.ensure_embed_model().unwrap(), 1);
    let stored = e.store().embed_model().unwrap().unwrap();
    assert_eq!((stored.name.as_str(), stored.dim), ("model-b", 4));
    assert_eq!(e.store().embedding_of(&n.id).unwrap().unwrap().len(), 4);
    assert_eq!(
        e.ensure_embed_model().unwrap(),
        0,
        "idempotent once stamped"
    );
}

#[test]
fn fake_embedder_skips_the_model_guard_entirely() {
    let e = engine();
    e.add_node(new_node(NodeType::Insight, "fake vectors", ""))
        .unwrap();
    assert_eq!(e.ensure_embed_model().unwrap(), 0);
    assert!(
        e.store().embed_model().unwrap().is_none(),
        "a fake open must never stamp or rebuild"
    );
}

#[test]
fn cortex_config_defaults_and_presets() {
    use crate::cortex::{CortexConfig, Role, presets, spec_files};
    let cfg = CortexConfig::default();
    assert_eq!(
        cfg.effective(Role::Embedding).name,
        rag::DEFAULT_EMBED_MODEL
    );
    assert_eq!(cfg.effective(Role::Embedding).dim, Some(384));
    assert_eq!(
        cfg.effective(Role::Reranker).name,
        "jina-reranker-v1-turbo-en"
    );
    assert_eq!(cfg.effective(Role::Nli).name, "nli-deberta-v3-small");
    // NLI needs three files, the fastembed-loaded roles five.
    assert_eq!(spec_files(Role::Nli, &presets(Role::Nli)[0]).len(), 3);
    assert_eq!(
        spec_files(Role::Embedding, &presets(Role::Embedding)[0]).len(),
        5
    );
    // A custom selection round-trips through JSON with the default model_file.
    let spec: crate::cortex::ModelSpec = serde_json::from_str(
        r#"{"name":"my-model","base_url":"https://example.com/m","dim":512,"pooling":"mean"}"#,
    )
    .unwrap();
    assert_eq!(spec.model_file, "onnx/model_quantized.onnx");
    assert_eq!(spec.dim, Some(512));
}

// ---- TepinDB capability checks -----------------------------------------
//
// The db-level behaviors the driver leans on, exercised the way real use
// exercises them: index-served lookups at volume, sequences of writes and
// reads across reopens, counter/meta durability, and serialized multi-thread
// access. The conformance battery proves API parity; these prove the file
// underneath keeps its promises.

#[test]
fn tepin_edge_indexes_answer_correctly_at_volume() {
    let s = TepinStore::open_in_memory().unwrap();
    let nodes: Vec<Node> = (0..40)
        .map(|i| {
            s.add_node(new_node(
                NodeType::Decision,
                &format!("volume node {i}"),
                "",
            ))
            .unwrap()
        })
        .collect();
    // A chain (builds-on) plus a hub (everything about node 0): the shape
    // that makes the from_id/to_id equality indexes earn their keep.
    for i in 1..40 {
        s.add_edge(NewEdge {
            edge_type: EdgeType::BuildsOn,
            from_id: nodes[i].id.clone(),
            to_id: nodes[i - 1].id.clone(),
            source: Source::Claude,
            confidence: None,
            strength: None,
            note: None,
            status: None,
        })
        .unwrap();
        s.add_edge(NewEdge {
            edge_type: EdgeType::About,
            from_id: nodes[i].id.clone(),
            to_id: nodes[0].id.clone(),
            source: Source::Claude,
            confidence: None,
            strength: None,
            note: None,
            status: None,
        })
        .unwrap();
    }
    // Index-served reads must agree exactly with a full scan.
    let all = s.all_edges().unwrap();
    assert_eq!(all.len(), 78);
    for n in [&nodes[0], &nodes[5], &nodes[39]] {
        let scan_in = all.iter().filter(|e| e.to_id == n.id).count();
        let scan_out = all.iter().filter(|e| e.from_id == n.id).count();
        assert_eq!(s.edges_in(&n.id).unwrap().len(), scan_in, "to_id index");
        assert_eq!(s.edges_out(&n.id).unwrap().len(), scan_out, "from_id index");
    }
    assert_eq!(s.edges_in(&nodes[0].id).unwrap().len(), 40); // 39 about + 1 builds-on
    assert_eq!(s.edges_out(&nodes[5].id).unwrap().len(), 2);
    // Traversal over the indexed edges reaches the whole chain.
    let (t_nodes, t_edges) = s.traverse(&nodes[0].id, &[EdgeType::BuildsOn], 39).unwrap();
    assert_eq!(t_nodes.len(), 40);
    assert_eq!(t_edges.len(), 39);
}

#[test]
fn tepin_write_read_sequences_hold_across_reopen() {
    let dir = std::env::temp_dir().join(format!("engram-tepin-seq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("graph.tepin");
    let _ = std::fs::remove_file(&path);

    let (a_id, b_id, suspect_id);
    {
        let s = TepinStore::open(&path).unwrap();
        let a = s
            .add_node(new_node(NodeType::Decision, "alpha decision", "first body"))
            .unwrap();
        let b = s
            .add_node(new_node(NodeType::Principle, "beta principle", ""))
            .unwrap();
        s.add_edge(NewEdge {
            edge_type: EdgeType::Because,
            from_id: a.id.clone(),
            to_id: b.id.clone(),
            source: Source::Claude,
            confidence: None,
            strength: None,
            note: None,
            status: None,
        })
        .unwrap();
        for i in 0..2 {
            s.add_audit(&AuditEntry {
                seq: 0,
                ts: 100 + i,
                action: "created".into(),
                entity: "node".into(),
                entity_id: a.id.clone(),
                title: None,
                before: None,
                after: None,
                origin: "library".into(),
                session_id: None,
                cwd: None,
                pid: None,
                version: None,
            })
            .unwrap();
        }
        s.set_embed_version(2).unwrap();
        s.set_embed_model(&EmbedModelId {
            name: "bge-small-en-v1.5".into(),
            dim: 384,
        })
        .unwrap();
        let sus = s.add_suspect(&a.id, &b.id, 0.9, None).unwrap();
        s.set_suspect_status(&sus.id, SuspectStatus::Dismissed)
            .unwrap();
        // A text update re-indexes keywords synchronously, same txn.
        s.update_node(
            &a.id,
            NodePatch {
                title: Some("alpha decision revised".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.search_fts("revised", &[], 5).unwrap().len(), 1);
        (a_id, b_id, suspect_id) = (a.id.clone(), b.id.clone(), sus.id.clone());
    }
    {
        // Reopen 1: every write is there, the audit counter continues.
        let s = TepinStore::open(&path).unwrap();
        assert_eq!(s.all_nodes().unwrap().len(), 2);
        assert_eq!(
            s.get_node(&a_id).unwrap().unwrap().title,
            "alpha decision revised"
        );
        assert_eq!(s.edges_out(&a_id).unwrap().len(), 1);
        assert_eq!(s.embed_version().unwrap(), 2);
        assert_eq!(s.embed_model().unwrap().unwrap().dim, 384);
        assert_eq!(
            s.get_suspect(&suspect_id).unwrap().unwrap().status,
            SuspectStatus::Dismissed
        );
        assert!(
            s.suspect_between(&b_id, &a_id).unwrap(),
            "judgment memory survives"
        );
        assert_eq!(s.search_fts("revised", &[], 5).unwrap().len(), 1);

        let page = s.audit_page(None, None, 10).unwrap();
        assert_eq!(page.total, 2);
        s.add_audit(&AuditEntry {
            seq: 0,
            ts: 300,
            action: "updated".into(),
            entity: "node".into(),
            entity_id: a_id.clone(),
            title: None,
            before: None,
            after: None,
            origin: "library".into(),
            session_id: None,
            cwd: None,
            pid: None,
            version: None,
        })
        .unwrap();
        let page = s.audit_page(None, None, 10).unwrap();
        assert_eq!(
            page.entries[0].seq, 3,
            "seq continues after reopen, never reuses"
        );
        assert!(s.delete_node(&a_id).unwrap());
        assert!(s.search_fts("revised", &[], 5).unwrap().is_empty());
    }
    {
        // Reopen 2: the delete and its cascade are durable.
        let s = TepinStore::open(&path).unwrap();
        assert!(s.get_node(&a_id).unwrap().is_none());
        assert!(s.get_node(&b_id).unwrap().is_some());
        assert!(s.edges_in(&b_id).unwrap().is_empty());
        assert_eq!(s.audit_page(None, None, 10).unwrap().total, 3);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tepin_interleaved_updates_keep_search_and_vectors_consistent() {
    // Engine-level (embed + index on every write), mirroring the sqlite
    // re-embed-on-update behavior on the tepin backend.
    let e = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let a = e
        .add_node(new_node(
            NodeType::Decision,
            "cache invalidation policy",
            "ttl",
        ))
        .unwrap();
    let b = e
        .add_node(new_node(NodeType::Insight, "zebra crossing patterns", ""))
        .unwrap();
    assert_eq!(e.search("invalidation", &[], 5).unwrap()[0].id, a.id);

    // Update the text: keyword index and vector must both follow.
    let before = e.store().embedding_of(&a.id).unwrap().unwrap();
    e.update_node(
        &a.id,
        NodePatch {
            title: Some("memoization eviction strategy".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let after = e.store().embedding_of(&a.id).unwrap().unwrap();
    assert_ne!(before, after, "text update re-embeds on tepin");
    // Keyword primitive: the old title's terms are gone from the index
    // (hybrid may still surface the node semantically — that's the vector
    // half doing its job, not staleness).
    assert!(
        e.store()
            .search_fts("invalidation", &[], 5)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        e.search("memoization eviction", &[], 5).unwrap()[0].id,
        a.id
    );

    // Archive: vector still stored, but no read surface shows the node.
    e.store()
        .archive_nodes(std::slice::from_ref(&a.id), now())
        .unwrap();
    assert!(e.store().embedding_of(&a.id).unwrap().is_some());
    assert!(e.search("memoization eviction", &[], 5).unwrap().is_empty());

    // Delete: the doc takes its vectors with it.
    e.store().delete_node(&b.id).unwrap();
    assert!(e.store().embedding_of(&b.id).unwrap().is_none());
}

#[test]
fn tepin_import_is_idempotent_at_volume() {
    let mk_node = |i: usize| Node {
        id: format!("00volnode{i:03}"),
        node_type: NodeType::Insight,
        title: format!("imported insight number {i}"),
        body: Some(format!("body of note {i}")),
        durability: Durability::Stable,
        source: Source::Claude,
        session_id: Some("bulk".into()),
        created_at: 1_000_000 + i as i64,
        valid_from: Some(1_000_000 + i as i64),
        valid_until: None,
        status: None,
        last_seen: None,
        confirmed_at: None,
        approved_at: None,
        demoted_at: None,
        trust_override: None,
        trust: 0.0,
        stale: false,
        version: None,
        code_refs: vec![],
        tags: vec!["bulk".into()],
    };
    let nodes: Vec<Node> = (0..120).map(mk_node).collect();
    let edges: Vec<Edge> = (1..120)
        .map(|i| Edge {
            id: format!("00voledge{i:03}"),
            edge_type: EdgeType::BuildsOn,
            from_id: nodes[i].id.clone(),
            to_id: nodes[i - 1].id.clone(),
            source: Source::Claude,
            created_at: 1_000_000,
            confidence: None,
            strength: None,
            note: None,
            valid_from: None,
            valid_until: None,
            status: None,
        })
        .collect();

    let e = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    for round in 0..2 {
        let summary = e
            .import(ExportGraph {
                config: None,
                version: 1,
                nodes: nodes.clone(),
                edges: edges.clone(),
            })
            .unwrap();
        assert_eq!((summary.nodes, summary.edges), (120, 119), "round {round}");
        let stats = e.store().stats().unwrap();
        assert_eq!(
            (stats.nodes, stats.edges),
            (120, 119),
            "no dupes on re-import"
        );
        assert_eq!(stats.embedded, 120, "every import round re-embeds all");
    }
    assert_eq!(e.search("imported insight", &[], 5).unwrap().len(), 5);
    assert_eq!(e.store().tag_stats(5).unwrap()[0].count, 120);
}

#[test]
fn tepin_engine_stays_consistent_under_threaded_access() {
    // The daemon serializes every engine behind a mutex; hammer that shape
    // from many threads — interleaved writes, reads, searches — and the
    // counts must come out exact.
    use std::sync::{Arc, Mutex};
    let engine = Arc::new(Mutex::new(Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    )));
    let mut handles = Vec::new();
    for t in 0..8 {
        let engine = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            for i in 0..15 {
                let e = engine.lock().unwrap();
                let n = e
                    .add_node(new_node(
                        NodeType::Insight,
                        &format!("thread {t} note {i}"),
                        "",
                    ))
                    .unwrap();
                assert!(!e.search(&format!("thread {t}"), &[], 3).unwrap().is_empty());
                e.store().touch(std::slice::from_ref(&n.id)).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let e = engine.lock().unwrap();
    let stats = e.store().stats().unwrap();
    assert_eq!(stats.nodes, 120);
    assert_eq!(stats.embedded, 120);
    let page = e.store().audit_page(None, None, 1).unwrap();
    assert_eq!(page.total, 120, "one journal row per write, none lost");
}

#[test]
fn tepin_embed_model_swap_keeps_writes_working() {
    // The v0.6.0 live bug: tepin pins each vector write with the store's
    // RECORDED model name, so stamping the new identity after the re-embed
    // loop pinned the file under the old name — and every later write died
    // with embedder_mismatch. The guard now stamps before it re-embeds.
    let mut e = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(NamedEmbedder {
            model: "bge-small-en-v1.5",
            width: 8,
        }),
    );
    e.ensure_embed_model().unwrap();
    e.add_node(new_node(NodeType::Decision, "before the swap", ""))
        .unwrap();

    // The killer case: same width, different name (bge-small → bge-base).
    e.set_embedder(Box::new(NamedEmbedder {
        model: "bge-base-en-v1.5",
        width: 8,
    }));
    assert_eq!(e.ensure_embed_model().unwrap(), 1);
    let after = e
        .add_node(new_node(NodeType::Decision, "after the swap", ""))
        .unwrap();
    assert!(
        e.store().embedding_of(&after.id).unwrap().is_some(),
        "writes keep embedding under the new identity"
    );
    let stored = e.store().embed_model().unwrap().unwrap();
    assert_eq!(stored.name, "bge-base-en-v1.5");

    // And the matching-identity path backfills any vector gaps (mid-swap
    // crash healing): simulate a gap via a raw import (no vectors written).
    let orphan = Node {
        id: "00gaplessnode".into(),
        node_type: NodeType::Insight,
        title: "imported without a vector".into(),
        body: None,
        durability: Durability::Stable,
        source: Source::Claude,
        session_id: None,
        created_at: 1,
        valid_from: Some(1),
        valid_until: None,
        status: None,
        last_seen: None,
        confirmed_at: None,
        approved_at: None,
        demoted_at: None,
        trust_override: None,
        trust: 0.0,
        stale: false,
        code_refs: vec![],
        tags: vec![],
        version: None,
    };
    e.store().import_raw(&[orphan], &[]).unwrap();
    assert!(e.store().embedding_of("00gaplessnode").unwrap().is_none());
    assert_eq!(e.ensure_embed_model().unwrap(), 1, "heals exactly the gap");
    assert!(e.store().embedding_of("00gaplessnode").unwrap().is_some());
}

#[test]
fn default_preset_cache_dirs_match_the_legacy_loader_dirs() {
    // The provisioning fix for the default models composes with the legacy
    // loaders ONLY because cortex's cache layout and the pre-selection dirs
    // agree — provision() downloads into the exact dir FastEmbedder::new /
    // FastReranker::new / FastNli::new prefer. Pin that invariant.
    use crate::cortex::{Role, cache_dir, presets};
    assert_eq!(
        cache_dir(&presets(Role::Embedding)[0].name),
        rag::model_dir(),
        "embedding default provisions where the loader looks"
    );
    assert_eq!(
        cache_dir(&presets(Role::Reranker)[0].name),
        rag::reranker_model_dir(),
        "reranker default provisions where the loader looks"
    );
    assert_eq!(
        cache_dir(&presets(Role::Nli)[0].name),
        nli::nli_model_dir(),
        "nli default provisions where the loader looks"
    );
    // And the reranker spec carries the full fastembed five-file layout.
    let files = crate::cortex::spec_files(Role::Reranker, &presets(Role::Reranker)[0]);
    assert_eq!(files.len(), 5);
    assert!(files.iter().all(|(_, url)| url.starts_with("https://")));
}

// ---- historical dating: capture carries its original date ------------------

#[test]
fn parse_day_handles_days_unix_and_rfc3339_prefixes() {
    assert_eq!(parse_day("2026-07-22"), Some(1784678400));
    assert_eq!(parse_day("2024-02-29"), Some(1709164800), "leap day");
    assert_eq!(parse_day("1970-01-01"), Some(0));
    assert_eq!(parse_day("2026-07-22T15:30:00Z"), Some(1784678400));
    assert_eq!(parse_day(" 1784678400 "), Some(1784678400));
    assert_eq!(parse_day("2026-13-01"), None, "month range");
    assert_eq!(parse_day("yesterday"), None);
}

#[test]
fn writes_carry_their_original_date_on_both_backends() {
    let stores: Vec<Box<dyn Store>> = vec![
        Box::new(SqliteStore::open_in_memory().unwrap()),
        Box::new(TepinStore::open_in_memory().unwrap()),
    ];
    for s in &stores {
        let mut spec = new_node(NodeType::Decision, "a decision from the past", "");
        spec.created_at = parse_day("2025-03-10");
        let n = s.add_node(spec).unwrap();
        assert_eq!(n.created_at, parse_day("2025-03-10").unwrap());
        assert_eq!(n.valid_from, parse_day("2025-03-10"));

        // The future is clamped away — no recency boost for sale.
        let mut spec = new_node(NodeType::Insight, "a note from tomorrow", "");
        spec.created_at = Some(now() + 7 * 24 * 3600);
        let n = s.add_node(spec).unwrap();
        assert!(n.created_at <= now());

        // Omitted stays exactly as before: dated now.
        let n = s
            .add_node(new_node(NodeType::Caution, "a live capture", ""))
            .unwrap();
        assert!((now() - n.created_at) < 5);
    }
}

// ---- claim-level search (v0.6.3): rich nodes, per-claim vectors ------------

#[test]
fn claim_texts_tokenizes_substantial_sentences_only() {
    use crate::engine_claims_for_tests as claims;
    let body = "The first substantial sentence carries a real claim here. Ok. \
                A second long sentence about an entirely different matter follows; \
                and a third one closes the paragraph with more concrete detail.";
    let out = claims("My node title", Some(body));
    assert!(
        out.len() >= 2,
        "multiple substantial sentences become claims: {out:?}"
    );
    assert!(
        out.iter().all(|c| c.starts_with("My node title. ")),
        "claims keep their subject"
    );
    assert!(
        out.iter().all(|c| !c.contains("Ok.")),
        "fragments are dropped"
    );
    assert!(
        claims(
            "t",
            Some("One sentence only, however long it happens to be written out.")
        )
        .is_empty(),
        "a single claim adds nothing over the composition vector"
    );
    assert!(claims("t", None).is_empty());
}

#[test]
fn buried_claims_are_reachable_on_both_backends() {
    let emb = FakeEmbedder::default();
    let stores: Vec<Box<dyn Store>> = vec![
        Box::new(SqliteStore::open_in_memory().unwrap()),
        Box::new(TepinStore::open_in_memory().unwrap()),
    ];
    for s in &stores {
        // A rich node whose third sentence is the interesting claim, and a
        // decoy that resembles the rich node's OVERALL average text.
        let title = "storage decision";
        let body = "We considered several options for the backend layer overall. \
                    The team benchmarked writes across a few candidate engines. \
                    Zanzibar quorum replication was rejected for latency reasons entirely.";
        let rich = s
            .add_node(new_node(NodeType::Decision, title, body))
            .unwrap();
        let decoy = s
            .add_node(new_node(
                NodeType::Insight,
                "backend layer benchmarks considered",
                "",
            ))
            .unwrap();

        // Store vectors the way the engine does: composition first, claims after.
        let mut texts = vec![format!("{title}\n{body}")];
        texts.extend(crate::engine_claims_for_tests(title, Some(body)));
        assert!(texts.len() > 2, "the rich body yields claim chunks");
        s.upsert_embeddings(&rich.id, &emb.embed(&texts).unwrap())
            .unwrap();
        s.upsert_embeddings(
            &decoy.id,
            &[emb
                .embed_one("backend layer benchmarks considered")
                .unwrap()],
        )
        .unwrap();

        // Query = the buried claim's chunk text: with claim vectors this is a
        // near-exact match (distance ≈ 0); the node-level vector alone would
        // sit much further away.
        let query = emb
            .embed_one(&format!(
                "{title}. Zanzibar quorum replication was rejected for latency reasons entirely."
            ))
            .unwrap();
        let hits = s.search_vec(&query, 2).unwrap();
        assert_eq!(hits[0].0, rich.id, "the buried claim finds its node");
        assert!(
            hits[0].1 < 1e-6,
            "claim chunk matches near-exactly: {}",
            hits[0].1
        );
        assert!(
            hits.iter().all(|(id, _)| !id.contains('#')),
            "chunk keys never leak"
        );

        // embedding_of still answers with the node-level vector.
        let node_vec = s.embedding_of(&rich.id).unwrap().unwrap();
        assert_eq!(
            node_vec,
            emb.embed_one(&format!("{title}\n{body}")).unwrap()
        );
    }
}

// ---------------------------------------------------------------------------
// engine on live config (PLAN §7D workstream 3): policy numbers, brief
// composition, and the ontology itself all read the graph's stored document.
// ---------------------------------------------------------------------------

/// A fully renamed ontology: none of the shipped names survive, the role
/// flags carry every behavior.
fn custom_ontology() -> GraphConfig {
    let mut cfg = GraphConfig::default();
    cfg.ontology.preset = "custom".into();
    cfg.ontology.types = vec![
        crate::config::TypeDef {
            name: "Rule".into(),
            hue: 10,
            thought: "a law of this project".into(),
            durability: Durability::Stable,
            roles: crate::config::TypeRoles {
                worklist: false,
                anchor: false,
                rank_prior: 0.05,
                highlight: true,
                versioned: true,
            },
            brief: crate::config::BriefSection {
                show: true,
                cap: 5,
                excerpt: 100,
            },
        },
        crate::config::TypeDef {
            name: "Task".into(),
            hue: 200,
            thought: "something to do".into(),
            durability: Durability::Volatile,
            roles: crate::config::TypeRoles {
                worklist: true,
                anchor: false,
                rank_prior: 0.0,
                highlight: true,
                versioned: true,
            },
            brief: crate::config::BriefSection {
                show: false,
                cap: 5,
                excerpt: 100,
            },
        },
        crate::config::TypeDef {
            name: "Subject".into(),
            hue: 300,
            thought: "a code subject".into(),
            durability: Durability::Stable,
            roles: crate::config::TypeRoles {
                worklist: false,
                anchor: true,
                rank_prior: 0.0,
                highlight: false,
                versioned: false,
            },
            brief: crate::config::BriefSection {
                show: false,
                cap: 5,
                excerpt: 100,
            },
        },
    ];
    cfg.ontology.verbs = vec![
        crate::config::VerbDef {
            name: "supersedes".into(),
            reads_as: "Rule supersedes Rule".into(),
            roles: crate::config::VerbRoles {
                supersession: true,
                contradiction: false,
                reason: false,
                answer: false,
                dependency: false,
            },
        },
        crate::config::VerbDef {
            name: "contradicts".into(),
            reads_as: "Rule contradicts Rule".into(),
            roles: crate::config::VerbRoles {
                supersession: false,
                contradiction: true,
                reason: false,
                answer: false,
                dependency: false,
            },
        },
    ];
    cfg
}

fn custom_node(t: &str, title: &str, body: &str) -> NewNode {
    NewNode {
        node_type: NodeType::parse(t).unwrap(),
        ..new_node(NodeType::Decision, title, body)
    }
}

#[test]
fn custom_ontology_runs_every_role_on_both_backends() {
    let engines = [
        engine(),
        Engine::new(
            TepinStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ),
    ];
    for e in engines {
        e.set_graph_config(&custom_ontology()).unwrap();

        // Unknown names — including yesterday's shipped ones — are refused
        // at the write boundary; declared ones flow.
        assert!(
            e.add_node(new_node(NodeType::Decision, "old name", "x"))
                .is_err()
        );
        let a = e
            .add_node(custom_node("Rule", "never hard-delete user data", "law"))
            .unwrap();
        let b = e
            .add_node(custom_node("Rule", "hard-delete is fine actually", "law"))
            .unwrap();

        // Worklist role: an open Task IS the worklist.
        let mut task = custom_node("Task", "ship the release", "todo");
        task.status = Some(NodeStatus::Open);
        let task = e.add_node(task).unwrap();
        let open = e.store().list_open(&[]).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, task.id);

        // Anchor role: excluded from the conflict scan's iteration set.
        e.add_node(custom_node("Subject", "auth module", "code subject"))
            .unwrap();
        assert!(
            e.store()
                .scannable_nodes()
                .unwrap()
                .iter()
                .all(|n| n.node_type.as_str() != "Subject"),
            "anchor-role types never enter the scan"
        );

        // Undeclared verbs are refused; the declared supersession verb chains
        // history and the timeline follows it.
        assert!(
            e.add_edge(NewEdge {
                edge_type: EdgeType::Replaces,
                from_id: b.id.clone(),
                to_id: a.id.clone(),
                source: Source::Claude,
                note: None,
                confidence: None,
                strength: None,
                status: None,
            })
            .is_err(),
            "'replaces' is not part of this ontology"
        );
        let edge = e
            .add_edge(NewEdge {
                edge_type: EdgeType::parse("supersedes").unwrap(),
                from_id: b.id.clone(),
                to_id: a.id.clone(),
                source: Source::Claude,
                note: None,
                confidence: None,
                strength: None,
                status: None,
            })
            .unwrap();
        assert_eq!(edge.edge_type.as_str(), "supersedes");
        let timeline = e.timeline(&a.id).unwrap();
        assert_eq!(timeline.len(), 2, "timeline follows the supersession role");

        // Contradiction role: a live edge demotes the older endpoint and
        // surfaces in the conflict list; deleting it withdraws the demotion.
        let c = e
            .add_node(custom_node("Rule", "always squash commits", "law"))
            .unwrap();
        // Strictly older: same-second ties demote the from-node instead.
        e.store().backdate_node(&c.id, now() - 1_000).unwrap();
        let d = e
            .add_node(custom_node("Rule", "never squash commits", "law"))
            .unwrap();
        let conflict = e
            .add_edge(NewEdge {
                edge_type: EdgeType::parse("contradicts").unwrap(),
                from_id: d.id.clone(),
                to_id: c.id.clone(),
                source: Source::Claude,
                note: None,
                confidence: None,
                strength: None,
                status: None,
            })
            .unwrap();
        assert!(e.store().has_active_conflict(&c.id).unwrap());
        assert!(
            e.store()
                .get_node(&c.id)
                .unwrap()
                .unwrap()
                .demoted_at
                .is_some(),
            "the contradiction role carries the demotion"
        );
        e.delete_edge(&conflict.id).unwrap();
        assert!(
            e.store()
                .get_node(&c.id)
                .unwrap()
                .unwrap()
                .demoted_at
                .is_none(),
            "withdrawing the evidence withdraws the demotion"
        );
    }
}

#[test]
fn suspect_verdicts_speak_the_ontology_verbs() {
    let e = engine();
    e.set_graph_config(&custom_ontology()).unwrap();
    let a = e
        .add_node(custom_node(
            "Rule",
            "connections use a pool of 10",
            "sizing",
        ))
        .unwrap();
    let b = e
        .add_node(custom_node(
            "Rule",
            "connections use a pool of 50",
            "sizing",
        ))
        .unwrap();
    // Queue the pair directly (the scan's thresholds are covered elsewhere —
    // this test is about which verb the verdict speaks).
    let s = e.store().add_suspect(&b.id, &a.id, 0.89, None).unwrap();
    let edge = e
        .resolve_suspect(&s.id, SuspectVerdict::Replaces, Source::User)
        .unwrap()
        .unwrap();
    assert_eq!(
        edge.edge_type.as_str(),
        "supersedes",
        "the verdict creates this graph's supersession verb, not 'replaces'"
    );
}

#[test]
fn tuned_policy_windows_move_trust_and_staleness() {
    let engines = [
        engine(),
        Engine::new(
            TepinStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ),
    ];
    for e in engines {
        let n = e
            .add_node(NewNode {
                durability: Durability::Episodic,
                ..new_node(NodeType::Insight, "aging note", "wisdom")
            })
            .unwrap();
        // 40 days old: healthy under the default 183-day episodic window.
        e.store().backdate_node(&n.id, now() - 40 * 86_400).unwrap();
        let fresh = e.store().get_node(&n.id).unwrap().unwrap();
        assert!(!fresh.stale, "40d old under a 183d window is healthy");

        // Shrink the window to 30 days: the same node is now past its
        // course — floor trust, stale, and a decay candidate.
        let mut cfg = GraphConfig::default();
        cfg.policy.episodic_window_days = 30;
        e.set_graph_config(&cfg).unwrap();
        let aged = e.store().get_node(&n.id).unwrap().unwrap();
        assert!(aged.stale, "the tuned 30d window ages the node out");
        assert!(aged.trust < fresh.trust);
        assert!(
            !e.decay(cfg.policy.decay_ttl_days, true).unwrap().is_empty(),
            "the decay pass sees it under the tuned window"
        );

        // Widen instead: trust comes back without touching the node.
        cfg.policy.episodic_window_days = 3650;
        e.set_graph_config(&cfg).unwrap();
        let relaxed = e.store().get_node(&n.id).unwrap().unwrap();
        assert!(!relaxed.stale);
        assert!(relaxed.trust > aged.trust);
    }
}

#[test]
fn tuned_duplicate_bar_flips_the_write_verdict() {
    let e = engine();
    let title = "cache invalidation happens on write";
    e.add_node_checked(new_node(NodeType::Decision, title, "body"))
        .unwrap();
    // Near-identical text: a duplicate under the default 0.90 bar.
    let near = format!("{title}!");
    match e
        .add_node_checked(new_node(NodeType::Decision, &near, "body"))
        .unwrap()
    {
        WriteOutcome::Matched { .. } => {}
        WriteOutcome::Created { .. } => panic!("near-identical text must match by default"),
    }
    // Raise both bars to 1.0: only EXACT vectors match, so the same text now
    // creates (and queues no suspect — the suspect band sits at the same bar).
    let mut cfg = GraphConfig::default();
    cfg.policy.duplicate_similarity = 1.0;
    cfg.policy.conflict_suspect_similarity = 1.0;
    e.set_graph_config(&cfg).unwrap();
    match e
        .add_node_checked(new_node(NodeType::Decision, &near, "body"))
        .unwrap()
    {
        WriteOutcome::Created { .. } => {}
        WriteOutcome::Matched { .. } => panic!("a 1.0 bar must not match near-identical text"),
    }
}

#[test]
fn brief_composition_follows_the_config() {
    let e = engine();
    // The problem first, then enough principles that the recent window
    // (which claims the newest nodes) can't swallow the whole canon.
    let mut task = new_node(NodeType::Problem, "flaky test", "in ci");
    task.status = Some(NodeStatus::Open);
    e.add_node(task).unwrap();
    for i in 0..9 {
        e.add_node(new_node(
            NodeType::Principle,
            &format!("principle number {i}"),
            "always",
        ))
        .unwrap();
    }

    let stock = e.brief(16_000).unwrap();
    assert!(stock.contains("## Principles"));
    assert!(stock.contains("## Recently added"));
    assert!(stock.contains("## Open problems & intents"));
    assert!(
        !stock.contains("This graph's ontology"),
        "the teaching section is off in the shipped preset"
    );

    let mut cfg = GraphConfig::default();
    cfg.brief.recent.show = false;
    cfg.brief.ontology.show = true;
    // Hide the Principle section, shrink nothing else.
    cfg.ontology
        .types
        .iter_mut()
        .find(|t| t.name == "Principle")
        .unwrap()
        .brief
        .show = false;
    e.set_graph_config(&cfg).unwrap();

    let tuned = e.brief(16_000).unwrap();
    assert!(!tuned.contains("## Principles"));
    assert!(!tuned.contains("## Recently added"));
    assert!(
        tuned.starts_with("# Engram brief\nThis graph's ontology"),
        "the ontology teaching section leads when toggled on"
    );
    assert!(
        tuned.contains("- Decision — \"we chose this, for a reason\""),
        "teaching lines carry each type's thought"
    );
}

#[test]
fn brief_speaks_a_custom_ontology() {
    let e = engine();
    let mut cfg = custom_ontology();
    cfg.brief.recent.show = false;
    e.set_graph_config(&cfg).unwrap();
    e.add_node(custom_node("Rule", "tabs not spaces", "law"))
        .unwrap();
    let mut task = custom_node("Task", "write the docs", "todo");
    task.status = Some(NodeStatus::Open);
    e.add_node(task).unwrap();

    let brief = e.brief(16_000).unwrap();
    assert!(
        brief.contains("## Rules"),
        "canon sections use the graph's type names"
    );
    assert!(
        brief.contains("## Open tasks"),
        "the worklist heading names the worklist-role types"
    );
    assert!(!brief.contains("## Principles"));
    assert!(!brief.contains("problems & intents"));
}

#[test]
fn describe_ontology_teaches_types_verbs_and_roles() {
    let cfg = custom_ontology();
    let text = cfg.describe_ontology();
    assert!(text.contains("- Rule — \"a law of this project\""));
    assert!(text.contains("worklist: carries open/resolved status"));
    assert!(text.contains("anchor: a code subject"));
    assert!(text.contains(
        "- supersedes — e.g. Rule supersedes Rule (supersession: archives the older endpoint)"
    ));
    assert!(text.contains("- contradicts — e.g. Rule contradicts Rule (contradiction: flags a conflict, demotes trust)"));
}

#[test]
fn rename_type_bulk_retypes_and_guards_hold() {
    let engines = [
        engine(),
        Engine::new(
            TepinStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ),
    ];
    for e in engines {
        let a = e
            .add_node(new_node(NodeType::Decision, "pick sqlite", "why"))
            .unwrap();
        e.add_node(new_node(NodeType::Decision, "pick axum", "why"))
            .unwrap();

        // A plain PUT that drops a type with stored nodes is refused — the
        // rename endpoints are the migration gesture.
        let mut dropped = GraphConfig::default();
        dropped.ontology.types.retain(|t| t.name != "Decision");
        let err = e.set_graph_config(&dropped).unwrap_err().to_string();
        assert!(err.contains("Decision"), "{err}");
        assert!(err.contains("2 node"), "{err}");

        let renamed = e.rename_type("Decision", "Choice").unwrap();
        assert_eq!(renamed, 2);
        let node = e.store().get_node(&a.id).unwrap().unwrap();
        assert_eq!(node.node_type.as_str(), "Choice");
        let cfg = e.graph_config();
        assert!(cfg.type_def("Choice").is_some());
        assert!(cfg.type_def("Decision").is_none());
        // The write boundary follows the rename immediately.
        assert!(
            e.add_node(new_node(NodeType::Decision, "old name", "x"))
                .is_err()
        );
        assert!(
            e.add_node(custom_node("Choice", "new name works", "x"))
                .is_ok()
        );
        // Unknown / colliding renames are refused.
        assert!(e.rename_type("Ghost", "Whatever").is_err());
        assert!(e.rename_type("Choice", "Principle").is_err());
    }
}

#[test]
fn rename_verb_bulk_retypes_edges_and_keeps_roles() {
    let e = engine();
    let a = e
        .add_node(new_node(NodeType::Decision, "old way", "x"))
        .unwrap();
    e.store().backdate_node(&a.id, now() - 1_000).unwrap();
    let b = e
        .add_node(new_node(NodeType::Decision, "new way", "x"))
        .unwrap();
    e.add_edge(NewEdge {
        edge_type: EdgeType::Replaces,
        from_id: b.id.clone(),
        to_id: a.id.clone(),
        source: Source::User,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();

    let renamed = e.rename_verb("replaces", "supersedes").unwrap();
    assert_eq!(renamed, 1);
    let cfg = e.graph_config();
    assert_eq!(
        cfg.supersession_verb(),
        "supersedes",
        "the supersession role rides the rename"
    );
    // The timeline still walks the (renamed) chain.
    assert_eq!(e.timeline(&a.id).unwrap().len(), 2);
    // A PUT dropping a verb with stored edges is refused.
    let mut dropped = cfg.clone();
    dropped.ontology.verbs.retain(|v| v.name != "supersedes");
    dropped.ontology.verbs[0].roles.supersession = true; // keep the invariant
    let err = e.set_graph_config(&dropped).unwrap_err().to_string();
    assert!(err.contains("supersedes"), "{err}");
}

#[test]
fn shipped_presets_are_valid_and_complete() {
    let shelf = crate::config::presets();
    assert_eq!(shelf.len(), 3);
    assert_eq!(shelf[0].id, "engram");
    assert_eq!(shelf[0].config, GraphConfig::default());
    for p in &shelf {
        p.config
            .validate()
            .unwrap_or_else(|e| panic!("preset {} violates the hard invariants: {e}", p.id));
        assert!(
            p.config.ontology.types.iter().any(|t| t.roles.worklist),
            "preset {} has no worklist type",
            p.id
        );
    }
}

// ---------------------------------------------------------------------------
// 0.7.0 workstream 6: file-read match lookup, activity journal, and the
// stale-triage NLI sweep.
// ---------------------------------------------------------------------------

#[test]
fn match_code_refs_covers_files_and_parent_dirs() {
    let e = engine();
    let a = e
        .add_node(NewNode {
            code_refs: vec!["crates/engram-core/src/policy.rs".into()],
            ..new_node(
                NodeType::Caution,
                "trust math is subtle",
                "watch the windows",
            )
        })
        .unwrap();
    let b = e
        .add_node(NewNode {
            code_refs: vec!["crates/engram-core".into()],
            ..new_node(NodeType::Decision, "core stays dependency-light", "why")
        })
        .unwrap();
    e.add_node(NewNode {
        code_refs: vec!["frontend/src".into()],
        ..new_node(NodeType::Decision, "vue for the pane", "why")
    })
    .unwrap();

    let hits = e
        .match_code_refs("crates/engram-core/src/policy.rs", 5)
        .unwrap();
    let ids: Vec<&str> = hits.iter().map(|n| n.id.as_str()).collect();
    assert!(ids.contains(&a.id.as_str()), "exact file ref matches");
    assert!(
        ids.contains(&b.id.as_str()),
        "a directory ref covers files under it"
    );
    assert_eq!(ids.len(), 2, "unrelated refs stay out");

    // Normalization: leading ./ and trailing / never matter.
    assert_eq!(
        e.match_code_refs("./crates/engram-core/src/policy.rs", 5)
            .unwrap()
            .len(),
        2
    );
    // A stale node never surfaces ambiently.
    e.store()
        .backdate_node(&a.id, now() - 400 * 86_400)
        .unwrap();
    let mut cfg = GraphConfig::default();
    cfg.policy.episodic_window_days = 30;
    e.set_graph_config(&cfg).unwrap();
    let hits = e
        .match_code_refs("crates/engram-core/src/policy.rs", 5)
        .unwrap();
    assert!(
        hits.iter().all(|n| n.id != a.id || !n.stale),
        "stale nodes are filtered from ambient injection"
    );
}

#[test]
fn activity_events_land_in_the_journal() {
    let e = engine();
    e.brief(4_000).unwrap();
    e.audit_activity("mcp_session_started", Some("project test".into()))
        .unwrap();
    let page = e.audit_log(None, None, 10).unwrap();
    let actions: Vec<&str> = page.entries.iter().map(|r| r.action.as_str()).collect();
    assert!(actions.contains(&"brief_served"));
    assert!(actions.contains(&"mcp_session_started"));
    let row = page
        .entries
        .iter()
        .find(|r| r.action == "mcp_session_started")
        .unwrap();
    assert_eq!(row.entity, "session");
}

#[test]
fn stale_triage_buckets_reconfirm_contradicted_isolated() {
    let e = engine_with_nli();
    let mut cfg = GraphConfig::default();
    cfg.policy.episodic_window_days = 30;
    e.set_graph_config(&cfg).unwrap();

    // A stale note whose claim the live canon still contains → reconfirm
    // (FakeNli entails when one claim contains the other verbatim).
    let stale_ok = e
        .add_node(NewNode {
            durability: Durability::Episodic,
            ..new_node(
                NodeType::Insight,
                "the cache warms in two minutes",
                "measured on staging",
            )
        })
        .unwrap();
    e.add_node(new_node(
        NodeType::Insight,
        "the cache warms in two minutes. measured on staging and on prod",
        "measured on staging",
    ))
    .unwrap();
    // A stale note nothing current speaks to → isolated (digit/punctuation
    // text keeps the byte-histogram fake embedder far from everything).
    let stale_alone = e
        .add_node(NewNode {
            durability: Durability::Episodic,
            ..new_node(NodeType::Insight, "0000 1111 2222 :::", "#### 9999 ;;;")
        })
        .unwrap();
    for id in [&stale_ok.id, &stale_alone.id] {
        e.store().backdate_node(id, now() - 400 * 86_400).unwrap();
    }

    let triage = e.audit_stale_triage().unwrap();
    let verdict = |id: &str| {
        triage
            .iter()
            .find(|t| t.node.id == id)
            .map(|t| t.verdict.clone())
    };
    assert_eq!(verdict(&stale_ok.id).as_deref(), Some("reconfirm"));
    assert!(
        triage
            .iter()
            .find(|t| t.node.id == stale_ok.id)
            .unwrap()
            .evidence
            .is_some()
    );
    assert_eq!(verdict(&stale_alone.id).as_deref(), Some("isolated"));
    assert!(
        triage
            .iter()
            .all(|t| t.node.id != stale_ok.id || t.verdict != "contradicted"),
        "no contradiction without the similarity gate"
    );
}

#[test]
fn write_time_canon_check_supports_and_contradicts() {
    let e = engine_with_nli();
    // Canon that ENTAILS the new text (FakeNli: premise contains the
    // hypothesis verbatim — the canon claim is a superset of the new claim).
    e.add_node(new_node(
        NodeType::Principle,
        "store sessions in redis. observed again while debugging and settled since v2",
        "observed again while debugging",
    ))
    .unwrap();
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Insight,
            "store sessions in redis",
            "observed again while debugging",
        ))
        .unwrap();
    match outcome {
        WriteOutcome::Created { canon, .. } => {
            assert!(
                canon.iter().any(|c| c.verdict == "supports"),
                "entailing canon yields a supports verdict: {canon:?}"
            );
        }
        _ => panic!("must create"),
    }

    // Contradiction only inside the suspect band (co-reference gate): two
    // "contra" texts (FakeNli contradiction) that are near-identical.
    let e = engine_with_nli();
    e.add_node(new_node(
        NodeType::Decision,
        "contra: use tabs for indentation everywhere",
        "why",
    ))
    .unwrap();
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Insight,
            "contra: use tabs for indentation everywhere!!",
            "why",
        ))
        .unwrap();
    match outcome {
        WriteOutcome::Created { canon, .. } => {
            assert!(
                canon.iter().any(|c| c.verdict == "contradicts"),
                "in-band contradiction yields a contradicts verdict: {canon:?}"
            );
        }
        _ => panic!("cross-type text never dupe-matches"),
    }
}

#[test]
fn validate_graph_runs_all_passes_and_journals() {
    let e = engine();
    e.add_node(new_node(NodeType::Decision, "keep it simple", "why"))
        .unwrap();
    let note = e.validate_graph().unwrap();
    assert!(note.contains("decayed"), "{note}");
    assert!(note.contains("suspect"), "{note}");
    let page = e.audit_log(None, None, 5).unwrap();
    let row = page
        .entries
        .iter()
        .find(|r| r.action == "graph_validated")
        .expect("validation journals itself");
    assert_eq!(row.entity, "session");
}

// ---------------------------------------------------------------------------
// Version tracking + handoff notes (0.7.0).
// ---------------------------------------------------------------------------

#[test]
fn version_tracking_stamps_bound_types_only() {
    let engines = [
        engine(),
        Engine::new(
            TepinStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ),
    ];
    for e in engines {
        // Off by default: nothing is stamped even with a version set.
        e.set_current_version(Some("v0.7.0")).unwrap();
        let off = e
            .add_node(new_node(NodeType::Decision, "before tracking", "x"))
            .unwrap();
        assert_eq!(off.version, None);

        let mut cfg = GraphConfig::default();
        cfg.versioning.enabled = true;
        e.set_graph_config(&cfg).unwrap();

        let d = e
            .add_node(new_node(NodeType::Decision, "while tracking", "x"))
            .unwrap();
        assert_eq!(d.version.as_deref(), Some("v0.7.0"));
        // Principle and Anchor transcend releases in the shipped ontology.
        let p = e
            .add_node(new_node(NodeType::Principle, "timeless value", "x"))
            .unwrap();
        assert_eq!(p.version, None);
        let a = e
            .add_node(new_node(NodeType::Anchor, "auth module", "x"))
            .unwrap();
        assert_eq!(a.version, None);

        // Explicit version (historical digestion) always wins.
        let hist = e
            .add_node(NewNode {
                version: Some("v0.1.0".into()),
                ..new_node(NodeType::Insight, "old lesson", "x")
            })
            .unwrap();
        assert_eq!(hist.version.as_deref(), Some("v0.1.0"));

        // Switching and unsetting; the switch history is journaled.
        assert_eq!(
            e.set_current_version(Some("v0.8.0")).unwrap().as_deref(),
            Some("v0.7.0")
        );
        e.set_current_version(None).unwrap();
        let unversioned = e
            .add_node(new_node(NodeType::Decision, "between versions", "x"))
            .unwrap();
        assert_eq!(unversioned.version, None);
        let history = e.audit_log(None, Some("version"), 10).unwrap();
        assert!(history.entries.len() >= 3);
        assert!(
            history
                .entries
                .iter()
                .any(|r| r.title.as_deref() == Some("v0.7.0 → v0.8.0"))
        );

        // Patch can correct a node's version; the brief shows the stamp.
        e.update_node(
            &unversioned.id,
            NodePatch {
                version: Some("v0.8.1".into()),
                ..NodePatch::default()
            },
        )
        .unwrap();
        assert_eq!(
            e.store()
                .get_node(&unversioned.id)
                .unwrap()
                .unwrap()
                .version
                .as_deref(),
            Some("v0.8.1")
        );
        let brief = e.brief(16_000).unwrap();
        assert!(brief.contains("Current working version: not set"));
        assert!(brief.contains("[Decision"), "{brief}");
        assert!(
            brief.contains("v0.8.1"),
            "node lines carry the stamp: {brief}"
        );
    }
}

#[test]
fn handoff_notes_lead_the_brief() {
    let e = engine();
    for i in 0..9 {
        e.add_node(new_node(
            NodeType::Principle,
            &format!("filler principle {i}"),
            "x",
        ))
        .unwrap();
    }
    let mut n = new_node(
        NodeType::Intent,
        "finish the cutover before touching search",
        "half-migrated state",
    );
    n.status = Some(NodeStatus::Open);
    n.tags = vec!["handoff".into()];
    n.durability = Durability::Volatile;
    let handoff = e.add_node(n).unwrap();

    let brief = e.brief(16_000).unwrap();
    let handoff_pos = brief.find("## Handoff — left for this session").unwrap();
    let first_section = brief.find("##").unwrap();
    assert_eq!(handoff_pos, first_section, "handoff is the FIRST section");
    assert!(brief.contains("finish the cutover before touching search"));

    // Resolved handoffs stop surfacing.
    e.update_node(
        &handoff.id,
        NodePatch {
            status: Some(NodeStatus::Resolved),
            ..NodePatch::default()
        },
    )
    .unwrap();
    let brief = e.brief(16_000).unwrap();
    assert!(!brief.contains("## Handoff"));
}

#[test]
fn contradiction_hints_carry_a_direction() {
    let e = engine_with_nli();
    // Older node carries the negation marker; texts are near-identical so
    // the write-time scan queues the pair (same band as the dupe test but
    // cross-type, so no dupe match).
    let older = e
        .add_node(new_node(
            NodeType::Decision,
            "contra neg: never use tabs for indentation",
            "law",
        ))
        .unwrap();
    e.store().backdate_node(&older.id, now() - 1_000).unwrap();
    e.add_node_checked(new_node(
        NodeType::Insight,
        "contra: never use tabs for indentation!!",
        "law",
    ))
    .unwrap();

    let suspects = e.suspects().unwrap();
    assert_eq!(suspects.len(), 1, "the pair queues one suspect");
    let s = &suspects[0];
    assert_eq!(s.nli_label.as_deref(), Some("contradiction"));
    assert_eq!(
        s.nli_direction.as_deref(),
        Some("older"),
        "the neg-marked older side reads as the negation carrier"
    );
    let brief = e.brief(16_000).unwrap();
    assert!(
        brief.contains("negation likely on the older side"),
        "{brief}"
    );
}

#[test]
fn answered_sweep_penalizes_already_linked_pairs() {
    let e = engine_with_nli();
    let mut problem = new_node(
        NodeType::Problem,
        "the importer times out on big graphs",
        "seen twice",
    );
    problem.status = Some(NodeStatus::Open);
    let problem = e.add_node(problem).unwrap();
    // Candidate whose claim contains the problem's claim (FakeNli entails).
    let fix = e
        .add_node(new_node(
            NodeType::Resolution,
            "the importer times out on big graphs. seen twice — fixed by streaming batches",
            "seen twice",
        ))
        .unwrap();

    let fresh = e.audit_answered().unwrap();
    let hit = fresh
        .iter()
        .find(|h| h.problem.id == problem.id && h.candidate.id == fix.id)
        .expect("unlinked pair is nominated");
    assert!(hit.existing_link.is_none());

    // Linked another way: the nomination survives but names the verb.
    e.add_edge(NewEdge {
        edge_type: EdgeType::BuildsOn,
        from_id: fix.id.clone(),
        to_id: problem.id.clone(),
        source: Source::Claude,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();
    let linked = e.audit_answered().unwrap();
    let hit = linked
        .iter()
        .find(|h| h.problem.id == problem.id)
        .expect("differently-linked pair still nominated");
    assert_eq!(hit.existing_link.as_deref(), Some("builds-on"));

    // Linked with the answer verb: nothing left to suggest.
    e.add_edge(NewEdge {
        edge_type: EdgeType::Answers,
        from_id: fix.id.clone(),
        to_id: problem.id.clone(),
        source: Source::Claude,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();
    let done = e.audit_answered().unwrap();
    assert!(
        done.iter().all(|h| h.problem.id != problem.id),
        "answer-linked pairs are dropped"
    );
}
