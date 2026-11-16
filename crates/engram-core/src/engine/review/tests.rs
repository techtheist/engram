use super::subjects::version_order;
use super::*;
use crate::{Durability, EdgeType, Embedder, NewEdge, NewNode, NodePatch, Source, SqliteStore};

const DIM: usize = 384;

/// Vectors with subjects: a `#word` marker puts the text on that word's
/// axis, and the whole text adds a smaller axis of its own — same-subject
/// notes sit at ~0.92, everything else near 0. Not fake, so the brief
/// treats it as real.
struct TopicEmbedder;

fn axis(text: &str) -> usize {
    (fnv(&[text.as_bytes()]) % DIM as u64) as usize
}

impl Embedder for TopicEmbedder {
    fn dim(&self) -> usize {
        DIM
    }
    fn name(&self) -> &str {
        "topic-test"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; DIM];
                if let Some(tag) = t.split_whitespace().find(|w| w.starts_with('#')) {
                    v[axis(tag)] += 1.0;
                }
                // Spread over three axes, so two strangers that share
                // one (384 axes, birthday odds) stay far apart.
                for salt in ["a", "b", "c"] {
                    v[axis(&format!("{salt}{t}"))] += 0.3 / 3f32.sqrt();
                }
                v
            })
            .collect())
    }
}

fn engine() -> Engine {
    Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(TopicEmbedder),
    )
}

fn add(e: &Engine, title: &str) -> Node {
    e.add_node(NewNode {
        node_type: NodeType::Decision,
        title: title.to_string(),
        body: Some(format!("{title} — body text")),
        created_at: None,
        durability: Durability::Stable,
        source: Source::Agent,
        session_id: None,
        status: None,
        code_refs: vec![],
        tags: vec![],
        version: None,
        props: None,
        fields: None,
    })
    .unwrap()
}

/// Six notes on #bridge, five on #brief, and 30 strangers: 41 notes,
/// so a subject needs 5 (the floor; 2% would be 1).
fn crowded() -> (Engine, Vec<Node>, Vec<Node>) {
    let e = engine();
    let bridge: Vec<Node> = (0..6)
        .map(|i| add(&e, &format!("#bridge note {i} on reconnects")))
        .collect();
    let brief: Vec<Node> = (0..5)
        .map(|i| add(&e, &format!("#brief note {i} on sections")))
        .collect();
    for i in 0..30 {
        add(&e, &format!("stranger {i} about something else entirely"));
    }
    (e, bridge, brief)
}

fn query() -> ReviewQuery {
    ReviewQuery {
        limit: 10,
        ..Default::default()
    }
}

fn ids(nodes: &[Node]) -> Vec<String> {
    let mut v: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    v.sort();
    v
}

fn member_ids(c: &ReviewSubject) -> Vec<String> {
    let mut v: Vec<String> = c.members.iter().map(|m| m.id.clone()).collect();
    v.sort();
    v
}

#[test]
fn the_review_queue_lists_subjects_most_worth_first() {
    let (e, bridge, brief) = crowded();
    let page = e.review_page(&query()).unwrap();
    assert_eq!((page.min_size, page.max_size), (5, 20));
    assert_eq!(page.total, 2, "two piles, no stranger subjects");
    assert_eq!(member_ids(&page.subjects[0]), ids(&bridge));
    assert_eq!(member_ids(&page.subjects[1]), ids(&brief));
    let first = &page.subjects[0];
    assert_eq!(first.size, 6);
    assert_eq!(
        first.pairs.len(),
        PAIRS,
        "15 close pairs, the closest 12 listed"
    );
    assert!(first.more.is_empty());
    assert!(first.members[0].similarity.is_none(), "the centre");
    assert!(
        first.members[1..]
            .iter()
            .all(|m| m.similarity.unwrap() > 0.9)
    );
    assert!(first.members.iter().all(|m| m.crowding == 5));
    assert!(first.members[0].snippet.contains("body text"));
    // All of this cycle, all new: live work, worth only its size × 0.3.
    assert_eq!(first.worth.settled, 0.0);
    assert_eq!(first.worth.score, 1.8);
}

#[test]
fn settled_history_outranks_a_bigger_live_pile() {
    let e = engine();
    e.set_current_version(Some("0.3.0")).unwrap();
    let month_ago = crate::now() - 40 * 86_400;
    for (i, v) in ["0.1.0", "0.1.0", "0.2.0", "0.2.0", "0.2.1"]
        .iter()
        .enumerate()
    {
        let title = format!("#cutover note {i} on the store move");
        e.add_node(NewNode {
            created_at: Some(month_ago + i as i64 * 5 * 86_400),
            version: Some(v.to_string()),
            body: Some(format!("{title} — body text")),
            title,
            ..new_like(&add(&e, "seed stranger for the shape"))
        })
        .unwrap();
    }
    for i in 0..8 {
        add(&e, &format!("#cycle note {i} on this release"));
    }
    for i in 0..30 {
        add(&e, &format!("stranger {i} about something else entirely"));
    }
    let page = e.review_page(&query()).unwrap();
    let top = &page.subjects[0];
    assert!(
        top.members.iter().all(|m| m.title.contains("#cutover")),
        "{:?}",
        top.members[0].title
    );
    assert_eq!(top.worth.versions, 3);
    assert_eq!(top.worth.span_days, 20);
    assert_eq!(top.versions, Some(("0.1.0".into(), "0.2.1".into())));
    let live = page
        .subjects
        .iter()
        .find(|s| s.members[0].title.contains("#cycle"))
        .unwrap();
    assert!(live.size < top.size || live.worth.score < top.worth.score);
    assert!(top.worth.score > live.worth.score);
}

#[test]
fn an_unstamped_note_takes_the_version_current_when_it_was_written() {
    let e = engine();
    let day = 86_400;
    let t0 = crate::now() - 90 * day;
    let mk = |title: &str, at: i64| NewNode {
        created_at: Some(at),
        version: None,
        body: Some(format!("{title} — body text")),
        title: title.to_string(),
        ..new_like(&add(&e, "seed stranger for the shape"))
    };
    // Written before any set_version, then while 0.2.0 was current.
    let early = e.add_node(mk("#era note before the journal", t0)).unwrap();
    e.set_current_version(Some("0.2.0")).unwrap();
    let clock = e.version_clock().unwrap();
    let later = Node {
        created_at: crate::now() + 1,
        ..early.clone()
    };
    assert_eq!(clock.version_of(&early).as_deref(), Some("<0.2.0"));
    assert_eq!(clock.version_of(&later).as_deref(), Some("0.2.0"));
    let stamped = Node {
        version: Some("v0.1.5".into()),
        ..early
    };
    assert_eq!(clock.version_of(&stamped).as_deref(), Some("0.1.5"));
    let mut vs = vec!["0.10.0", "<0.2.0", "0.2.0"];
    vs.sort_by(|a, b| version_order(a, b));
    assert_eq!(vs, ["<0.2.0", "0.2.0", "0.10.0"]);
}

#[test]
fn review_pages_and_filters() {
    let (e, _, brief) = crowded();
    let second = e
        .review_page(&ReviewQuery {
            offset: 1,
            limit: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(second.total, 2);
    assert_eq!(second.subjects.len(), 1);
    assert_eq!(member_ids(&second.subjects[0]), ids(&brief));
    let none = e
        .review_page(&ReviewQuery {
            types: vec![NodeType::Caution],
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(none.total, 0);
}

#[test]
fn a_reviewed_cluster_leaves_the_queue_until_half_its_members_change() {
    let (e, bridge, _) = crowded();
    let all: Vec<String> = bridge.iter().map(|n| n.id.clone()).collect();
    assert_eq!(e.mark_reviewed(&all, true).unwrap().len(), 6);
    let page = e.review_page(&query()).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.hidden_reviewed, 1);
    let shown = e
        .review_page(&ReviewQuery {
            include_reviewed: true,
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(shown.total, 2);
    assert!(
        shown
            .subjects
            .iter()
            .any(|s| s.members.iter().all(|m| m.reviewed))
    );

    // An edit voids that member's mark; one changed note of six keeps
    // the subject hidden, half of them brings it back.
    let rewrite = |n: &Node| {
        e.update_node(
            &n.id,
            NodePatch {
                body: Some(format!("{} rewritten", n.title)),
                ..Default::default()
            },
        )
        .unwrap();
    };
    rewrite(&bridge[2]);
    assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 1);
    rewrite(&bridge[3]);
    rewrite(&bridge[4]);
    let page = e.review_page(&query()).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.hidden_reviewed, 0);
    let back = page
        .subjects
        .iter()
        .find(|c| c.members.iter().any(|m| m.id == bridge[2].id))
        .unwrap();
    let fresh = back.members.iter().find(|m| m.id == bridge[2].id).unwrap();
    assert!(!fresh.reviewed);
    assert!(back.members.iter().filter(|m| m.reviewed).count() == 3);
}

#[test]
fn newcomers_reopen_a_reviewed_cluster_once_they_are_half() {
    let (e, bridge, _) = crowded();
    let all: Vec<String> = bridge.iter().map(|n| n.id.clone()).collect();
    e.mark_reviewed(&all, true).unwrap();
    add(&e, "#bridge note 9, a fresh restatement");
    assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 1);
    for i in 10..15 {
        add(&e, &format!("#bridge note {i}, a fresh restatement"));
    }
    let page = e.review_page(&query()).unwrap();
    assert_eq!(page.hidden_reviewed, 0);
    assert_eq!(page.total, 2);
}

#[test]
fn a_mark_moves_no_trust_field() {
    let (e, bridge, _) = crowded();
    let before = e.get_node(&bridge[0].id).unwrap().unwrap();
    e.mark_reviewed(std::slice::from_ref(&before.id), true)
        .unwrap();
    let after = e.get_node(&bridge[0].id).unwrap().unwrap();
    assert_eq!(after.confirmed_at, before.confirmed_at);
    assert_eq!(after.approved_at, before.approved_at);
    assert_eq!(after.trust, before.trust);
}

#[test]
fn unmarking_and_unknown_ids() {
    let (e, bridge, _) = crowded();
    let all: Vec<String> = bridge.iter().map(|n| n.id.clone()).collect();
    e.mark_reviewed(&all, true).unwrap();
    assert_eq!(
        e.mark_reviewed(&["nope".to_string()], true).unwrap().len(),
        0
    );
    e.mark_reviewed(&all[..1], false).unwrap();
    assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 1);
    e.mark_reviewed(&all[..3], false).unwrap();
    assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 0);
}

#[test]
fn marks_from_a_store_without_them_or_garbled_are_empty() {
    // A graph written by 0.10.4 has no key; a hand-mangled one parses
    // to nothing instead of failing every review.
    let (e, _, _) = crowded();
    assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 0);
    e.store.kv_set(MARKS_KEY, "not json").unwrap();
    let page = e.review_page(&query()).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.hidden_reviewed, 0);
}

#[test]
fn archived_notes_lose_their_marks_and_leave_the_index() {
    let (e, bridge, _) = crowded();
    let all: Vec<String> = bridge.iter().map(|n| n.id.clone()).collect();
    e.mark_reviewed(&all, true).unwrap();
    e.merge_nodes(
        &bridge[0].id,
        &all[1..],
        Some("#bridge one consolidated note".into()),
        None,
        Source::Agent,
    )
    .unwrap();
    let index = e.crowding().unwrap();
    assert!(all[1..].iter().all(|id| !index.nodes.contains_key(id)));
    // The next mark prunes the archived victims' entries.
    e.mark_reviewed(&all[..1], true).unwrap();
    let marks = e.review_marks().unwrap();
    assert_eq!(marks.len(), 1);
    assert!(marks.contains_key(&all[0]));
}

#[test]
fn the_index_is_cached_until_the_graph_changes() {
    let (e, _, _) = crowded();
    let a = e.crowding().unwrap();
    let b = e.crowding().unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    add(&e, "one more stranger");
    let c = e.crowding().unwrap();
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(c.size, a.size + 1);
}

#[test]
fn around_lists_nearest_and_subject_mates() {
    let (e, bridge, _) = crowded();
    let anchor = e
        .add_node(NewNode {
            node_type: NodeType::Anchor,
            title: "crates/engram-mcp/src/bridge.rs".into(),
            ..new_like(&bridge[0])
        })
        .unwrap();
    let tagged = e
        .add_node(NewNode {
            tags: vec!["process-model".into()],
            ..new_like(&bridge[0])
        })
        .unwrap();
    let far = add(&e, "a stranger that is about the bridge file");
    for from in [&bridge[0].id, &far.id] {
        e.add_edge(NewEdge {
            edge_type: EdgeType::About,
            from_id: from.clone(),
            to_id: anchor.id.clone(),
            source: Source::Agent,
            note: None,
            confidence: None,
            strength: None,
            status: None,
        })
        .unwrap();
    }
    e.update_node(
        &bridge[0].id,
        NodePatch {
            tags: Some(vec!["process-model".into()]),
            ..Default::default()
        },
    )
    .unwrap();

    let view = e.around(&bridge[0].id).unwrap();
    assert_eq!(view.node.id, bridge[0].id);
    // The five other #bridge notes and the tagged sibling (also #bridge)
    // tie for nearest; ties fall in id order, so read all six.
    let near: Vec<&str> = view.nearest.iter().take(6).map(|n| n.id.as_str()).collect();
    for b in bridge[1..].iter().chain([&tagged]) {
        assert!(near.contains(&b.id.as_str()), "{near:?}");
    }
    assert!(view.nearest[..6].iter().all(|n| n.close));
    assert!(view.nearest[0].close);
    let mate = |id: &str| view.subject.iter().find(|n| n.id == id);
    assert_eq!(
        mate(&far.id).unwrap().via,
        ["anchor:crates/engram-mcp/src/bridge.rs"]
    );
    assert_eq!(mate(&tagged.id).unwrap().via, ["tag:process-model"]);
    assert!(
        view.node
            .edges
            .iter()
            .any(|e| e.verb == "about" && e.dir == "out")
    );

    // An anchor's view lists what is about it.
    let view = e.around(&anchor.id).unwrap();
    assert!(view.nearest.is_empty(), "anchors are not in the index");
    let about: Vec<&str> = view.subject.iter().map(|n| n.id.as_str()).collect();
    assert!(about.contains(&far.id.as_str()) && about.contains(&bridge[0].id.as_str()));

    assert!(e.around("missing").is_err());
}

fn new_like(n: &Node) -> NewNode {
    NewNode {
        node_type: n.node_type.clone(),
        title: format!("{} (sibling)", n.title),
        body: None,
        created_at: None,
        durability: Durability::Stable,
        source: Source::Agent,
        session_id: None,
        status: None,
        code_refs: vec![],
        tags: vec![],
        version: None,
        props: None,
        fields: None,
    }
}

#[test]
fn islands_are_notes_no_edge_and_no_tag_reaches() {
    let e = engine();
    let lone = add(&e, "#delivery the floor is calibrated per graph");
    let near = add(&e, "#delivery the floor moved to phantom probes");
    let tagged = add(&e, "#bridge a tagged note");
    e.update_node(
        &tagged.id,
        NodePatch {
            tags: Some(vec!["bridge".into()]),
            ..Default::default()
        },
    )
    .unwrap();
    let a = add(&e, "#brief linked one");
    let b = add(&e, "#brief linked two");
    e.add_edge(NewEdge {
        from_id: a.id.clone(),
        to_id: b.id.clone(),
        edge_type: EdgeType::BuildsOn,
        source: Source::Agent,
        confidence: None,
        note: None,
        strength: None,
        status: None,
    })
    .unwrap();
    let page = e.islands_page(false, 0, 10).unwrap();
    let ids: Vec<&str> = page.islands.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(page.total, 2, "{ids:?}");
    assert!(ids.contains(&lone.id.as_str()) && ids.contains(&near.id.as_str()));
    // Each points at the other: the link a sleep would add.
    let first = &page.islands[0];
    assert!(first.nearest[0].id == lone.id || first.nearest[0].id == near.id);
    assert!(first.nearest[0].close);

    e.mark_reviewed(std::slice::from_ref(&lone.id), true)
        .unwrap();
    let page = e.islands_page(false, 0, 10).unwrap();
    assert_eq!((page.total, page.hidden_reviewed), (1, 1));
    assert_eq!(e.islands_page(true, 0, 10).unwrap().total, 2);
}

#[test]
fn the_conflicts_view_lists_standing_edges_with_the_newer_side() {
    let e = engine();
    let old = add(&e, "#delivery the floor is 0.22");
    let new = add(&e, "#delivery the floor is calibrated");
    e.add_edge(NewEdge {
        from_id: new.id.clone(),
        to_id: old.id.clone(),
        edge_type: EdgeType::ConflictsWith,
        source: Source::Agent,
        confidence: None,
        note: Some("measured".into()),
        strength: None,
        status: None,
    })
    .unwrap();
    let view = e.conflicts_view().unwrap();
    assert_eq!(view.standing.len(), 1);
    let c = &view.standing[0];
    assert_eq!(
        (c.a.id.as_str(), c.b.id.as_str()),
        (new.id.as_str(), old.id.as_str())
    );
    assert_eq!(c.newer, "a");
    assert_eq!(c.note.as_deref(), Some("measured"));
}

#[test]
fn a_pile_below_the_minimum_is_no_subject() {
    let e = engine();
    for i in 0..4 {
        add(&e, &format!("#bridge note {i} on reconnects"));
    }
    for i in 0..30 {
        add(&e, &format!("stranger {i} about something else entirely"));
    }
    assert_eq!(
        e.review_page(&query()).unwrap().total,
        0,
        "a pile of 4 is a pocket"
    );
}

#[test]
fn the_report_carries_heat_and_open_subjects() {
    let (e, bridge, _) = crowded();
    let report = e.crowding_report().unwrap();
    assert_eq!(report.size, 41);
    assert_eq!(report.min_size, 5);
    assert_eq!(report.subjects.len(), 2);
    assert_eq!(report.subjects[0].members.len(), 6);
    assert!(
        report.subjects[0]
            .name
            .as_deref()
            .is_some_and(|n| n.contains("bridge"))
    );
    assert_eq!(report.nodes[&bridge[0].id].crowding, 5);
    assert_eq!(report.nodes[&bridge[0].id].heat, 1.0);
    assert!(serde_json::to_value(&report).unwrap()["nodes"].is_object());
}
