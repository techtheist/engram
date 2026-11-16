//! File-read matches, the activity journal, stale triage, canon checks.

use super::*;

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
        WriteOutcome::Matched { .. } => panic!("must create"),
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
        WriteOutcome::Matched { .. } => panic!("cross-type text never dupe-matches"),
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
