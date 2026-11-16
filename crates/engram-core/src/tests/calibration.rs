//! Search tuning, calibrated delivery and auto-tune.

use super::*;

#[test]
fn search_tuning_is_live_config_not_a_constant() {
    // The fusion balance, the semantic floor and the two cut-offs moved from
    // compile-time consts into PolicyConfig so they can be measured instead of
    // guessed. This asserts they are actually read at query time — a knob that
    // silently does nothing is worse than no knob.
    let e = engine();
    e.add_node(new_node(
        NodeType::Decision,
        "alpha beta gamma",
        "the quick brown fox",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Decision,
        "delta epsilon zeta",
        "jumps over the lazy dog",
    ))
    .unwrap();

    // One word from each title, so both documents match at different scores.
    let baseline = e.search("gamma zeta", &[], 5).unwrap();
    assert_eq!(baseline.len(), 2, "both documents should match the query");

    // Keeping only hits that reach the top hit's full score must drop the
    // weaker one — a strict shrink, so the retain() cannot be passing by
    // accident while still reading the old constant.
    let mut cfg = e.graph_config();
    cfg.policy.search_relative_cut = 1.0;
    e.set_graph_config(&cfg).unwrap();
    let cut = e.search("gamma zeta", &[], 5).unwrap();
    assert!(
        cut.len() < baseline.len(),
        "relative_cut 1.0 kept {} of {} hits",
        cut.len(),
        baseline.len()
    );

    // Restoring the default restores the second hit.
    cfg.policy.search_relative_cut = crate::policy::SEARCH_RELATIVE_CUT;
    e.set_graph_config(&cfg).unwrap();
    assert_eq!(e.search("gamma zeta", &[], 5).unwrap().len(), 2);

    // Values outside 0..=1 are refused rather than silently clamped.
    cfg.policy.search_min_score = 10.0;
    assert!(e.set_graph_config(&cfg).is_err());
}

#[test]
fn keyword_weight_moves_the_fusion_balance() {
    let e = engine();
    let mut cfg = e.graph_config();
    for (weight, label) in [(0.0, "pure vectors"), (1.0, "pure keywords")] {
        cfg.policy.keyword_weight = weight;
        e.set_graph_config(&cfg).unwrap();
        // Both extremes must remain valid configurations that still answer an
        // exact-title query; the balance is a preference, not a switch that
        // can break search.
        let hits = e.search("alpha beta gamma", &[], 5).unwrap();
        assert!(
            hits.is_empty() || hits[0].score > 0.0,
            "{label} produced a broken score"
        );
    }
    // Out-of-range values are refused rather than silently clamped.
    cfg.policy.keyword_weight = 1.5;
    assert!(e.set_graph_config(&cfg).is_err());
}

// ------------------------------------------------------------------ auto-tune

/// A graph big enough to clear the note gate, with enough real nodes to hang
/// one suspect row per pair (the store enforces one suspect per pair).
/// Titles get distinct vocabularies so the fake embedder's dupe pre-check
/// never collapses them.
pub(super) fn auto_tune_fixture(nodes: usize) -> (Engine, Vec<String>) {
    let e = engine();
    let mut ids = Vec::new();
    for i in 0..nodes {
        let n = e
            .add_node(new_node(
                NodeType::Insight,
                &format!("subject-{i} behaves differently under load {}", i * 7 + 3),
                &format!("distinct body {i}"),
            ))
            .unwrap();
        ids.push(n.id);
    }
    (e, ids)
}

/// Judge the `k`-th fresh pair from `ids` at the given similarity.
pub(super) fn judge(e: &Engine, ids: &[String], k: usize, sim: f64, status: SuspectStatus) {
    let s = e
        .store()
        .add_suspect(&ids[2 * k], &ids[2 * k + 1], sim, None)
        .unwrap();
    e.store().set_suspect_status(&s.id, status).unwrap();
}

#[test]
fn auto_tune_fits_the_floor_from_judged_history_and_settles() {
    let (e, ids) = auto_tune_fixture(crate::policy::AUTO_TUNE_MIN_NOTES as usize + 5);
    // The dogfood story, synthesized: dismissals cluster low, confirmations
    // sit clearly above, with a clean gap between 0.895 and 0.92.
    for i in 0..20 {
        judge(
            &e,
            &ids,
            i,
            0.85 + (i as f64) * 0.0023,
            SuspectStatus::Dismissed,
        );
    }
    for i in 0..8 {
        judge(
            &e,
            &ids,
            20 + i,
            0.92 + (i as f64) * 0.005,
            SuspectStatus::Confirmed,
        );
    }
    let note = e
        .auto_tune()
        .unwrap()
        .expect("gates cleared, floor must move");
    assert!(
        note.contains("judged pairs"),
        "the note explains itself: {note}"
    );
    // Damped moves: the first pass travels only half the distance to the
    // fit, so it must land BELOW the gap the fit sits in...
    let first = e.graph_config().policy.conflict_suspect_similarity;
    assert!(
        first < 0.895,
        "the first damped step must not teleport into the gap: {first}"
    );
    // ...and repeated passes on the same history converge into it, then
    // settle -- no config churn.
    let mut passes = 1;
    while e.auto_tune().unwrap().is_some() {
        passes += 1;
        assert!(passes < 12, "damped fits must converge");
    }
    let fitted = e.graph_config().policy.conflict_suspect_similarity;
    assert!(
        (0.89..=0.92).contains(&fitted),
        "the converged floor belongs at the gap between the classes: {fitted}"
    );
    assert!(e.auto_tune().unwrap().is_none(), "converged means settled");
}

#[test]
fn auto_tune_is_silent_below_its_gates_and_when_opted_out() {
    // Judgment-rich but note-poor: the size gate holds.
    let (e, ids) = auto_tune_fixture(64);
    for i in 0..30 {
        judge(
            &e,
            &ids,
            i,
            0.86 + (i as f64) * 0.003,
            if i % 3 == 0 {
                SuspectStatus::Confirmed
            } else {
                SuspectStatus::Dismissed
            },
        );
    }
    assert!(e.auto_tune().unwrap().is_none(), "200-note gate must hold");

    // Big graph, judgments one-sided: a floor can be pushed, not placed.
    let (e, ids) = auto_tune_fixture(crate::policy::AUTO_TUNE_MIN_NOTES as usize + 70);
    for i in 0..25 {
        judge(
            &e,
            &ids,
            i,
            0.85 + (i as f64) * 0.003,
            SuspectStatus::Dismissed,
        );
    }
    assert!(
        e.auto_tune().unwrap().is_none(),
        "one-sided history must not fit"
    );

    // Fully qualified but opted out.
    for i in 0..8 {
        judge(
            &e,
            &ids,
            25 + i,
            0.93 + (i as f64) * 0.004,
            SuspectStatus::Confirmed,
        );
    }
    let mut cfg = e.graph_config();
    cfg.policy.auto_tune = false;
    e.set_graph_config(&cfg).unwrap();
    assert!(
        e.auto_tune().unwrap().is_none(),
        "auto_tune=false is a hard opt-out"
    );
}

// ---------------------------------------------------- calibrated delivery

/// A cross-encoder with a script instead of a model: logit i goes to doc i
/// in retrieval order.
pub(super) struct ScriptedReranker(Vec<f32>);
impl crate::Reranker for ScriptedReranker {
    fn rank(&self, _query: &str, documents: &[String]) -> Result<Vec<f32>> {
        Ok((0..documents.len())
            .map(|i| self.0.get(i).copied().unwrap_or(-10.0))
            .collect())
    }
}

pub(super) fn engine_with_reranker(logits: Vec<f32>) -> Engine {
    let mut e = Engine::new(store(), Box::new(FakeEmbedder::default()));
    e.set_reranker(Box::new(ScriptedReranker(logits)));
    e
}

#[test]
fn calibrated_delivery_trims_the_tail_and_issues_verdicts() {
    // Confident first doc, hopeless second: sigmoid(-8) ~ 0.0003 sits far
    // under the delivery floor and must not be delivered, however the vote
    // ordered it. The survivor clears the weak-evidence line -> strong.
    let e = engine_with_reranker(vec![4.0, -8.0]);
    e.add_node(new_node(
        NodeType::Insight,
        "alpha component retry budget is seven attempts",
        "the retry budget",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Insight,
        "alpha component retry budget backup copy of the same claim",
        "the retry budget again",
    ))
    .unwrap();
    let hits = e
        .search("alpha component retry budget is seven attempts", &[], 10)
        .unwrap();
    assert_eq!(hits.len(), 1, "the tail hit must be trimmed, not delivered");
    assert_eq!(e.search_confidence(&hits), Some("strong"));

    // Nothing clears the line: everything hovers at sigmoid(0) ~ 0.5 —
    // kept (above the trim floor), but the verdict says verify first.
    let e = engine_with_reranker(vec![0.0, 0.0]);
    e.add_node(new_node(
        NodeType::Insight,
        "beta component lease window is nine seconds",
        "the lease window",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Insight,
        "beta component lease window recorded twice for the test",
        "the lease window again",
    ))
    .unwrap();
    let hits = e
        .search("beta component lease window is nine seconds", &[], 10)
        .unwrap();
    assert_eq!(
        hits.len(),
        2,
        "mid-confidence hits are delivered, only labeled"
    );
    assert_eq!(e.search_confidence(&hits), Some("weak"));

    // An empty graph is silent on any scale.
    let e = engine_with_reranker(vec![]);
    let hits = e.search("anything at all", &[], 10).unwrap();
    assert_eq!(e.search_confidence(&hits), Some("none"));
}

#[test]
fn without_a_reranker_scores_are_uncalibrated_and_carry_no_verdict() {
    let e = engine();
    e.add_node(new_node(
        NodeType::Insight,
        "gamma component spill threshold is three megabytes",
        "the spill threshold",
    ))
    .unwrap();
    let hits = e
        .search(
            "gamma component spill threshold is three megabytes",
            &[],
            10,
        )
        .unwrap();
    assert!(!hits.is_empty(), "hybrid search still delivers");
    assert_eq!(
        e.search_confidence(&hits),
        None,
        "a verdict from an uncalibrated scale would be noise wearing a badge"
    );
}

#[test]
fn knee_trim_cuts_the_cliff_tail_but_spares_flat_curves() {
    // Two confident docs, one mid-scoring straggler above the fixed floor:
    // the ~36% relative drop is a cliff, so the straggler is tail and must
    // stay home even though the 0.22 floor would have delivered it.
    let e = engine_with_reranker(vec![4.0, 3.8, 0.5]);
    for (t, b) in [
        ("delta component flush interval is four seconds", "flush"),
        (
            "delta component flush interval restated for the test",
            "again",
        ),
        (
            "delta component flush interval noted a third time",
            "thrice",
        ),
    ] {
        e.add_node(new_node(NodeType::Insight, t, b)).unwrap();
    }
    let hits = e
        .search("delta component flush interval is four seconds", &[], 10)
        .unwrap();
    assert_eq!(hits.len(), 2, "the hit past the score cliff is tail");

    // A flat curve has no knee: nothing beyond the fixed floor is trimmed.
    let e = engine_with_reranker(vec![0.5, 0.4, 0.3]);
    for (t, b) in [
        ("epsilon component drain window is six seconds", "drain"),
        (
            "epsilon component drain window restated for the test",
            "again",
        ),
        (
            "epsilon component drain window noted a third time",
            "thrice",
        ),
    ] {
        e.add_node(new_node(NodeType::Insight, t, b)).unwrap();
    }
    let hits = e
        .search("epsilon component drain window is six seconds", &[], 10)
        .unwrap();
    assert_eq!(hits.len(), 3, "a flat curve must not be amputated");
}

#[test]
fn knee_trim_is_an_opt_out_via_config() {
    let e = engine_with_reranker(vec![4.0, 3.8, 0.5]);
    let mut cfg = e.graph_config();
    cfg.policy.knee_cliff = None;
    e.set_graph_config(&cfg).unwrap();
    for (t, b) in [
        ("zeta component grace period is two seconds", "grace"),
        ("zeta component grace period restated for the test", "again"),
        ("zeta component grace period noted a third time", "thrice"),
    ] {
        e.add_node(new_node(NodeType::Insight, t, b)).unwrap();
    }
    let hits = e
        .search("zeta component grace period is two seconds", &[], 10)
        .unwrap();
    assert_eq!(hits.len(), 3, "knee_cliff = null disables the knee trim");
}

/// A zero cliff is not "off" — every relative drop clears it, so it would
/// trim every curve at its largest drop. The config refuses it and names
/// the real off switch (found by the ForgetEval adapter, 2026-09-12).
#[test]
fn knee_cliff_zero_is_refused_and_taught() {
    let e = engine_with_reranker(vec![4.0, 3.8, 0.5]);
    let mut cfg = e.graph_config();
    cfg.policy.knee_cliff = Some(0.0);
    let err = e.set_graph_config(&cfg).unwrap_err().to_string();
    assert!(err.contains("knee_cliff 0"), "{err}");
    assert!(err.contains("null"), "teaches the off switch: {err}");
    assert_eq!(
        e.graph_config().policy.knee_cliff,
        Some(crate::policy::KNEE_MIN_CLIFF),
        "a refused config leaves the stored one untouched"
    );
}

#[test]
fn auto_tune_calibrates_the_weak_line_from_phantom_probes_and_settles() {
    let (mut e, _) = auto_tune_fixture(crate::policy::WEAK_LINE_MIN_NOTES as usize + 10);
    e.set_reranker(Box::new(ScriptedReranker(vec![2.0; 12])));
    let note = e
        .auto_tune()
        .unwrap()
        .expect("past the note gate the weak line must be fitted");
    assert!(note.contains("weak line"), "journal names the dial: {note}");
    let first = e.graph_config().policy.weak_evidence_top;
    let lower = e.graph_config().policy.delivery_floor + crate::policy::WEAK_LINE_ABOVE_FLOOR;
    assert!(
        (lower..=crate::policy::WEAK_LINE_MAX).contains(&first),
        "damped line {first} escaped the clamp band [{lower}, {}]",
        crate::policy::WEAK_LINE_MAX
    );
    assert!(
        (first - crate::policy::WEAK_EVIDENCE_TOP).abs() >= crate::policy::AUTO_TUNE_MIN_DELTA,
        "the journaled move must be a real move"
    );
    // Deterministic probes on an unchanged graph: damped passes converge
    // monotonically and settle.
    let mut passes = 1;
    while e.auto_tune().unwrap().is_some() {
        passes += 1;
        assert!(passes < 12, "damped fits must converge");
    }
    let converged = e.graph_config().policy.weak_evidence_top;
    assert!(
        (lower..=crate::policy::WEAK_LINE_MAX).contains(&converged),
        "converged line {converged} escaped the clamp band"
    );
    assert!(
        (converged - crate::policy::WEAK_EVIDENCE_TOP).abs()
            >= (first - crate::policy::WEAK_EVIDENCE_TOP).abs(),
        "convergence continues in the first step's direction: first {first}, converged {converged}"
    );
    assert!(e.auto_tune().unwrap().is_none(), "converged means settled");
}

/// Scores a query by its own text — so a fitted line depends on WHICH
/// probes were asked, the thing the sampling order decides.
pub(super) struct TextReranker;
impl crate::Reranker for TextReranker {
    fn rank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        let h = query
            .bytes()
            .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
        let logit = (h % 97) as f32 / 97.0 * 6.0 - 3.0;
        Ok(vec![logit; documents.len()])
    }
}

#[test]
fn the_weak_line_fit_depends_on_content_not_on_minted_ids() {
    // The same corpus written in two orders mints ids in two orders — the
    // sample must not notice (eval Finding 00dvpn0t6gx6: the same seed
    // fitted 0.898 in one run and 0.902 in the next).
    let n = crate::policy::WEAK_LINE_MIN_NOTES as usize + 30;
    let fit = |order: &mut dyn Iterator<Item = usize>| {
        let mut e = engine();
        for i in order {
            e.add_node(new_node(
                NodeType::Insight,
                &format!("subject-{i} behaves differently under load {}", i * 7 + 3),
                &format!("distinct body {i} about module {} and its cache", i % 11),
            ))
            .unwrap();
        }
        e.set_reranker(Box::new(TextReranker));
        e.auto_tune().unwrap();
        e.graph_config().policy.weak_evidence_top
    };
    let forward = fit(&mut (0..n));
    let backward = fit(&mut (0..n).rev());
    assert_eq!(forward, backward, "same content, same line");
}

/// Rewards only a document that holds most of the query's words — the way
/// the cross-encoder reads a transplant against the note it was cut from.
pub(super) struct OverlapReranker;
impl crate::Reranker for OverlapReranker {
    fn rank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        let words = |t: &str| -> std::collections::HashSet<String> {
            t.split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() >= 3)
                .map(str::to_lowercase)
                .collect()
        };
        let q = words(query);
        Ok(documents
            .iter()
            .map(|d| {
                let d = words(d);
                let shared = q.iter().filter(|w| d.contains(*w)).count();
                if shared * 10 >= q.len() * 6 {
                    6.0
                } else {
                    -6.0
                }
            })
            .collect())
    }
}

#[test]
fn a_transplant_probe_cannot_be_answered_by_its_own_source() {
    // engram Problem 00e353wiopga: on long real prose a two-word swap leaves
    // a transplant pointing straight at the note it was cut from, the probe
    // scores like a found answer, and the line climbs to its ceiling. Held
    // out, the source cannot answer — on a graph where nothing ELSE answers
    // a probe, the fitted line must come down, not go up.
    let n = crate::policy::WEAK_LINE_MIN_NOTES as usize + 20;
    let mut e = engine();
    for i in 0..n {
        let words: Vec<String> = (0..12).map(|k| format!("w{i}x{k}")).collect();
        e.add_node(new_node(
            NodeType::Insight,
            &format!("t{i}alpha t{i}beta note"),
            &format!("{} closes the window.", words.join(" ")),
        ))
        .unwrap();
    }
    e.set_reranker(Box::new(OverlapReranker));
    e.auto_tune().unwrap();
    let line = e.graph_config().policy.weak_evidence_top;
    assert!(
        line < crate::policy::WEAK_EVIDENCE_TOP,
        "no probe is answerable here, so the line must move down: {line}"
    );
}

#[test]
fn weak_line_dial_is_silent_without_a_reranker_or_below_the_note_gate() {
    // Reranker present but the graph is too small to place a quantile on.
    let (mut e, _) = auto_tune_fixture(10);
    e.set_reranker(Box::new(ScriptedReranker(vec![2.0; 12])));
    assert!(
        e.auto_tune().unwrap().is_none(),
        "50-note gate must hold for the weak line"
    );
    // Big enough, but no reranker: the line reads the cross-encoder's scale,
    // so there is nothing to fit.
    let (e, _) = auto_tune_fixture(crate::policy::WEAK_LINE_MIN_NOTES as usize + 10);
    assert!(
        e.auto_tune().unwrap().is_none(),
        "no reranker means no weak-line fit"
    );
}
