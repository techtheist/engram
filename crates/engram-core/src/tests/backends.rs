//! Store conformance across backends, config roundtrips, migration.

use super::*;

pub(super) fn store_battery(s: &dyn Store, backend: &str) {
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
            source: Source::Agent,
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
            source: Source::Agent,
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
            source: Source::Agent,
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
    let hybrid = s
        .search_hybrid(
            "journaling",
            Some(&va),
            &[],
            5,
            Default::default(),
            &Default::default(),
        )
        .unwrap();
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
    assert!(pending[0].via.is_none());
    // A bridged pair names its bridge, stored and joined.
    s.set_suspect_via(&sus.id, &a.id).unwrap();
    assert_eq!(
        s.get_suspect(&sus.id).unwrap().unwrap().via.as_deref(),
        Some(a.id.as_str()),
        "{backend}"
    );
    let pending = s.suspects_pending().unwrap();
    assert_eq!(
        pending[0].via.as_ref().map(|v| v.id.as_str()),
        Some(a.id.as_str()),
        "{backend}"
    );
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
            session_id: Some(if i == 0 { "sess-a" } else { "sess-b" }.into()),
            cwd: None,
            pid: None,
            version: None,
            op_id: (i > 0).then(|| "op-1".to_string()),
            undone_by: None,
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
    // Undo's lookups: by session, by operation, by seq, and later
    // rows on one entity — oldest first, the op id round-tripped.
    let q = |q: AuditQuery| s.audit_rows(&q).unwrap();
    let by_session = q(AuditQuery {
        session_id: Some("sess-b".into()),
        ..AuditQuery::default()
    });
    assert_eq!(by_session.len(), 2, "{backend}");
    assert!(
        by_session[0].seq < by_session[1].seq,
        "{backend}: oldest first"
    );
    let by_op = q(AuditQuery {
        op_id: Some("op-1".into()),
        ..AuditQuery::default()
    });
    assert_eq!(by_op.len(), 2, "{backend}");
    assert_eq!(by_op[0].op_id.as_deref(), Some("op-1"), "{backend}");
    let first = by_session[0].seq;
    assert_eq!(
        q(AuditQuery {
            seq: Some(first),
            ..AuditQuery::default()
        })
        .len(),
        1,
        "{backend}"
    );
    let later_on_a = q(AuditQuery {
        entity_id: Some(a.id.clone()),
        after: Some(page.entries[1].seq - 1),
        ..AuditQuery::default()
    });
    assert!(later_on_a.iter().all(|r| r.entity_id == a.id), "{backend}");

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
