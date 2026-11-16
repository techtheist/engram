//! The 0.9 machinery against the older surfaces and shapes.

use super::*;

#[test]
fn sealed_store_write_verdicts_and_suspects_still_fire() {
    let e = sealed_capable_engine();
    e.set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();

    // The verdict machinery reads back through the sealed store: an
    // identical re-write must MATCH, not create a twin.
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
        WriteOutcome::Matched { node, .. } => assert_eq!(node.id, first.id),
        WriteOutcome::Created { .. } => panic!("sealed store must still detect duplicates"),
    }

    // Cross-type lookalike queues a suspect, and the verdict resolves it —
    // the whole conflict loop over sealed rows.
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Caution,
            "Adopt SQLite WAL mode",
            "concurrent reads",
        ))
        .unwrap();
    assert!(matches!(outcome, WriteOutcome::Created { .. }));
    let suspects = e.suspects().unwrap();
    assert_eq!(suspects.len(), 1, "sealed store still queues lookalikes");
    assert!(
        !suspects[0].a.title.contains("enc1:"),
        "suspect pairs read as prose"
    );
    let edge = e
        .resolve_suspect(&suspects[0].id, SuspectVerdict::Conflict, Source::User)
        .unwrap()
        .expect("edge created");
    assert_eq!(edge.edge_type, EdgeType::ConflictsWith);
    assert!(e.suspects().unwrap().is_empty());
}

#[test]
fn sealed_store_brief_merge_timeline_and_tombstone_read_prose() {
    let e = sealed_capable_engine();
    let nodes = seed_corpus(&e);
    e.set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();

    // Brief digests sealed rows as prose.
    let brief = e.brief(6000).unwrap();
    assert!(brief.contains("Adopt sqlite for the ledger"), "{brief}");
    assert!(!brief.contains("enc1:"), "no ciphertext leaks: {brief}");

    // A replaces chain written AFTER sealing walks as a timeline.
    let v2 = e
        .add_node(new_node(
            NodeType::Decision,
            "The ledger flushes every minute",
            "hourly was too coarse",
        ))
        .unwrap();
    e.add_edge(NewEdge {
        edge_type: EdgeType::Replaces,
        from_id: v2.id.clone(),
        to_id: nodes[1].id.clone(),
        source: Source::Agent,
        note: Some("cadence tightened".into()),
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();
    let chain = e.timeline(&v2.id).unwrap();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].title, "The ledger flushes hourly");
    assert_eq!(chain[0].replaced_note.as_deref(), Some("cadence tightened"));

    // Merge over sealed rows: union carries, victim archives, prose reads.
    let survivor = e
        .merge_nodes(
            &nodes[3].id,
            &[nodes[4].id.clone()],
            None,
            None,
            Source::Agent,
        )
        .unwrap();
    assert_eq!(survivor.survivor.title, "Retry with jitter");
    let victim = e.get_node(&nodes[4].id).unwrap().unwrap();
    assert!(victim.valid_until.is_some(), "victim archived");
    assert_eq!(victim.title, "The exporter owns backoff");

    // Hard delete with tombstone mints a readable marker.
    let (removed, ts) = e
        .delete_node_with_tombstone(&nodes[2].id, Some("wrong call"), true)
        .unwrap();
    assert!(removed);
    let ts = ts.expect("default ontology tombstones");
    assert!(ts.title.contains("Postgres was rejected"), "{}", ts.title);
    assert!(ts.body.as_deref().unwrap().contains("wrong call"));
}

#[test]
fn redaction_still_scrubs_before_sealing() {
    let e = sealed_capable_engine();
    e.set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();
    let n = e
        .add_node(new_node(
            NodeType::Caution,
            "leak AKIAIOSFODNN7EXAMPLE",
            "token=abc123secretvalue99",
        ))
        .unwrap();
    // Sealing must not become a reason to skip redaction: the secret is
    // gone from the decrypted read AND from a plaintext export.
    let got = e.get_node(&n.id).unwrap().unwrap();
    assert!(!got.title.contains("AKIA"), "{}", got.title);
    assert!(got.title.contains("[REDACTED]"));
    assert!(!got.body.as_deref().unwrap().contains("abc123secretvalue99"));
    let export = serde_json::to_string(&e.export().unwrap()).unwrap();
    assert!(!export.contains("AKIA"));
    assert!(!export.contains("abc123secretvalue99"));
}

#[test]
fn sealed_indexed_fields_and_event_clock_still_work() {
    use crate::config::FieldKind;
    let e = sealed_capable_engine();
    let cfg = GraphConfig {
        fields: vec![
            crate::config::FieldDef {
                indexed: true,
                ..fdef("codename", FieldKind::Text)
            },
            fdef("event_date", FieldKind::Date),
        ],
        ..GraphConfig::default()
    };
    e.set_graph_config(&cfg).unwrap();

    // One row sealed by the migration, one written under sealed state —
    // both must stay reachable through the blind keyword index.
    let mut n = new_node(NodeType::Decision, "pre-seal note", "body one");
    n.fields = Some(jmap(&[
        ("codename", serde_json::json!("zanzibar")),
        ("event_date", serde_json::json!("2019-06-01")),
    ]));
    let pre = e.add_node(n).unwrap();
    e.set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();
    let mut n = new_node(NodeType::Decision, "post-seal note", "body two");
    n.fields = Some(jmap(&[
        ("codename", serde_json::json!("quagmire")),
        ("event_date", serde_json::json!("2023-02-10")),
    ]));
    let post = e.add_node(n).unwrap();

    for (term, id) in [("zanzibar", &pre.id), ("quagmire", &post.id)] {
        let hits = e.store().search_fts(term, &[], 5).unwrap();
        assert!(
            hits.iter().any(|h| &h.id == id),
            "indexed field {term:?} reaches keyword search on the sealed store"
        );
    }

    // The bitemporal clock reads field values through the sealed rows.
    let filter = e
        .time_filter_clocked(
            Some("2022-01-01"),
            Some("2024-01-01"),
            None,
            None,
            Some("event_date"),
        )
        .unwrap();
    let hits = e.search_filtered("note body", &[], 10, &filter).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&post.id.as_str()), "in-window event: {ids:?}");
    assert!(!ids.contains(&pre.id.as_str()), "out-of-window drops");

    // And the stored values read back as values, not blobs.
    let f = e.get_node(&post.id).unwrap().unwrap().fields.unwrap();
    assert_eq!(f["codename"], "quagmire");
}

#[test]
fn sqlite_store_refuses_encryption_with_a_teaching_error() {
    let e = engine();
    let err = e
        .set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap_err()
        .to_string();
    assert!(err.contains("TepinDB"), "names the fix: {err}");
    assert_eq!(
        e.encryption_states(),
        (crate::seal::EncryptionState::Plaintext, None),
        "a refused enable leaves the recorded state untouched"
    );
    // Disabling on an already-plaintext store is a no-op, not an error —
    // reconcile toward the state you're already in must always succeed.
    assert_eq!(
        e.set_store_encryption("graph", false, &mut |_, _| {})
            .unwrap(),
        0
    );
    let err = e
        .set_store_encryption("vault", true, &mut |_, _| {})
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("graph | history"),
        "teaches the targets: {err}"
    );
}

#[test]
fn graph_sealing_disk_journey_reconcile_and_keyless_reads() {
    let mut env = crate::sync::EnvGuard::lock();
    let tmp = std::env::temp_dir().join(format!("engram-graphseal-{}", id::new_id()));
    let _ = std::fs::remove_dir_all(&tmp);
    env.set("ENGRAM_HOME", tmp.join("enghome"));
    env.set("ENGRAM_KEYRING", "off");
    let dir = tmp.join("proj/.engram");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("graph.tepin");

    // Era one: desired encryption reconciles the store at open.
    let node_id;
    {
        let mut e = Engine::with_store(
            crate::store::open_store(&db).unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        node_id = e
            .add_node(new_node(
                NodeType::Decision,
                "the marker decision",
                "MARKER-BODY-7391",
            ))
            .unwrap()
            .id;
        e.set_desired_encryption(Some(true), None);
        assert!(
            e.ensure_encryption().unwrap() >= 1,
            "open-time reconcile seals"
        );
        assert_eq!(
            e.store().encryption_state(),
            crate::seal::EncryptionState::Sealed
        );
    }

    // Era two: a daemon with NO desired setting must follow the store's own
    // recorded state — never migrate, never desync.
    {
        let e = Engine::with_store(
            crate::store::open_store(&db).unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        assert_eq!(e.ensure_encryption().unwrap(), 0, "no desire, no migration");
        assert_eq!(
            e.store().encryption_state(),
            crate::seal::EncryptionState::Sealed
        );
        let n = e.get_node(&node_id).unwrap().unwrap();
        assert_eq!(n.title, "the marker decision", "key from the home file");
    }

    // Keyless reopen: placeholders, never plaintext, never an error.
    env.set("ENGRAM_HOME", tmp.join("keyless"));
    {
        let s = crate::store::open_store(&db).unwrap();
        let n = s.get_node(&node_id).unwrap().unwrap();
        assert!(
            n.title.contains("unavailable"),
            "no key → placeholder: {}",
            n.title
        );
        assert!(
            !format!("{:?}", n.body).contains("MARKER-BODY-7391"),
            "sealed body never leaks keyless"
        );
        // Keyword search over a sealed store without the key degrades to
        // silence, not to an error (an error would read as graph-is-silent
        // anyway — but it must not poison the whole search).
        assert!(s.search_fts("marker decision", &[], 5).is_ok());
    }

    // Era three: key back, desired OFF unseals the whole store.
    env.set("ENGRAM_HOME", tmp.join("enghome"));
    {
        let mut e = Engine::with_store(
            crate::store::open_store(&db).unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        e.set_desired_encryption(Some(false), None);
        assert!(e.ensure_encryption().unwrap() >= 1);
        assert_eq!(
            e.store().encryption_state(),
            crate::seal::EncryptionState::Plaintext
        );
        let n = e.get_node(&node_id).unwrap().unwrap();
        assert_eq!(n.body.as_deref(), Some("MARKER-BODY-7391"));
    }

    env.remove("ENGRAM_KEYRING");
    env.remove("ENGRAM_HOME");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn import_into_a_sealed_store_seals_and_stays_searchable() {
    let plain = sealed_capable_engine();
    seed_corpus(&plain);
    let export = plain.export().unwrap();

    let sealed = sealed_capable_engine();
    sealed
        .set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();
    sealed.import(export).unwrap();
    assert_eq!(
        sealed.store().encryption_state(),
        crate::seal::EncryptionState::Sealed,
        "import never flips the recorded state"
    );
    // Imported rows went through the sealed write path: keyword search
    // still finds them (blind index), and reads are prose.
    let hits = sealed.store().search_fts("jitter retries", &[], 5).unwrap();
    assert!(!hits.is_empty(), "imported rows reach the blind index");
    assert!(hits.iter().all(|h| !h.title.contains("enc1:")));
}

#[test]
fn required_field_can_never_veto_a_hard_delete() {
    use crate::config::FieldKind;
    let e = engine();
    // A required field on EVERY type — tombstones included.
    let cfg = GraphConfig {
        fields: vec![crate::config::FieldDef {
            required: true,
            ..fdef("origin", FieldKind::Text)
        }],
        ..GraphConfig::default()
    };
    e.set_graph_config(&cfg).unwrap();
    let mut n = new_node(NodeType::Decision, "short-lived decision", "x");
    n.fields = Some(jmap(&[("origin", serde_json::json!("field report"))]));
    let victim = e.add_node(n).unwrap();

    // The delete's tombstone mint is engine-authored: required enforcement
    // is a contract for authors and must not block the user's delete.
    let (removed, ts) = e
        .delete_node_with_tombstone(&victim.id, Some("cleanup"), true)
        .unwrap();
    assert!(removed);
    let ts = ts.expect("tombstone minted despite the required field");
    assert!(ts.fields.is_none(), "the mint carries no invented values");

    // But an AUTHORED tombstone still owes the field — the exemption is the
    // mint path, not the type.
    let err = e
        .add_node(new_node(NodeType::Tombstone, "hand-written marker", "x"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing required"), "{err}");
}

#[test]
fn retype_revalidates_fields_against_the_target_type() {
    use crate::config::FieldKind;
    let e = engine();
    let mut cfg = fields_config();
    cfg.fields.push(crate::config::FieldDef {
        applies_to: vec!["Insight".into()],
        ..fdef("hunch_strength", FieldKind::Number)
    });
    e.set_graph_config(&cfg).unwrap();

    let mut n = new_node(NodeType::Insight, "cache is the bottleneck", "x");
    n.fields = Some(jmap(&[("hunch_strength", serde_json::json!(0.8))]));
    let node = e.add_node(n).unwrap();

    // Retype to a type where the stored field doesn't apply: refused, and
    // the error teaches the target type's roster.
    let err = e
        .update_node(
            &node.id,
            NodePatch {
                node_type: Some(NodeType::Caution),
                ..NodePatch::default()
            },
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown custom field"), "{err}");
    assert!(err.contains("hunch_strength"), "{err}");

    // Retype to a type with an unmet REQUIRED field: refused too.
    let plain = e
        .add_node(new_node(NodeType::Insight, "plain note", "x"))
        .unwrap();
    let err = e
        .update_node(
            &plain.id,
            NodePatch {
                node_type: Some(NodeType::Decision),
                ..NodePatch::default()
            },
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing required"), "{err}");

    // The same retype WITH the fix in one patch goes through: null the
    // inapplicable field, supply the required one.
    let node = e
        .update_node(
            &node.id,
            NodePatch {
                node_type: Some(NodeType::Decision),
                fields: Some(jmap(&[
                    ("hunch_strength", serde_json::Value::Null),
                    ("priority", serde_json::json!("high")),
                ])),
                ..NodePatch::default()
            },
        )
        .unwrap();
    assert_eq!(node.node_type, NodeType::Decision);
    let f = node.fields.as_ref().unwrap();
    assert!(!f.contains_key("hunch_strength"));
    assert_eq!(f["priority"], "high");
}

#[test]
fn merge_keeps_the_survivor_fields_and_never_leaks_the_victims() {
    let e = engine();
    e.set_graph_config(&fields_config()).unwrap();
    let mut a = new_node(NodeType::Decision, "queue is rabbitmq", "x");
    a.fields = Some(jmap(&[
        ("priority", serde_json::json!("high")),
        ("owner", serde_json::json!("core team")),
    ]));
    let a = e.add_node(a).unwrap();
    let mut b = new_node(NodeType::Decision, "rabbit mq is the queue", "y");
    b.fields = Some(jmap(&[
        ("priority", serde_json::json!("low")),
        ("owner", serde_json::json!("someone else")),
    ]));
    let b = e.add_node(b).unwrap();

    let out = e
        .merge_nodes(&a.id, std::slice::from_ref(&b.id), None, None, Source::User)
        .unwrap();
    let f = out.survivor.fields.as_ref().expect("survivor keeps fields");
    assert_eq!(f["priority"], "high", "survivor's values win");
    assert_eq!(f["owner"], "core team");
    // The victim's fields stay on the archived victim — history, not loss.
    let victim = e.get_node(&b.id).unwrap().unwrap();
    assert_eq!(victim.fields.as_ref().unwrap()["priority"], "low");
}

#[test]
fn pre_0_9_export_and_config_shapes_still_parse() {
    // (a) A 0.8.x export: node objects with NO `fields` member at all.
    let old = engine();
    old.add_node(new_node(NodeType::Decision, "from the old era", "x"))
        .unwrap();
    let mut v = serde_json::to_value(old.export().unwrap()).unwrap();
    for n in v["nodes"].as_array_mut().unwrap() {
        let o = n.as_object_mut().unwrap();
        o.remove("fields");
        assert!(!o.contains_key("fields"));
    }
    let parsed: ExportGraph = serde_json::from_value(v).expect("pre-fields export parses");
    let e = engine();
    e.import(parsed).unwrap();
    let hits = e.store().search_fts("old era", &[], 5).unwrap();
    assert_eq!(hits.len(), 1, "imported node is whole");

    // (b) A pre-0.9 stored GraphConfig: 8-type ontology, no `fields` array,
    // no `tombstone` role key anywhere. Must parse, validate, and leave
    // delete-with-tombstone degrading to a plain delete.
    let mut v = serde_json::to_value(GraphConfig::default()).unwrap();
    let o = v.as_object_mut().unwrap();
    o.remove("fields");
    let types = v["ontology"]["types"].as_array_mut().unwrap();
    types.retain(|t| t["name"] != "Tombstone");
    assert_eq!(types.len(), 8, "the pre-0.9 roster");
    for t in types.iter_mut() {
        if let Some(roles) = t.get_mut("roles").and_then(|r| r.as_object_mut()) {
            roles.remove("tombstone");
        }
    }
    let old_cfg: GraphConfig = serde_json::from_value(v).expect("pre-0.9 config parses");
    old_cfg.validate().expect("and validates");
    assert!(old_cfg.tombstone_type().is_none());
    let e = engine();
    let victim = e
        .add_node(new_node(NodeType::Decision, "doomed", "x"))
        .unwrap();
    e.set_graph_config(&old_cfg).unwrap();
    let (removed, ts) = e
        .delete_node_with_tombstone(&victim.id, None, true)
        .unwrap();
    assert!(removed);
    assert!(ts.is_none(), "no tombstone role → plain delete, no error");
}
