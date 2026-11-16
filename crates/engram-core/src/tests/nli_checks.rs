//! NLI hints, check_claim and the Checkup sweeps.

use super::*;

#[test]
fn write_time_suspects_carry_nli_hints() {
    let e = engine_with_nli();
    // FakeNli: both texts containing "contra" → contradiction hint.
    e.add_node(new_node(
        NodeType::Decision,
        "contra: sessions in redis",
        "",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Decision,
        "contra: sessions in redis!",
        "",
    ))
    .unwrap();
    e.scan_conflicts().unwrap();
    let s = e.suspects().unwrap().remove(0);
    assert_eq!(s.nli_label.as_deref(), Some("contradiction"));
    assert!(s.nli_score.unwrap() > 0.5, "hint carries its probability");

    // The brief surfaces the hint next to the pair.
    let brief = e.brief(8000).unwrap();
    assert!(brief.contains("hint: contradiction"), "got: {brief}");
}

#[test]
fn nli_agreement_scores_hints_against_verdicts() {
    let e = engine_with_nli();
    // Four near-identical pairs: "contra" pairs draw contradiction hints,
    // containment pairs entailment hints (FakeNli). "yes" marks what the
    // judge will confirm, so each of the four cells gets at least one pair.
    // The fake embedder may also queue cross-pairs; the marker rule judges
    // those deterministically and the expected counts track them.
    for t in [
        "contra alpha yes plan",
        "contra alpha yes plan!",
        "contra beta cache rule",
        "contra beta cache rule!",
        "gamma retry policy yes",
        "gamma retry policy yes!",
        "delta log format",
        "delta log format!",
    ] {
        e.add_node(new_node(NodeType::Decision, t, "")).unwrap();
    }
    e.scan_conflicts().unwrap();
    let before = e.nli_agreement().unwrap();
    assert_eq!(before.judged, 0, "pending pairs don't count");
    assert!(before.agreement.is_none());

    let suspects = e.suspects().unwrap();
    assert!(suspects.len() >= 4, "the four intended pairs queue");
    let (mut hits, mut fa, mut misses, mut passes) = (0, 0, 0, 0);
    for s in &suspects {
        let confirm = s.a.title.contains("yes");
        let contra = s.nli_label.as_deref() == Some("contradiction");
        match (contra, confirm) {
            (true, true) => hits += 1,
            (true, false) => fa += 1,
            (false, true) => misses += 1,
            (false, false) => passes += 1,
        }
        let verdict = if confirm {
            SuspectVerdict::Conflict
        } else {
            SuspectVerdict::Dismiss
        };
        e.resolve_suspect(&s.id, verdict, Source::User).unwrap();
    }

    let r = e.nli_agreement().unwrap();
    assert_eq!(r.judged, suspects.len());
    assert_eq!(
        (r.hits, r.false_alarms, r.misses, r.passes),
        (hits, fa, misses, passes)
    );
    assert!(
        r.hits >= 1 && r.false_alarms >= 1 && r.misses >= 1 && r.passes >= 1,
        "every cell exercised: {r:?}"
    );
    assert_eq!(r.with_hint, hits + fa + misses + passes);
    let expect = (hits + passes) as f64 / r.with_hint as f64;
    assert!((r.agreement.unwrap() - expect).abs() < 1e-9);
}

#[test]
fn check_claim_buckets_supports_contradicts_silent() {
    let e = engine_with_nli();
    e.add_node(new_node(
        NodeType::Decision,
        "contra: we store sessions in cookies",
        "",
    ))
    .unwrap();
    e.add_node(new_node(NodeType::Insight, "the parser uses nom", ""))
        .unwrap();

    // FakeNli reads shared "contra" as contradiction.
    let report = e
        .check_claim("contra: sessions live in localStorage", 8)
        .unwrap();
    assert_eq!(report.contradicts.len(), 1, "{report:?}");
    assert!(report.contradicts[0].title.contains("cookies"));

    // Entailment: the claim is contained in a node's claim text.
    let report = e.check_claim("the parser uses nom", 8).unwrap();
    assert_eq!(report.supports.len(), 1, "{report:?}");

    // Without the NLI layer the check refuses instead of guessing.
    let bare = engine();
    assert!(bare.check_claim("anything", 8).is_err());
}

#[test]
fn an_unconfident_contradiction_is_reported_as_silence() {
    // The gate the contradiction benchmark bought: every asserted
    // contradiction costs a human judgment, so a verdict the model is not
    // confident about is withheld rather than spent. FakeNli scores this pair
    // at 0.9, so a floor above it must move the verdict without changing what
    // the model said.
    let e = engine_with_nli();
    e.add_node(new_node(
        NodeType::Decision,
        "contra: we store sessions in cookies",
        "",
    ))
    .unwrap();
    // A second node the model has no opinion about, so the ordering assertion
    // below has something to out-rank. FakeNli scores an unrelated pair at
    // 0.05 entailment / 0.02 contradiction.
    e.add_node(new_node(
        NodeType::Decision,
        "sessions are unrelated to the deployment pipeline",
        "",
    ))
    .unwrap();

    let report = e
        .check_claim("contra: sessions live in localStorage", 8)
        .unwrap();
    assert_eq!(report.contradicts.len(), 1, "ungated baseline");

    let mut cfg = e.graph_config();
    cfg.policy.claim_contradiction_min_confidence = 0.95;
    e.set_graph_config(&cfg).unwrap();

    let report = e
        .check_claim("contra: sessions live in localStorage", 8)
        .unwrap();
    assert!(report.contradicts.is_empty(), "{report:?}");
    let silent = report
        .silent
        .iter()
        .find(|v| v.title.contains("cookies"))
        .expect("the verdict is withheld, not dropped");
    // The evidence survives the gate — only the assertion is withheld, so a
    // caller can still see exactly what the model thought.
    assert!(silent.contradiction > 0.5, "{silent:?}");

    // ...and it sorts to the front of the bucket. `silent` is where the layer's
    // declined-to-assert cases collect — a gated contradiction, or a claim that
    // agrees but was judged neutral (about a fifth of them, per
    // eval/CONTRADICTIONS.md) — so the near-misses have to be readable first.
    assert_eq!(
        report.silent.len(),
        2,
        "both nodes reach the bucket, or the ordering assertion below is vacuous: {report:?}"
    );
    let front = report.silent.first().expect("non-empty");
    assert!(
        front.title.contains("cookies"),
        "strongest signal leads the bucket, got {front:?}"
    );
}

#[test]
fn audit_sweeps_queue_only_their_target_label() {
    let e = engine_with_nli();
    // A contradiction pair (shared "contra" marker) and a duplicate pair
    // (identical text, different type so the dupe guard doesn't collapse it).
    e.add_node(new_node(NodeType::Decision, "contra: deploy via ftp", ""))
        .unwrap();
    e.add_node(new_node(NodeType::Caution, "contra: deploy via ftp!", ""))
        .unwrap();
    e.add_node(new_node(
        NodeType::Decision,
        "release trains ship monthly",
        "",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Insight,
        "release trains ship monthly",
        "",
    ))
    .unwrap();

    let conflicts = e.audit_conflicts().unwrap();
    assert_eq!(
        conflicts.queued, 1,
        "only the contradiction pair: {conflicts:?}"
    );
    let duplicates = e.audit_duplicates().unwrap();
    assert_eq!(
        duplicates.queued, 1,
        "only the duplicate pair: {duplicates:?}"
    );
    assert!(!conflicts.truncated && !duplicates.truncated);

    let suspects = e.suspects().unwrap();
    assert_eq!(suspects.len(), 2);
    // Re-running queues nothing (raised pairs are never re-raised) — and
    // says why: the pair it found last time now counts once as raised, not
    // twice, and a linked pair counts as linked.
    assert_eq!(conflicts.already_raised, 0, "its own find is not a skip");
    let again = e.audit_conflicts().unwrap();
    assert_eq!(again.queued, 0);
    assert_eq!(again.examined, 0, "nothing left to judge: {again:?}");
    assert_eq!(again.already_raised, 2, "each raised pair once: {again:?}");
    let pair = &e.suspects().unwrap()[0];
    edge(&e, EdgeType::BuildsOn, &pair.a.id, &pair.b.id);
    let linked = e.audit_conflicts().unwrap();
    assert_eq!(
        (linked.already_linked, linked.already_raised),
        (1, 1),
        "{linked:?}"
    );
}

/// FakeNli that counts the pairs it is asked to judge, under a chosen model
/// id — the Checkup sweep's memory is keyed on both.
pub(super) struct CountingNli {
    pairs: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    id: &'static str,
}

impl crate::nli::Nli for CountingNli {
    fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
        self.pairs
            .fetch_add(pairs.len(), std::sync::atomic::Ordering::Relaxed);
        crate::nli::FakeNli.judge(pairs)
    }

    fn model_id(&self) -> String {
        self.id.to_string()
    }
}

/// An engine over five mutually close notes no NLI read calls duplicates —
/// up to ten close pairs, every one of them judged and passed. Returns the
/// judged-pair counter of the installed [`CountingNli`].
pub(super) fn sweep_fixture() -> (Engine, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let mut e = engine();
    let pairs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    e.set_nli(Box::new(CountingNli {
        pairs: pairs.clone(),
        id: "counting-a",
    }));
    for word in ["alpha", "bravo", "charlie", "delta", "echo"] {
        // Plain add_node: no write-time scan, the sweep has it all to do.
        e.add_node(new_node(
            NodeType::Decision,
            &format!("{word} release trains ship monthly"),
            "",
        ))
        .unwrap();
    }
    (e, pairs)
}

pub(super) const NO_BUDGET: usize = 10_000;

#[test]
fn a_sweep_judges_each_pair_once_per_run_and_never_again_unchanged() {
    // Problem 00dxmtfmipga: the sweep met every close pair from both sides
    // and judged it twice, and remembered nothing it judged without
    // queueing, so the next run read the same pairs again.
    let (e, judged) = sweep_fixture();
    let first = e.audit_sweep("entailment", 0.80, NO_BUDGET).unwrap();
    assert_eq!(first.queued, 0, "{first:?}");
    assert!(first.examined > 0, "the fixture has close pairs: {first:?}");
    assert!(first.examined <= 10, "five notes have ten pairs: {first:?}");
    assert_eq!(
        judged.load(std::sync::atomic::Ordering::Relaxed),
        2 * first.examined,
        "one two-way read per pair, never a second from the other side"
    );
    let again = e.audit_sweep("entailment", 0.80, NO_BUDGET).unwrap();
    assert_eq!(again.examined, 0, "{again:?}");
    assert_eq!(again.already_judged, first.examined, "{again:?}");
    assert_eq!(
        judged.load(std::sync::atomic::Ordering::Relaxed),
        2 * first.examined,
        "the second run asked the judge nothing"
    );
}

#[test]
fn a_truncated_sweep_continues_where_it_stopped() {
    let (full, _) = sweep_fixture();
    let total = full
        .audit_sweep("entailment", 0.80, NO_BUDGET)
        .unwrap()
        .examined;
    assert!(total > 3, "the fixture must outlast a budget of 3: {total}");

    let (e, _) = sweep_fixture();
    let mut covered = 0;
    let mut runs = 0;
    loop {
        let run = e.audit_sweep("entailment", 0.80, 3).unwrap();
        covered += run.examined;
        runs += 1;
        assert!(run.examined <= 3, "{run:?}");
        assert!(runs <= 10, "the sweep must finish, not loop: {run:?}");
        if !run.truncated {
            break;
        }
        assert_eq!(run.examined, 3, "a truncated run spent its budget");
    }
    assert_eq!(covered, total, "every pair judged exactly once across runs");
    let done = e.audit_sweep("entailment", 0.80, 3).unwrap();
    assert_eq!((done.examined, done.truncated), (0, false), "{done:?}");
    assert_eq!(done.already_judged, total);
}

#[test]
fn an_edit_or_a_new_judge_reopens_remembered_pairs() {
    let (mut e, _) = sweep_fixture();
    let total = e
        .audit_sweep("entailment", 0.80, NO_BUDGET)
        .unwrap()
        .examined;
    // Editing one note re-opens exactly its pairs: the memory is keyed on
    // content, and the edited note reads differently now.
    let edited = e
        .store()
        .scannable_nodes()
        .unwrap()
        .into_iter()
        .find(|n| n.title.starts_with("charlie"))
        .unwrap();
    e.update_node(
        &edited.id,
        NodePatch {
            body: Some("a cadence note".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let after_edit = e.audit_sweep("entailment", 0.80, NO_BUDGET).unwrap();
    assert!(after_edit.examined > 0, "{after_edit:?}");
    assert!(
        after_edit.examined < total,
        "only the edited note's pairs: {after_edit:?}"
    );
    // A different judge model: nothing remembered holds.
    let pairs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    e.set_nli(Box::new(CountingNli {
        pairs,
        id: "counting-b",
    }));
    let new_judge = e.audit_sweep("entailment", 0.80, NO_BUDGET).unwrap();
    assert_eq!(new_judge.already_judged, 0, "{new_judge:?}");
    assert!(new_judge.examined >= after_edit.examined, "{new_judge:?}");
    // The two sweeps keep separate memories: the conflict sweep has read
    // nothing yet.
    let conflicts = e.audit_conflicts().unwrap();
    assert_eq!(conflicts.already_judged, 0, "{conflicts:?}");
}

#[test]
fn the_conflict_sweep_reaches_below_the_floor_through_the_title_guard() {
    // 0.9.11: the Checkup button admits what the write path admits since
    // 0.9.4 — same subject, a shared claim word, a title contradiction at
    // the gate — so it no longer stops at the look-alike floor.
    let e = engine_with_nli();
    let (body_a, body_b) = bodies_in_band(TITLE_A, TITLE_B);
    e.add_node(new_node(NodeType::Decision, TITLE_A, &body_a))
        .unwrap();
    e.add_node(new_node(NodeType::Decision, TITLE_B, &body_b))
        .unwrap();
    let sweep = e.audit_conflicts().unwrap();
    assert_eq!(sweep.queued, 1, "{sweep:?}");
    let s = &e.suspects().unwrap()[0];
    assert!(s.similarity < e.graph_config().policy.conflict_suspect_similarity);
    assert_eq!(s.nli_label.as_deref(), Some("contradiction"));
    // Below the floor, pairs the guard refuses are not "close pairs passed
    // over" — the counters describe the band above it.
    assert_eq!(
        (sweep.already_linked, sweep.already_raised, sweep.inactive),
        (0, 0, 0)
    );
}

#[test]
fn the_conflict_sweeps_title_band_refuses_two_subjects_and_obeys_a_null_gate() {
    let other = engine_with_nli();
    let (body_a, body_b) = bodies_in_band(TITLE_A, TITLE_OTHER);
    other
        .add_node(new_node(NodeType::Decision, TITLE_A, &body_a))
        .unwrap();
    other
        .add_node(new_node(NodeType::Decision, TITLE_OTHER, &body_b))
        .unwrap();
    let sweep = other.audit_conflicts().unwrap();
    assert_eq!((sweep.queued, sweep.examined), (0, 0), "{sweep:?}");

    let off = engine_with_nli();
    let mut cfg = off.graph_config();
    cfg.policy.conflict_nli_gate = None;
    off.set_graph_config(&cfg).unwrap();
    let (body_a, body_b) = bodies_in_band(TITLE_A, TITLE_B);
    off.add_node(new_node(NodeType::Decision, TITLE_A, &body_a))
        .unwrap();
    off.add_node(new_node(NodeType::Decision, TITLE_B, &body_b))
        .unwrap();
    let sweep = off.audit_conflicts().unwrap();
    assert_eq!((sweep.queued, sweep.examined), (0, 0), "{sweep:?}");
}
