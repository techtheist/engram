//! Standing verdicts and twin ordering.

use super::*;

#[test]
fn search_hits_carry_the_graphs_standing_verdict() {
    // Grounded answers: each hit says how far it can be relied on.
    let e = engine();
    let standing_of = |title: &str| {
        e.search(title, &[], 10)
            .unwrap()
            .into_iter()
            .find(|h| h.title == title)
            .and_then(|h| h.standing)
            .unwrap_or_else(|| panic!("{title} delivered with a standing"))
    };
    let lead = e
        .add_node(new_node(NodeType::Insight, "caching halves latency", "x"))
        .unwrap();
    assert_eq!(standing_of(&lead.title).verdict, "unverified");

    let endorsed = e
        .add_node(new_node(
            NodeType::Decision,
            "deploys go through canary",
            "x",
        ))
        .unwrap();
    e.approve(&endorsed.id).unwrap();
    assert_eq!(standing_of(&endorsed.title).verdict, "canon");

    let a = e
        .add_node(new_node(NodeType::Decision, "tabs for indentation", "x"))
        .unwrap();
    let b = e
        .add_node(new_node(NodeType::Decision, "spaces for indentation", "x"))
        .unwrap();
    edge(&e, EdgeType::ConflictsWith, &b.id, &a.id);
    let contested = standing_of(&a.title);
    assert_eq!(contested.verdict, "contested");
    assert_eq!(contested.other.as_deref(), Some(b.id.as_str()));

    // A pinned note survives a `replaces` onto it — and now says so.
    let old = e
        .add_node(new_node(NodeType::Decision, "queue is rabbitmq", "x"))
        .unwrap();
    e.set_trust_override(&old.id, Some(1.0)).unwrap();
    let new = e
        .add_node(new_node(NodeType::Decision, "queue is kafka now", "x"))
        .unwrap();
    edge(&e, EdgeType::Replaces, &new.id, &old.id);
    let superseded = standing_of(&old.title);
    assert_eq!(superseded.verdict, "superseded");
    assert_eq!(superseded.other.as_deref(), Some(new.id.as_str()));
}

#[test]
fn endorsed_twins_rank_first_pinned_above_approved() {
    // 0.9.10 twin order: between near-identical delivered hits the endorsed
    // one goes first — pinned above approved above neither — whatever the
    // vote said; nothing that isn't a twin moves.
    let e = engine();
    let mut cfg = e.graph_config();
    cfg.policy.twin_trust_order = Some(0.5); // the fake embedder's scale
    e.set_graph_config(&cfg).unwrap();
    let body = "set after the spring load tests; applies to production";
    let plain = e
        .add_node(new_node(
            NodeType::Decision,
            "the retry budget is three attempts",
            body,
        ))
        .unwrap();
    let approved = e
        .add_node(new_node(
            NodeType::Decision,
            "the retry budget is five attempts",
            body,
        ))
        .unwrap();
    let pinned = e
        .add_node(new_node(
            NodeType::Decision,
            "the retry budget is seven attempts",
            body,
        ))
        .unwrap();
    e.approve(&approved.id).unwrap();
    e.set_trust_override(&pinned.id, Some(1.0)).unwrap();
    let order: Vec<String> = e
        .search("retry budget", &[], 10)
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .filter(|id| [&plain.id, &approved.id, &pinned.id].contains(&id))
        .collect();
    assert_eq!(order, vec![pinned.id, approved.id, plain.id]);
}

#[test]
fn twins_on_the_same_rung_rank_the_latest_endorsement_first() {
    // 0.9.10 endorsement clock: two pinned (approved, confirmed) twins
    // compute the same trust, so which was endorsed last decides — within
    // the same second too, where the journal seq breaks the tie.
    let e = engine();
    let mut cfg = e.graph_config();
    cfg.policy.twin_trust_order = Some(0.5); // the fake embedder's scale
    e.set_graph_config(&cfg).unwrap();
    let body = "set after the spring load tests; applies to production";
    let twin = |value: &str| {
        let mut n = new_node(
            NodeType::Decision,
            &format!("the retry budget is {value} attempts"),
            body,
        );
        n.durability = Durability::Stable;
        e.add_node(n).unwrap().id
    };
    let (a, b) = (twin("three"), twin("five"));
    let order = |e: &Engine| -> Vec<String> {
        e.search("retry budget", &[], 10)
            .unwrap()
            .into_iter()
            .map(|h| h.id)
            .filter(|id| *id == a || *id == b)
            .collect()
    };
    type Endorse = fn(&Engine, &str);
    let rungs: [(&str, Endorse); 3] = [
        ("confirm", |e, id| {
            e.reconfirm(id).unwrap();
        }),
        ("approve", |e, id| {
            e.approve(id).unwrap();
        }),
        ("pin", |e, id| {
            e.set_trust_override(id, Some(1.0)).unwrap();
        }),
    ];
    for (name, endorse) in rungs {
        endorse(&e, &a);
        endorse(&e, &b);
        assert_eq!(
            order(&e),
            vec![b.clone(), a.clone()],
            "{name}: b was endorsed last"
        );
        endorse(&e, &a);
        assert_eq!(
            order(&e),
            vec![a.clone(), b.clone()],
            "{name}: a was endorsed last"
        );
    }
}
