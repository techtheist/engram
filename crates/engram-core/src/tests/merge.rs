//! merge_nodes.

use super::*;

pub(super) fn edge(e: &Engine, t: EdgeType, from: &str, to: &str) -> Edge {
    e.add_edge(NewEdge {
        edge_type: t,
        from_id: from.to_string(),
        to_id: to.to_string(),
        source: Source::Agent,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap()
}

#[test]
fn merge_nodes_unions_rehomes_and_supersedes() {
    let e = engine();
    let mut sn = new_node(NodeType::Decision, "survivor", "the canonical form");
    sn.tags = vec!["kept".into()];
    let s = e.add_node(sn).unwrap();
    let mut v1n = new_node(NodeType::Decision, "victim one", "same claim, older words");
    v1n.tags = vec!["kept".into(), "extra".into()];
    let v1 = e.add_node(v1n).unwrap();
    let v2 = e
        .add_node(new_node(
            NodeType::Decision,
            "victim two",
            "same claim again",
        ))
        .unwrap();
    let x = e.add_node(new_node(NodeType::Principle, "X", "")).unwrap();
    let y = e.add_node(new_node(NodeType::Resolution, "Y", "")).unwrap();

    edge(&e, EdgeType::Because, &v1.id, &x.id); // duplicate of survivor's own
    edge(&e, EdgeType::Because, &s.id, &x.id);
    edge(&e, EdgeType::BuildsOn, &v1.id, &v2.id); // internal to the merged set
    edge(&e, EdgeType::Answers, &y.id, &v2.id); // the one that must move

    let outcome = e
        .merge_nodes(
            &s.id,
            &[v1.id.clone(), v2.id.clone()],
            None,
            Some("the merged canonical form".into()),
            Source::Agent,
        )
        .unwrap();

    // Victims archived, survivor live and updated.
    assert!(e.get_node(&v1.id).unwrap().unwrap().valid_until.is_some());
    assert!(e.get_node(&v2.id).unwrap().unwrap().valid_until.is_some());
    let s_after = outcome.survivor;
    assert!(s_after.valid_until.is_none());
    assert_eq!(s_after.body.as_deref(), Some("the merged canonical form"));
    assert!(s_after.tags.contains(&"extra".to_string()), "tags union");

    // Y -answers-> now points at the survivor, with its identity kept.
    let s_in = e.edges_in(&s.id).unwrap();
    assert!(
        s_in.iter()
            .any(|ed| ed.edge_type == EdgeType::Answers && ed.from_id == y.id)
    );
    // The duplicate because->X was skipped, not doubled.
    let because_x = e
        .edges_out(&s.id)
        .unwrap()
        .into_iter()
        .filter(|ed| ed.edge_type == EdgeType::Because && ed.to_id == x.id)
        .count();
    assert_eq!(because_x, 1);
    // Story edges: survivor replaces both victims.
    let replaces: Vec<String> = e
        .edges_out(&s.id)
        .unwrap()
        .into_iter()
        .filter(|ed| ed.edge_type == EdgeType::Replaces)
        .map(|ed| ed.to_id)
        .collect();
    assert!(replaces.contains(&v1.id) && replaces.contains(&v2.id));

    // Counts: v1 kept both its edges (dup + internal), v2 moved one.
    let m1 = outcome.merged.iter().find(|m| m.id == v1.id).unwrap();
    assert_eq!((m1.rehomed_edges, m1.skipped_edges), (0, 2));
    let m2 = outcome.merged.iter().find(|m| m.id == v2.id).unwrap();
    assert_eq!(m2.rehomed_edges, 1);
    // Archiving the victims is the point — never a warning about them.
    assert!(
        outcome
            .warnings
            .iter()
            .all(|w| w.id != v1.id && w.id != v2.id)
    );
}

#[test]
fn merge_refuses_pinned_victims_for_the_assistant_but_not_the_user() {
    let e = engine();
    let s = e
        .add_node(new_node(NodeType::Caution, "survivor", ""))
        .unwrap();
    let v = e
        .add_node(new_node(NodeType::Caution, "pinned twin", ""))
        .unwrap();
    e.set_trust_override(&v.id, Some(1.0)).unwrap();

    let refused = e.merge_nodes(
        &s.id,
        std::slice::from_ref(&v.id),
        None,
        None,
        Source::Agent,
    );
    assert!(matches!(refused, Err(Error::Pinned(_))));
    assert!(e.get_node(&v.id).unwrap().unwrap().valid_until.is_none());

    // A user merge overrides the pin, like the pane's replaces verdict.
    e.merge_nodes(&s.id, std::slice::from_ref(&v.id), None, None, Source::User)
        .unwrap();
    assert!(e.get_node(&v.id).unwrap().unwrap().valid_until.is_some());
}

#[test]
fn merge_moves_outgoing_supersession_but_never_incoming() {
    let e = engine();
    let o = e
        .add_node(new_node(NodeType::Insight, "old gen", ""))
        .unwrap();
    let v = e
        .add_node(new_node(NodeType::Insight, "victim", ""))
        .unwrap();
    let x = e
        .add_node(new_node(NodeType::Insight, "newer gen", ""))
        .unwrap();
    let s = e
        .add_node(new_node(NodeType::Insight, "survivor", ""))
        .unwrap();
    edge(&e, EdgeType::Replaces, &v.id, &o.id); // victim's own supersession: moves
    edge(&e, EdgeType::Replaces, &x.id, &v.id); // victim's retirement: stays

    e.merge_nodes(
        &s.id,
        std::slice::from_ref(&v.id),
        None,
        None,
        Source::Agent,
    )
    .unwrap();

    let s_after = e.get_node(&s.id).unwrap().unwrap();
    assert!(
        s_after.valid_until.is_none(),
        "an inherited incoming replaces would have archived the survivor"
    );
    let out: Vec<(EdgeType, String)> = e
        .edges_out(&s.id)
        .unwrap()
        .into_iter()
        .map(|ed| (ed.edge_type, ed.to_id))
        .collect();
    assert!(
        out.contains(&(EdgeType::Replaces, o.id.clone())),
        "chain compressed"
    );
    assert!(
        out.contains(&(EdgeType::Replaces, v.id.clone())),
        "story edge"
    );
    assert!(e.edges_in(&s.id).unwrap().is_empty());
    // X's retirement of the victim is untouched history.
    assert!(
        e.edges_out(&x.id)
            .unwrap()
            .iter()
            .any(|ed| ed.edge_type == EdgeType::Replaces && ed.to_id == v.id)
    );
}

#[test]
fn merge_rehomes_a_live_conflict_and_the_survivor_inherits_reconciliation() {
    let e = engine();
    let x = e
        .add_node(new_node(NodeType::Insight, "contrarian", ""))
        .unwrap();
    let v = e
        .add_node(new_node(NodeType::Insight, "victim", ""))
        .unwrap();
    let s = e
        .add_node(new_node(NodeType::Insight, "survivor", ""))
        .unwrap();
    edge(&e, EdgeType::ConflictsWith, &x.id, &v.id);

    e.merge_nodes(
        &s.id,
        std::slice::from_ref(&v.id),
        None,
        None,
        Source::Agent,
    )
    .unwrap();

    assert!(
        e.edges_in(&s.id)
            .unwrap()
            .iter()
            .any(|ed| ed.edge_type == EdgeType::ConflictsWith && ed.from_id == x.id),
        "the live conflict follows the claim onto the survivor"
    );
    // Demotion re-reconciled over the moved edge: the older endpoint carries it.
    assert!(e.get_node(&x.id).unwrap().unwrap().demoted_at.is_some());
}
