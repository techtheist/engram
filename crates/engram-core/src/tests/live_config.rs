//! The engine on live config: policy numbers, brief composition, ontology.

use super::*;

/// A fully renamed ontology: none of the shipped names survive, the role
/// flags carry every behavior.
pub(super) fn custom_ontology() -> GraphConfig {
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
                tombstone: false,
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
                tombstone: false,
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
                tombstone: false,
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
                inherits: None,
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
                inherits: None,
            },
        },
    ];
    cfg
}

pub(super) fn custom_node(t: &str, title: &str, body: &str) -> NewNode {
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
                source: Source::Agent,
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
                source: Source::Agent,
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
                source: Source::Agent,
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
    assert_eq!(shelf.len(), 4);
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
    // The 0.9.0 tombstone role: every preset except minimal carries a
    // deletion-marker type (minimal stays ceremony-free by design).
    for id in ["engram", "research", "general"] {
        let p = shelf.iter().find(|p| p.id == id).unwrap();
        assert!(
            p.config.ontology.types.iter().any(|t| t.roles.tombstone),
            "preset {id} has no tombstone type"
        );
    }
    assert_eq!(
        GraphConfig::default().tombstone_type(),
        Some("Tombstone"),
        "the default ontology's deletion marker"
    );
}

/// 0.9.12 (Caution 00e3547lopga): applying a preset swaps the ontology and
/// nothing else — a tuned policy, the brief, version tracking and custom
/// field definitions all survive.
#[test]
fn applying_a_preset_keeps_tuned_policy_brief_versioning_and_fields() {
    let e = engine();
    let mut cfg = e.graph_config();
    cfg.policy.conflict_suspect_similarity = 0.8897;
    cfg.policy.weak_evidence_top = 0.72;
    cfg.brief.recent.cap = 3;
    cfg.versioning.enabled = !cfg.versioning.enabled;
    cfg.fields = vec![fdef("owner", crate::config::FieldKind::Text)];
    e.set_graph_config(&cfg).unwrap();

    let applied = e.apply_preset("research").unwrap();
    let research = crate::config::presets()
        .into_iter()
        .find(|p| p.id == "research")
        .unwrap();
    assert_eq!(
        applied.ontology, research.config.ontology,
        "the ontology is the preset's"
    );
    let stored = e.graph_config();
    assert_eq!(stored, applied, "the answer is what was stored");
    assert_eq!(stored.policy, cfg.policy, "the tuned policy survives");
    assert_eq!(stored.brief, cfg.brief);
    assert_eq!(stored.versioning, cfg.versioning);
    assert_eq!(stored.history, cfg.history);
    assert_eq!(stored.fields, cfg.fields);

    // Back to the default ontology: still only the ontology moves.
    let back = e.apply_preset("engram").unwrap();
    assert_eq!(back.ontology, GraphConfig::default().ontology);
    assert_eq!(back.policy.weak_evidence_top, 0.72);
}

#[test]
fn applying_a_preset_refuses_unknown_ids_and_keeps_the_in_use_guard() {
    let e = engine();
    let err = e.apply_preset("nope").unwrap_err().to_string();
    assert!(
        err.contains("unknown preset") && err.contains("research"),
        "{err}"
    );

    // A Decision exists; `minimal` has no Decision type — the same refusal a
    // PUT would get, and the stored config is untouched.
    e.add_node(new_node(NodeType::Decision, "Use Rust", "for the backend"))
        .unwrap();
    let before = e.graph_config();
    let err = e.apply_preset("minimal").unwrap_err().to_string();
    assert!(
        err.contains("Decision") && err.contains("still has"),
        "{err}"
    );
    assert_eq!(e.graph_config(), before);
}

// ---------------------------------------------------------------------------
// 0.9.0: the tombstone role — deletion finally leaves a trace.
// ---------------------------------------------------------------------------
