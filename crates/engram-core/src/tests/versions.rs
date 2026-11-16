//! Version tracking, handoff notes, hint direction, propagation.

use super::*;

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
        // Off by default — but setting a version IS the tracking opt-in,
        // so set_current_version flips it on.
        assert!(!e.config().versioning.enabled);
        e.set_current_version(Some("v0.7.0")).unwrap();
        assert!(e.config().versioning.enabled);

        // Tracking deliberately switched off again: the stamp stops even
        // with a version still set.
        let mut cfg = GraphConfig::default();
        cfg.versioning.enabled = false;
        e.set_graph_config(&cfg).unwrap();
        let off = e
            .add_node(new_node(NodeType::Decision, "tracking off", "x"))
            .unwrap();
        assert_eq!(off.version, None);

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
fn setting_a_version_enables_tracking_clearing_never_toggles() {
    let e = engine();
    assert!(!e.config().versioning.enabled);
    // Clearing on a graph that never tracked stays a no-op.
    e.set_current_version(None).unwrap();
    assert!(!e.config().versioning.enabled);
    // Setting is the opt-in gesture: tracking turns on and the very next
    // version-bound write carries the stamp.
    e.set_current_version(Some("0.7.1")).unwrap();
    assert!(e.config().versioning.enabled);
    let d = e
        .add_node(new_node(NodeType::Decision, "first tracked note", "x"))
        .unwrap();
    assert_eq!(d.version.as_deref(), Some("0.7.1"));
    // Clearing the version leaves tracking on.
    e.set_current_version(None).unwrap();
    assert!(e.config().versioning.enabled);
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
fn current_cycle_work_leads_the_brief_under_the_version_line() {
    let e = engine();
    let open = |title: &str, t: NodeType| {
        let mut n = new_node(t, title, "owed");
        n.status = Some(NodeStatus::Open);
        e.add_node(n).unwrap()
    };
    e.set_current_version(Some("0.9.8")).unwrap();
    let old = open("last cycle's leftover intent", NodeType::Intent);
    e.set_current_version(Some("0.9.9")).unwrap();
    open("measure the new judge on every shape", NodeType::Intent);
    open("the bench pads every row to 512", NodeType::Problem);

    let brief = e.brief(16_000).unwrap();
    let cycle = brief.find("## Current cycle\n").expect("cycle section");
    assert_eq!(cycle, brief.find("##").unwrap(), "first section: {brief}");
    // The section ends at the next heading; other versions' work lives on
    // below it (Recently added or Open, whichever claims it first).
    let end = cycle + 2 + brief[cycle + 2..].find("\n## ").unwrap();
    let body = &brief[cycle..end];
    assert!(body.contains("measure the new judge on every shape"));
    assert!(body.contains("the bench pads every row to 512"));
    // The working version is named once up top, not on every line; an older
    // stamp still shows where it lands.
    assert_eq!(
        brief.matches("0.9.9").count(),
        2,
        "version line + during_version hint: {brief}"
    );
    assert!(brief.contains("0.9.8 open]"), "{brief}");
    assert!(
        !body.contains(&old.title),
        "other versions stay out: {brief}"
    );
    assert!(brief[end..].contains(&old.title));
    // Each note appears once.
    assert_eq!(brief.matches("measure the new judge").count(), 1);

    // Tracking switched off: no section even with a version still set, and
    // the brief is exactly what a graph that never tracked versions gets —
    // the eval benches (versioning off) keep their brief byte for byte.
    let mut cfg = e.config().as_ref().clone();
    cfg.versioning.enabled = false;
    e.set_graph_config(&cfg).unwrap();
    let off = e.brief(16_000).unwrap();
    assert!(!off.contains("## Current cycle"), "{off}");
    assert!(!off.contains("Current working version"));

    // No working version, no section.
    cfg.versioning.enabled = true;
    e.set_graph_config(&cfg).unwrap();
    e.set_current_version(None).unwrap();
    assert!(!e.brief(16_000).unwrap().contains("## Current cycle"));
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
fn brief_bodies_and_canon_order_are_graph_settings() {
    let e = engine();
    let lonely = e
        .add_node(new_node(
            NodeType::Decision,
            "a lonely decision",
            "its body text",
        ))
        .unwrap();
    let hub = e
        .add_node(new_node(
            NodeType::Decision,
            "a load-bearing decision",
            "hub body",
        ))
        .unwrap();
    e.store().backdate_node(&hub.id, now() - 5_000).unwrap();
    for i in 0..4 {
        let n = e
            .add_node(new_node(
                NodeType::Insight,
                &format!("leans on it {i}"),
                "x",
            ))
            .unwrap();
        edge(&e, EdgeType::BuildsOn, &n.id, &hub.id);
    }
    let mut cfg = e.config().as_ref().clone();
    cfg.brief.recent.show = false; // let the canon section place both
    e.set_graph_config(&cfg).unwrap();

    // Defaults: bodies on, endorsed order (newest capture first).
    let brief = e.brief(16_000).unwrap();
    assert!(brief.contains("a lonely decision [Decision"));
    assert!(brief.contains("— its body text"), "{brief}");
    assert!(brief.find(&lonely.title) < brief.find(&hub.title));

    // Titles only, connected canon first.
    cfg.brief.bodies = false;
    cfg.brief.canon_order = "connected".into();
    e.set_graph_config(&cfg).unwrap();
    let brief = e.brief(16_000).unwrap();
    assert!(!brief.contains(" — its body text"), "{brief}");
    assert!(!brief.contains(" — hub body"));
    assert!(
        brief.find(&hub.title) < brief.find(&lonely.title),
        "{brief}"
    );

    // Unknown orders are refused with the roster.
    cfg.brief.canon_order = "random".into();
    let err = e.set_graph_config(&cfg).unwrap_err().to_string();
    assert!(
        err.contains("endorsed") && err.contains("connected"),
        "{err}"
    );
}

#[test]
fn a_contradiction_reaches_what_inherits_from_its_target() {
    // The transitive shape: X builds on G's setting, a new note flips
    // G's setting. The judge reads (G-old, G-new) — same subject — and the
    // builds-on edge carries the contradiction to X.
    let e = engine_with_nli();
    let upstream = e
        .add_node(new_node(
            NodeType::Decision,
            "contra: the sibling ingester retries seven times",
            "law",
        ))
        .unwrap();
    e.store()
        .backdate_node(&upstream.id, now() - 2_000)
        .unwrap();
    let dependent = e
        .add_node(new_node(
            NodeType::Decision,
            "the harbor ingester mirrors its sibling's settings",
            "same template",
        ))
        .unwrap();
    e.store()
        .backdate_node(&dependent.id, now() - 1_000)
        .unwrap();
    edge(&e, EdgeType::BuildsOn, &dependent.id, &upstream.id);
    // An `about` edge carries no inheritance.
    let bystander = e
        .add_node(new_node(NodeType::Insight, "retry budgets drift", "noted"))
        .unwrap();
    edge(&e, EdgeType::About, &bystander.id, &upstream.id);

    let flip = e
        .add_node_checked(new_node(
            NodeType::Insight,
            "contra neg: the sibling ingester retries seven times!!",
            "law",
        ))
        .unwrap();
    let WriteOutcome::Created { node: flip, .. } = flip else {
        panic!("the flip is a new note, not a match");
    };
    let flip_id = flip.id.clone();
    let suspects = e.suspects().unwrap();
    let pair = |x: &str, y: &str| {
        suspects
            .iter()
            .find(|s| (s.a.id == x && s.b.id == y) || (s.a.id == y && s.b.id == x))
    };
    assert_eq!(
        pair(&flip_id, &upstream.id).and_then(|s| s.nli_label.as_deref()),
        Some("contradiction")
    );
    let inherited = pair(&flip_id, &dependent.id).expect("the dependent is queued");
    assert_eq!(inherited.nli_label.as_deref(), Some("inherited"));
    assert!(
        pair(&flip_id, &bystander.id).is_none(),
        "about doesn't inherit"
    );
    assert!(e.brief(16_000).unwrap().contains("hint: inherited"));
}

#[test]
fn a_judged_link_propagates_but_a_tombstone_does_not() {
    let e = engine();
    let upstream = e
        .add_node(new_node(
            NodeType::Decision,
            "the sibling retries seven times",
            "x",
        ))
        .unwrap();
    let dependent = e
        .add_node(new_node(
            NodeType::Decision,
            "the harbor mirrors the sibling",
            "x",
        ))
        .unwrap();
    edge(&e, EdgeType::Needs, &dependent.id, &upstream.id);
    let flip = e
        .add_node(new_node(
            NodeType::Decision,
            "the sibling retries nineteen times",
            "x",
        ))
        .unwrap();
    edge(&e, EdgeType::ConflictsWith, &flip.id, &upstream.id);
    let suspects = e.suspects().unwrap();
    assert_eq!(suspects.len(), 1, "{suspects:?}");
    assert_eq!(suspects[0].nli_label.as_deref(), Some("inherited"));
    let ids = [suspects[0].a.id.as_str(), suspects[0].b.id.as_str()];
    assert!(ids.contains(&flip.id.as_str()) && ids.contains(&dependent.id.as_str()));

    // Burying the upstream is the user's gesture, not a new claim.
    let other_up = e
        .add_node(new_node(
            NodeType::Decision,
            "the cache keeps ten entries",
            "x",
        ))
        .unwrap();
    let other_dep = e
        .add_node(new_node(
            NodeType::Insight,
            "the pane assumes the cache size",
            "x",
        ))
        .unwrap();
    edge(&e, EdgeType::BuildsOn, &other_dep.id, &other_up.id);
    let tomb = e
        .add_node(new_node(
            NodeType::Tombstone,
            "Removed: the cache keeps ten entries",
            "wrong",
        ))
        .unwrap();
    edge(&e, EdgeType::Replaces, &tomb.id, &other_up.id);
    assert_eq!(
        e.suspects().unwrap().len(),
        1,
        "no suspect from a tombstone"
    );

    // An explicit `inherits: false` on a verb turns it off for this graph.
    let mut cfg = e.config().as_ref().clone();
    for v in &mut cfg.ontology.verbs {
        if v.name == "needs" {
            v.roles.inherits = Some(false);
        }
    }
    e.set_graph_config(&cfg).unwrap();
    let up3 = e
        .add_node(new_node(NodeType::Decision, "the queue drains hourly", "x"))
        .unwrap();
    let dep3 = e
        .add_node(new_node(
            NodeType::Decision,
            "reports assume hourly drains",
            "x",
        ))
        .unwrap();
    edge(&e, EdgeType::Needs, &dep3.id, &up3.id);
    let flip3 = e
        .add_node(new_node(NodeType::Decision, "the queue drains daily", "x"))
        .unwrap();
    edge(&e, EdgeType::ConflictsWith, &flip3.id, &up3.id);
    assert_eq!(e.suspects().unwrap().len(), 1, "needs no longer inherits");
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
        source: Source::Agent,
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
        source: Source::Agent,
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
