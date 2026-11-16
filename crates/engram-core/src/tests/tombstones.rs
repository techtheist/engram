//! The tombstone role and the write-path guards it drives.

use super::*;

#[test]
fn tombstone_role_refuses_worklist_and_anchor() {
    let mut cfg = GraphConfig::default();
    let idx = cfg
        .ontology
        .types
        .iter()
        .position(|t| t.roles.tombstone)
        .unwrap();
    cfg.ontology.types[idx].roles.worklist = true;
    assert!(cfg.validate().is_err(), "tombstone+worklist must fail");
    cfg.ontology.types[idx].roles.worklist = false;
    cfg.ontology.types[idx].roles.anchor = true;
    assert!(cfg.validate().is_err(), "tombstone+anchor must fail");
    cfg.ontology.types[idx].roles.anchor = false;
    cfg.validate().unwrap();
}

#[test]
fn delete_with_tombstone_records_the_victim_and_reason() {
    let e = engine();
    let victim = e
        .add_node(new_node(
            NodeType::Decision,
            "Use the flaky vendor SDK",
            "It seemed fine at the time.",
        ))
        .unwrap();
    let (removed, tombstone) = e
        .delete_node_with_tombstone(&victim.id, Some("vendor sunset the SDK"), true)
        .unwrap();
    assert!(removed);
    let t = tombstone.expect("default ontology has a tombstone type");
    assert_eq!(t.node_type, NodeType::Tombstone);
    assert!(t.title.contains("Use the flaky vendor SDK"));
    let body = t.body.as_deref().unwrap();
    assert!(body.contains("Decision"), "victim type recorded: {body}");
    assert!(body.contains(&victim.id), "victim id recorded: {body}");
    assert!(
        body.contains("vendor sunset the SDK"),
        "reason recorded: {body}"
    );
    assert_eq!(t.source, Source::User, "delete is a user gesture");
    assert!(e.get_node(&victim.id).unwrap().is_none(), "victim gone");
    assert!(e.get_node(&t.id).unwrap().is_some(), "tombstone persists");
}

/// Search hits carry the tombstone ROLE as a flag, so a reader on
/// any surface — REST has no `tombstone_note` — can tell "Removed: X" from
/// a memory of X without knowing which type name plays the role. Found by
/// the ForgetEval adapter (2026-09-12): a role-blind reader took the
/// marker for the fact, and a type-name filter breaks on custom ontologies.
#[test]
fn search_hits_flag_the_tombstone_role_and_nothing_else() {
    let e = engine();
    let victim = e
        .add_node(new_node(
            NodeType::Decision,
            "Use the flaky vendor SDK",
            "It seemed fine at the time.",
        ))
        .unwrap();
    let survivor = e
        .add_node(new_node(
            NodeType::Caution,
            "The vendor SDK retries forever on 5xx",
            "Wrap every call in a deadline.",
        ))
        .unwrap();
    let (_, tombstone) = e
        .delete_node_with_tombstone(&victim.id, Some("vendor sunset the SDK"), true)
        .unwrap();
    let t = tombstone.unwrap();
    let hits = e.search("vendor SDK", &[], 8).unwrap();
    let flagged: Vec<&str> = hits
        .iter()
        .filter(|h| h.tombstone)
        .map(|h| h.id.as_str())
        .collect();
    assert_eq!(flagged, vec![t.id.as_str()], "only the marker is flagged");
    assert!(
        hits.iter().any(|h| h.id == survivor.id && !h.tombstone),
        "live canon is not flagged: {hits:?}"
    );
    // Serialized only when true: a plain hit's wire shape is unchanged.
    let json = serde_json::to_value(&hits).unwrap();
    for h in json.as_array().unwrap() {
        let is_marker = h["id"] == t.id.as_str();
        assert_eq!(h.get("tombstone").is_some(), is_marker, "{h}");
    }
}

#[test]
fn delete_with_tombstone_degrades_to_plain_delete_without_the_role() {
    let e = engine();
    // The minimal preset declares no tombstone type.
    let minimal = crate::config::presets()
        .into_iter()
        .find(|p| p.id == "minimal")
        .unwrap()
        .config;
    let victim = e
        .add_node(new_node(NodeType::Decision, "short-lived", "x"))
        .unwrap();
    // Reshape AFTER the write: Decision no longer exists, but the delete path
    // must not care about the victim's type — only about the tombstone role.
    let mut cfg = minimal;
    cfg.ontology.types.push(crate::config::tdef(
        "Decision",
        217,
        "legacy",
        Durability::Stable,
        crate::config::TypeRoles::plain(0.0),
        crate::config::hidden_brief(),
    ));
    e.set_graph_config(&cfg).unwrap();
    let (removed, tombstone) = e
        .delete_node_with_tombstone(&victim.id, None, true)
        .unwrap();
    assert!(removed);
    assert!(tombstone.is_none(), "no tombstone type — plain delete");
    assert!(e.get_node(&victim.id).unwrap().is_none());
}

#[test]
fn tombstones_sit_out_the_conflict_scan() {
    let e = engine();
    e.add_node(new_node(
        NodeType::Insight,
        "The cache uses LRU eviction",
        "Measured on the live daemon.",
    ))
    .unwrap();
    let t = e
        .add_node(new_node(
            NodeType::Tombstone,
            "Removed: the cache uses LRU eviction",
            "Deleted Insight — the cache was ripped out entirely.",
        ))
        .unwrap();
    let scannable = e.store().scannable_nodes().unwrap();
    assert!(
        scannable.iter().all(|n| n.id != t.id),
        "tombstone must not be conflict-scan material"
    );
    assert!(
        scannable.iter().any(|n| n.node_type == NodeType::Insight),
        "ordinary nodes still scan"
    );
}

// ---------------------------------------------------------------------------
// 0.9.2: tombstones guard the write path — like superseded, but "don't
// re-add" instead of "follow the successor".
// ---------------------------------------------------------------------------

#[test]
fn writes_near_a_tombstone_warn_tombstoned_across_types() {
    let e = engine();
    let t = e
        .add_node(new_node(
            NodeType::Tombstone,
            "Removed: cache results in redis",
            "Deleted Decision. **Why:** redis was ripped out of the stack.",
        ))
        .unwrap();

    // A different type re-deriving the dead knowledge: the same-type
    // duplicate match can't see it, the tombstoned warning must.
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Decision,
            "cache results in redis",
            "for speed",
        ))
        .unwrap();
    let WriteOutcome::Created { warnings, .. } = outcome else {
        panic!("a tombstone never blocks a write — it warns")
    };
    let w = warnings
        .iter()
        .find(|w| w.id == t.id)
        .unwrap_or_else(|| panic!("writing near a tombstone must warn: {warnings:?}"));
    assert_eq!(w.reason, "tombstoned");
    assert!(
        w.note
            .as_deref()
            .is_some_and(|n| n.contains("redis was ripped out")),
        "the warning carries the tombstone's reason: {w:?}"
    );
    // The tombstone is active, so it is never mistaken for superseded.
    assert!(
        warnings
            .iter()
            .all(|w| w.id != t.id || w.reason != "superseded"),
        "{warnings:?}"
    );
}

#[test]
fn re_adding_a_buried_title_word_for_word_warns_whatever_the_vectors_say() {
    // The purge-shaped marker ("Removed: X" + a why line) embeds far from a
    // full-bodied resurrection of X; the title channel does not care.
    let e = engine();
    let victim = e
        .add_node(new_node(
            NodeType::Caution,
            "cobalt grove sweep worker expires its own grants when the shard rebalances",
            "Found the hard way. The window is narrow enough that a healthy run never shows it, \
             which is exactly why it survived review. Two engineers examined it separately.",
        ))
        .unwrap();
    let (_, tomb) = e
        .delete_node_with_tombstone(
            &victim.id,
            Some("no longer applies after the sweep worker rework"),
            false,
        )
        .unwrap();
    let tomb = tomb.expect("the default ontology mints a tombstone");
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Caution,
            "Cobalt grove sweep worker expires its own grants, when the shard rebalances!",
            "Recorded again from the runbook, at length, with a body that shares nothing with \
             the marker's why line and everything with the component's other notes.",
        ))
        .unwrap();
    let WriteOutcome::Created { warnings, .. } = outcome else {
        panic!("a tombstone never blocks a write — it warns")
    };
    let w = warnings
        .iter()
        .find(|w| w.id == tomb.id)
        .unwrap_or_else(|| panic!("re-adding the buried title must warn: {warnings:?}"));
    assert_eq!(w.reason, "tombstoned");
    assert!(
        w.note
            .as_deref()
            .is_some_and(|n| n.contains("no longer applies")),
        "{w:?}"
    );
    assert_eq!(
        warnings.iter().filter(|w| w.reason == "tombstoned").count(),
        1,
        "one channel per marker: {warnings:?}"
    );
}

#[test]
fn tombstones_queue_no_write_time_suspects_on_either_side() {
    let e = engine();
    let victim = e
        .add_node(new_node(
            NodeType::Decision,
            "sessions live in redis",
            "fast and shared",
        ))
        .unwrap();
    // Writing the tombstone itself (the bury gesture) must not pair it with
    // its victim — that suspect would invite archiving the marker.
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Tombstone,
            "Removed: sessions live in redis",
            "Deleted Decision. **Why:** redis was dropped.",
        ))
        .unwrap();
    let WriteOutcome::Created {
        node: t, suspects, ..
    } = outcome
    else {
        panic!("creates")
    };
    assert!(
        suspects.is_empty(),
        "tombstone write queues nothing: {suspects:?}"
    );
    // Nor does a later note landing near the tombstone — the warning is the
    // channel, and the victim (still live here) remains fair game.
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Insight,
            "sessions live in redis",
            "fast and shared",
        ))
        .unwrap();
    let WriteOutcome::Created {
        suspects, warnings, ..
    } = outcome
    else {
        panic!("creates")
    };
    assert!(
        suspects.iter().all(|s| s.a.id != t.id && s.b.id != t.id),
        "no suspect involves the tombstone: {suspects:?}"
    );
    assert!(
        suspects
            .iter()
            .any(|s| s.a.id == victim.id || s.b.id == victim.id),
        "the live victim still pairs: {suspects:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.id == t.id && w.reason == "tombstoned"),
        "{warnings:?}"
    );
}

#[test]
fn canon_check_skips_tombstones_the_warning_already_covers() {
    let e = engine_with_nli();
    // FakeNli reads shared "contra" as contradiction — the one pair shape
    // that would have produced a spurious canon verdict from a tombstone.
    let t = e
        .add_node(new_node(
            NodeType::Tombstone,
            "Removed: contra sessions in redis",
            "Deleted Decision.",
        ))
        .unwrap();
    let outcome = e
        .add_node_checked(new_node(NodeType::Decision, "contra sessions in redis", ""))
        .unwrap();
    let WriteOutcome::Created {
        warnings, canon, ..
    } = outcome
    else {
        panic!("different type must create")
    };
    assert!(
        canon.iter().all(|c| c.id != t.id),
        "tombstones are not canon-check material: {canon:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.id == t.id && w.reason == "tombstoned"),
        "the tombstoned warning is the channel: {warnings:?}"
    );
}

#[test]
fn delete_with_tombstone_carries_the_removed_text_and_rehomes_anchor_edges() {
    let e = engine();
    let anchor = e
        .add_node(new_node(NodeType::Anchor, "sdk layer", ""))
        .unwrap();
    let principle = e
        .add_node(new_node(NodeType::Principle, "prefer boring tools", ""))
        .unwrap();
    let mut nn = new_node(
        NodeType::Decision,
        "Use the flaky vendor SDK",
        "The vendor SDK handles retries and auth for us.",
    );
    nn.tags = vec!["sdk".into()];
    nn.code_refs = vec!["src/sdk.rs".into()];
    let victim = e.add_node(nn).unwrap();
    let about = link(e.store(), EdgeType::About, &victim.id, &anchor.id);
    link(e.store(), EdgeType::Because, &victim.id, &principle.id);

    let (removed, tombstone) = e
        .delete_node_with_tombstone(&victim.id, Some("vendor sunset the SDK"), true)
        .unwrap();
    assert!(removed);
    let t = tombstone.expect("default ontology has a tombstone type");
    let body = t.body.as_deref().unwrap();
    assert!(
        body.contains("The vendor SDK handles retries and auth for us."),
        "victim body carried: {body}"
    );
    assert!(
        body.contains("vendor sunset the SDK"),
        "reason kept: {body}"
    );
    assert_eq!(t.tags, vec!["sdk".to_string()], "tags carried");
    assert_eq!(
        t.code_refs,
        vec!["src/sdk.rs".to_string()],
        "code_refs carried"
    );

    // The anchor edge moved (same id — the connection moved, it didn't
    // recur); the reason edge cascaded with the victim.
    let out = e.store().edges_out(&t.id).unwrap();
    assert_eq!(out.len(), 1, "only the anchor edge rehomes: {out:?}");
    assert_eq!(out[0].id, about.id);
    assert_eq!(out[0].edge_type, EdgeType::About);
    assert_eq!(out[0].to_id, anchor.id);
    assert!(
        e.store().edges_in(&principle.id).unwrap().is_empty(),
        "because-edge does not follow the tombstone"
    );
    assert!(e.get_node(&victim.id).unwrap().is_none(), "victim gone");

    // And the point of carrying the text: a paraphrase of the buried
    // CONTENT (not the title) now lands on the tombstone.
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Insight,
            "SDK retries and auth",
            "The vendor SDK handles retries and auth for us.",
        ))
        .unwrap();
    let WriteOutcome::Created { warnings, .. } = outcome else {
        panic!("must create")
    };
    assert!(
        warnings
            .iter()
            .any(|w| w.id == t.id && w.reason == "tombstoned"),
        "content paraphrase must hit the tombstone: {warnings:?}"
    );
}

#[test]
fn delete_with_tombstone_purge_shape_keeps_identity_and_reason_only() {
    let e = engine();
    let mut nn = new_node(
        NodeType::Decision,
        "Use the flaky vendor SDK",
        "The vendor SDK handles retries and auth for us.",
    );
    nn.tags = vec!["sdk".into()];
    let victim = e.add_node(nn).unwrap();
    let (_, tombstone) = e
        .delete_node_with_tombstone(&victim.id, Some("purge it"), false)
        .unwrap();
    let t = tombstone.unwrap();
    let body = t.body.as_deref().unwrap();
    assert!(
        !body.contains("handles retries"),
        "purge drops the text: {body}"
    );
    assert!(
        body.contains(&victim.id) && body.contains("purge it"),
        "{body}"
    );
    assert!(t.tags.is_empty(), "purge carries no tags");
}

#[test]
fn check_claim_files_tombstones_under_retracted() {
    let e = engine_with_nli();
    let t = e
        .add_node(new_node(
            NodeType::Tombstone,
            "Removed: sessions live in localStorage",
            "Deleted Decision. **Why:** moved to server sessions.",
        ))
        .unwrap();
    e.add_node(new_node(
        NodeType::Decision,
        "sessions live in localStorage",
        "",
    ))
    .unwrap();
    let report = e.check_claim("sessions live in localStorage", 8).unwrap();
    assert!(
        report.retracted.iter().any(|v| v.id == t.id),
        "tombstone lands in retracted: {report:?}"
    );
    let elsewhere = report
        .supports
        .iter()
        .chain(&report.contradicts)
        .chain(&report.silent)
        .any(|v| v.id == t.id);
    assert!(!elsewhere, "never judged by the NLI: {report:?}");
    let r = report.retracted.iter().find(|v| v.id == t.id).unwrap();
    assert!(r.entailment > 0.0 && r.neutral == 0.0 && r.contradiction == 0.0);
}

#[test]
fn an_authored_tombstone_that_replaces_its_victim_buries_it_traceably() {
    // The assistant's bury gesture: no hard delete, the victim is archived
    // behind `replaces` and a later resurrection warns on BOTH records.
    let e = engine();
    let victim = e
        .add_node(new_node(
            NodeType::Decision,
            "sessions live in redis",
            "fast and shared",
        ))
        .unwrap();
    let outcome = e
        .add_node_checked(new_node(
            NodeType::Tombstone,
            "Removed: sessions live in redis",
            "Deleted Decision. **Why:** redis was dropped from the stack.",
        ))
        .unwrap();
    let WriteOutcome::Created { node: t, .. } = outcome else {
        panic!("a tombstone is a different type from its victim — it creates")
    };
    e.add_edge(NewEdge {
        edge_type: EdgeType::Replaces,
        from_id: t.id.clone(),
        to_id: victim.id.clone(),
        source: Source::Agent,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();
    let buried = e.get_node(&victim.id).unwrap().unwrap();
    assert!(buried.valid_until.is_some(), "replaces archives the victim");

    let outcome = e
        .add_node_checked(new_node(
            NodeType::Insight,
            "sessions live in redis",
            "fast and shared",
        ))
        .unwrap();
    let WriteOutcome::Created { warnings, .. } = outcome else {
        panic!("must create")
    };
    assert!(
        warnings
            .iter()
            .any(|w| w.id == victim.id && w.reason == "superseded"),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.id == t.id && w.reason == "tombstoned"),
        "{warnings:?}"
    );
}
