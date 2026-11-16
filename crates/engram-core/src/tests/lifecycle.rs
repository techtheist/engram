//! Demotion, pins, drift, approval and other node lifecycle rules.

use super::*;

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
        source: Source::Agent,
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
        source: Source::Agent,
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
        source: Source::Agent,
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
        .resolve_suspect(&s.id, SuspectVerdict::Replaces, Source::Agent)
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
        source: Source::Agent,
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
        props: None,
        fields: None,
    };
    e.import(ExportGraph {
        version: EXPORT_VERSION,
        nodes: vec![node],
        edges: vec![],
        config: None,
        tasks: vec![],
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
    assert_eq!(v["source"], "agent");
}

#[test]
fn the_pre_0_11_agent_source_still_reads() {
    // Before 0.11.0 every MCP client wrote "claude" (issue #18).
    assert_eq!(Source::parse("claude").unwrap(), Source::Agent);
    let s: Source = serde_json::from_value(serde_json::json!("claude")).unwrap();
    assert_eq!(s.as_str(), "agent");
}
