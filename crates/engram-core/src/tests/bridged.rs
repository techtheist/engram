//! Linked-suspect settling and bridged contradictions.

use super::*;

#[test]
fn a_link_settles_the_pending_suspect_between_its_notes() {
    let e = engine();
    let summary = e
        .add_node(new_node(
            NodeType::Insight,
            "The store move as of 0.8",
            "every graph on one core",
        ))
        .unwrap();
    let member = e
        .add_node(new_node(
            NodeType::Decision,
            "One core owns every store",
            "lock",
        ))
        .unwrap();
    let other = e
        .add_node(new_node(
            NodeType::Decision,
            "Old stores stay read-only",
            "legacy",
        ))
        .unwrap();
    let s1 = e
        .store()
        .add_suspect(
            &summary.id,
            &member.id,
            0.9,
            Some(("contradiction", 0.91, None)),
        )
        .unwrap();
    let s2 = e
        .store()
        .add_suspect(&summary.id, &other.id, 0.9, None)
        .unwrap();
    let link = |from: &Node, to: &Node, verb: EdgeType| {
        e.add_edge(NewEdge {
            edge_type: verb,
            from_id: from.id.clone(),
            to_id: to.id.clone(),
            source: Source::Agent,
            note: None,
            confidence: None,
            strength: None,
            status: None,
        })
        .unwrap()
    };
    // A summary building on a member says they agree: dismissed.
    link(&summary, &member, EdgeType::BuildsOn);
    let s1 = e.store().get_suspect(&s1.id).unwrap().unwrap();
    assert_eq!(s1.status, SuspectStatus::Dismissed);
    // Replacing one is the suspect's own verdict: confirmed.
    link(&summary, &other, EdgeType::Replaces);
    let s2 = e.store().get_suspect(&s2.id).unwrap().unwrap();
    assert_eq!(s2.status, SuspectStatus::Confirmed);
    assert!(e.store().suspects_pending().unwrap().is_empty());
}

// ---- bridged contradictions ------------------------------------

/// A judge that reads a datastore bridge: "… same datastore … . <fact>" ⟹
/// <other fact> is a contradiction; everything else is FakeNli's call.
pub(super) struct BridgeNli;

impl crate::nli::Nli for BridgeNli {
    fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
        let mut out = crate::nli::FakeNli.judge(pairs)?;
        for (j, (p, h)) in out.iter_mut().zip(pairs) {
            let fact = |t: &str| t.contains("PostgreSQL") || t.contains("MongoDB");
            if p.contains("same datastore") && fact(p) && fact(h) {
                *j = NliJudgment {
                    entailment: 0.02,
                    neutral: 0.05,
                    contradiction: 0.93,
                };
            }
        }
        Ok(out)
    }
}

pub(super) fn bridge_engine() -> Engine {
    let mut e = engine();
    e.set_nli(Box::new(BridgeNli));
    e
}

pub(super) const BRIDGE: &str = "The Export worker uses the same datastore as the Import worker";
pub(super) const FACT_X: &str = "Export worker keeps its records in PostgreSQL";
pub(super) const FACT_Y: &str = "Import worker is backed by MongoDB";

pub(super) fn bridged_views(views: &[SuspectView]) -> Vec<&SuspectView> {
    views
        .iter()
        .filter(|s| s.nli_label.as_deref() == Some("bridged"))
        .collect()
}

#[test]
fn a_new_bridge_queues_the_two_facts_it_makes_disagree() {
    let e = bridge_engine();
    let x = e
        .add_node(new_node(NodeType::Decision, FACT_X, ""))
        .unwrap();
    let y = e.add_node(new_node(NodeType::Caution, FACT_Y, "")).unwrap();
    let WriteOutcome::Created {
        node: bridge,
        suspects,
        ..
    } = e
        .add_node_checked(new_node(NodeType::Insight, BRIDGE, ""))
        .unwrap()
    else {
        panic!("created")
    };
    // The verdict carries the pair even though it does not involve the
    // written note: the writer is the one who should judge it.
    let bridged = bridged_views(&suspects);
    assert_eq!(bridged.len(), 1, "{suspects:?}");
    let s = bridged[0];
    let ends = [s.a.id.as_str(), s.b.id.as_str()];
    assert!(ends.contains(&x.id.as_str()) && ends.contains(&y.id.as_str()));
    assert_eq!(
        s.via.as_ref().map(|v| v.id.as_str()),
        Some(bridge.id.as_str())
    );
    // The brief names the bridge.
    let brief = e.brief(e.brief_chars(None)).unwrap();
    assert!(brief.contains(&format!("via \"{BRIDGE}\"")), "{brief}");
}

#[test]
fn a_new_fact_meets_an_older_bridge_that_names_its_subject() {
    let e = bridge_engine();
    let y = e.add_node(new_node(NodeType::Caution, FACT_Y, "")).unwrap();
    let bridge = e.add_node(new_node(NodeType::Insight, BRIDGE, "")).unwrap();
    let WriteOutcome::Created {
        node: x, suspects, ..
    } = e
        .add_node_checked(new_node(NodeType::Decision, FACT_X, ""))
        .unwrap()
    else {
        panic!("created")
    };
    let bridged = bridged_views(&suspects);
    assert_eq!(bridged.len(), 1, "{suspects:?}");
    let s = bridged[0];
    assert_eq!(s.a.id, x.id, "newer first");
    assert_eq!(s.b.id, y.id);
    assert_eq!(
        s.via.as_ref().map(|v| v.id.as_str()),
        Some(bridge.id.as_str())
    );
    // Stored, not only reported.
    let stored = e.store().get_suspect(&s.id).unwrap().unwrap();
    assert_eq!(stored.via.as_deref(), Some(bridge.id.as_str()));
}

#[test]
fn a_bridge_queues_nothing_when_the_gate_is_off_or_the_facts_agree() {
    // Gate off.
    let e = bridge_engine();
    let mut cfg = e.graph_config();
    cfg.policy.conflict_bridge_gate = None;
    e.set_graph_config(&cfg).unwrap();
    e.add_node(new_node(NodeType::Decision, FACT_X, ""))
        .unwrap();
    e.add_node(new_node(NodeType::Caution, FACT_Y, "")).unwrap();
    let WriteOutcome::Created { suspects, .. } = e
        .add_node_checked(new_node(NodeType::Insight, BRIDGE, ""))
        .unwrap()
    else {
        panic!("created")
    };
    assert!(bridged_views(&suspects).is_empty());

    // Two notes making the same claim about the two subjects: a bridge
    // that ties the subjects cannot make them disagree.
    let e = bridge_engine();
    e.add_node(new_node(
        NodeType::Decision,
        "Export worker drops writes when PostgreSQL fails over",
        "",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Caution,
        "Import worker drops writes when PostgreSQL fails over",
        "",
    ))
    .unwrap();
    let WriteOutcome::Created { suspects, .. } = e
        .add_node_checked(new_node(NodeType::Insight, BRIDGE, ""))
        .unwrap()
    else {
        panic!("created")
    };
    assert!(bridged_views(&suspects).is_empty(), "{suspects:?}");
}

#[test]
fn a_fact_about_a_subject_no_bridge_names_pairs_nothing() {
    let e = bridge_engine();
    e.add_node(new_node(NodeType::Caution, FACT_Y, "")).unwrap();
    e.add_node(new_node(NodeType::Insight, BRIDGE, "")).unwrap();
    let WriteOutcome::Created { suspects, .. } = e
        .add_node_checked(new_node(
            NodeType::Decision,
            "Billing service keeps its records in PostgreSQL",
            "",
        ))
        .unwrap()
    else {
        panic!("created")
    };
    assert!(bridged_views(&suspects).is_empty(), "{suspects:?}");
}

#[test]
fn the_conflict_sweep_reads_bridges_written_before_the_path_existed() {
    let e = bridge_engine();
    // Plain writes: no write-time check ran.
    e.add_node(new_node(NodeType::Decision, FACT_X, ""))
        .unwrap();
    e.add_node(new_node(NodeType::Caution, FACT_Y, "")).unwrap();
    e.add_node(new_node(NodeType::Insight, BRIDGE, "")).unwrap();
    let sweep = e.audit_conflicts().unwrap();
    assert_eq!(sweep.bridged, 1, "{sweep:?}");
    assert!(sweep.queued >= 1);
    // Raised once: the next run finds it already in the queue.
    let again = e.audit_conflicts().unwrap();
    assert_eq!(again.bridged, 0, "{again:?}");
}

#[test]
fn found_bridges_show_the_notes_a_bridge_ties_in_until_an_edge_does() {
    let e = bridge_engine();
    let x = e
        .add_node(new_node(NodeType::Decision, FACT_X, ""))
        .unwrap();
    let y = e.add_node(new_node(NodeType::Caution, FACT_Y, "")).unwrap();
    let bridge = e.add_node(new_node(NodeType::Insight, BRIDGE, "")).unwrap();
    let other = e
        .add_node(new_node(
            NodeType::Principle,
            "Billing keeps its records in MongoDB",
            "",
        ))
        .unwrap();
    // A fact: the other subject's fact, through the bridge.
    let found = e.found_bridges(&x.id, 3).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].note.id, y.id);
    assert_eq!(
        found[0].via.as_ref().map(|v| v.id.as_str()),
        Some(bridge.id.as_str())
    );
    // The bridge: the notes it names, no via.
    let named = e.found_bridges(&bridge.id, 3).unwrap();
    let ids: Vec<&str> = named.iter().map(|f| f.note.id.as_str()).collect();
    assert!(ids.contains(&x.id.as_str()) && ids.contains(&y.id.as_str()));
    assert!(named.iter().all(|f| f.via.is_none()));
    // A subject no bridge names finds nothing; the cap holds.
    assert!(e.found_bridges(&other.id, 3).unwrap().is_empty());
    assert_eq!(e.found_bridges(&bridge.id, 1).unwrap().len(), 1);
    // Sleep's island queue carries them: a tie the text already states.
    let islands = e.islands_page(true, 0, 10).unwrap();
    let island = islands.islands.iter().find(|i| i.id == x.id).unwrap();
    assert_eq!(island.bridges.len(), 1, "{island:?}");
    assert_eq!(island.bridges[0].note.id, y.id);
    // Once an edge joins them, it is no longer a found bridge.
    e.add_edge(NewEdge {
        edge_type: EdgeType::BuildsOn,
        from_id: x.id.clone(),
        to_id: y.id.clone(),
        source: Source::Agent,
        note: None,
        confidence: None,
        strength: None,
        status: None,
    })
    .unwrap();
    assert!(e.found_bridges(&x.id, 3).unwrap().is_empty());
    // The many form keeps the asked order; an unknown id finds nothing.
    let many = e
        .found_bridges_many(&[bridge.id.clone(), "nope".into()], 3)
        .unwrap();
    assert_eq!(many.len(), 2);
    assert!(many[1].is_empty());
}
