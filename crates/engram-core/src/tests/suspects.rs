//! The suspect queue and decay.

use super::*;

pub(super) fn backdate(e: &Engine, id: &str, days: i64) {
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
        .resolve_suspect(&s.id, SuspectVerdict::Replaces, Source::Agent)
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

// ---- decay --------------------------------------------------------

pub(super) fn episodic(t: NodeType, title: &str) -> NewNode {
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
