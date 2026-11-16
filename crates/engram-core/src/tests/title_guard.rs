//! The title NLI path and its subject and overlap guards.

use super::*;

pub(super) fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut d, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        d += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    d / (na.sqrt() * nb.sqrt())
}

/// Two bodies that pull the composed texts into the NLI band under the
/// fake char-bag embedder: below the 0.88 look-alike floor (so the
/// similarity path cannot raise the pair and the duplicate check cannot
/// swallow it), above the 0.50 NLI floor (so the pair is still a
/// candidate). Found by construction rather than hard-coded, so the test
/// says what it needs instead of what a particular embedder happens to do.
pub(super) fn bodies_in_band(title_a: &str, title_b: &str) -> (String, String) {
    let emb = FakeEmbedder::default();
    for n in (1usize..=64).chain((80..=400).step_by(16)) {
        let a: String = "aaaa ".repeat(n);
        let b: String = "zzzz ".repeat(n);
        let va = emb.embed_one(&format!("{title_a}\n{a}")).unwrap();
        let vb = emb.embed_one(&format!("{title_b}\n{b}")).unwrap();
        let c = cosine(&va, &vb);
        if (0.52..=0.87).contains(&c) {
            return (a, b);
        }
    }
    panic!("no body length lands the pair in the NLI band under the fake embedder");
}

pub(super) const TITLE_A: &str = "Kelnor lease broker contra uses a retry budget of 7";
pub(super) const TITLE_B: &str =
    "Kelnor lease broker contra neg is configured with a retry budget of 19";
pub(super) const TITLE_OTHER: &str =
    "Vantor lease broker contra neg is configured with a retry budget of 19";

pub(super) fn write_pair(e: &Engine, title_a: &str, title_b: &str) -> (Node, WriteOutcome) {
    let (body_a, body_b) = bodies_in_band(title_a, title_b);
    let a = e
        .add_node(new_node(NodeType::Decision, title_a, &body_a))
        .unwrap();
    let out = e
        .add_node_checked(new_node(NodeType::Decision, title_b, &body_b))
        .unwrap();
    (a, out)
}

#[test]
fn a_title_contradiction_queues_below_the_similarity_floor() {
    let e = engine_with_nli();
    let (a, out) = write_pair(&e, TITLE_A, TITLE_B);
    let WriteOutcome::Created { suspects, .. } = out else {
        panic!("distinct bodies must not match as a duplicate")
    };
    let s = suspects
        .iter()
        .find(|s| s.a.id == a.id || s.b.id == a.id)
        .expect("the title contradiction was nominated");
    assert!(
        s.similarity < e.graph_config().policy.conflict_suspect_similarity,
        "similarity {} is above the floor — the similarity path raised it, not the NLI path",
        s.similarity
    );
    assert!(s.similarity >= crate::policy::CONFLICT_NLI_FLOOR);
    assert_eq!(s.nli_label.as_deref(), Some("contradiction"));
    assert_eq!(s.nli_direction.as_deref(), Some("newer"));
}

#[test]
fn the_title_path_is_off_when_the_gate_is_null() {
    let e = engine_with_nli();
    let mut cfg = e.graph_config();
    cfg.policy.conflict_nli_gate = None;
    e.set_graph_config(&cfg).unwrap();
    let (_, out) = write_pair(&e, TITLE_A, TITLE_B);
    let WriteOutcome::Created { suspects, .. } = out else {
        panic!("distinct bodies must not match as a duplicate")
    };
    assert!(
        suspects.is_empty(),
        "with the gate off only look-alikes queue"
    );
}

#[test]
fn the_title_path_refuses_pairs_about_different_subjects() {
    let e = engine_with_nli();
    let (_, out) = write_pair(&e, TITLE_A, TITLE_OTHER);
    let WriteOutcome::Created { suspects, .. } = out else {
        panic!("distinct bodies must not match as a duplicate")
    };
    assert!(
        suspects.is_empty(),
        "two facts about two subjects are not a contradiction (RefNLI)"
    );
}

#[test]
fn the_subject_guard_reads_names_not_sentence_starters() {
    let same = |a: &str, b: &str| crate::engine::shared_subject(a, b).is_some();
    assert!(same(
        "It is not the case that the Kelnor broker loses its cursor",
        "Kelnor broker loses its cursor when the lease expires"
    ));
    assert!(!same(
        "Vanor lease broker uses a retry budget of 7",
        "Kelnor lease broker uses a retry budget of 19"
    ));
    // Nothing shared at all: no NLI nomination.
    assert!(!same("never store secrets", "always rotate keys"));
    assert!(!same(
        "Engram Alpha is written in Rust",
        "the daemon never loads a language model"
    ));
    // Release notes: the lenient form raised fifty-six of these on the
    // dogfood graph. Either the guard or the overlap count must refuse them.
    assert!(!crate::engine::title_pair_admissible(
        "v0.9.1 released 2026-09-02 — SelectMenu and screenshots",
        "v0.9.0 released 2026-08-30 — tombstones and custom fields"
    ));
    // Names on both sides, none shared: the RefNLI false contradiction.
    assert!(!same(
        "Engram Alpha is written in Rust",
        "TepinDB is published on crates.io"
    ));
}

#[test]
fn the_unnamed_subject_guard_reads_lowercase_subjects() {
    use crate::engine::title_pair_admissible as ok;
    // KnowledgeDrift v2's register: shared-vocabulary lowercase subjects.
    assert!(ok(
        "hazel estuary sweep worker uses a drain deadline of 3 seconds — surfaced on the third occurrence",
        "hazel estuary sweep worker is configured with 21 seconds"
    ));
    assert!(ok(
        "It is not the case that the onyx cliff replay buffer mislabels its vials.",
        "onyx cliff replay buffer mislabels its vials when a poison message recirculates"
    ));
    assert!(ok(
        "Operators pinned the chalk glade telemetry mast to 143 attempts for its retry budget",
        "chalk glade telemetry mast uses a retry budget of 120 attempts — corroborated independently"
    ));
    assert!(ok(
        "carmine ravine sweep worker is permitted to outrun the replication horizon",
        "carmine ravine sweep worker must never outrun the replication horizon, and no surrounding layer makes up for it"
    ));
    // Co-reference: the component is shared, the modifiers in front of it
    // name two different things.
    assert!(!ok(
        "wheat dale assay station never overruns its dwell, whatever happens",
        "hazel bayou assay station overruns its dwell when the daylight saving shift lands"
    ));
    assert!(!ok(
        "walnut geyser provenance ledger no longer shadows its primary",
        "hazel lagoon provenance ledger shadows its primary during certificate rotation"
    ));
    // Collider: same subject, an unrelated claim — refused by the overlap.
    assert!(!ok(
        "violet pond lease broker expires its own grants",
        "violet pond lease broker uses a batch size of 81 records — reproduced in staging"
    ));
    // A history note whose subject sits behind a qualifying content word.
    assert!(!ok(
        "Until the 3.1 rollout, navy forest lease broker never corrupts its checkpoint, whatever happens",
        "navy forest lease broker corrupts its checkpoint when the mains power browns out"
    ));
    // Two release notes sharing a tail phrase: the run must sit in the
    // first clause of both (dogfood replay, 2026-09-16).
    assert!(!ok(
        "v0.9.3 shipped 2026-09-09 (93bd8a7): the pane fits any width, README rewrite — Release green with nine assets after one draft-release rerun",
        "v0.8.13 released 2026-08-29 at 55f3387 — identity-checked engine cache + honest stop (issue #8); all nine assets"
    ));
    // A run of function words is not a subject.
    assert!(!ok(
        "in the end the pane is the transparency layer",
        "in the end the daemon never loads a language model"
    ));
}

#[test]
fn the_duplicate_match_refuses_titles_that_name_different_things() {
    use crate::engine::subjects_differ;
    assert!(subjects_differ(
        "hazel quarry edge cache forgets its offsets when the almanac rolls over",
        "wheat geyser edge cache corrupts its checkpoint when the coolant loop cavitates"
    ));
    assert!(subjects_differ(
        "Vanor lease broker uses a retry budget of 7",
        "Kelnor lease broker uses a retry budget of 19"
    ));
    // The same subject, restated: still the duplicate it looks like.
    assert!(!subjects_differ(
        "hazel quarry edge cache forgets its offsets when the almanac rolls over",
        "The hazel quarry edge cache loses its offsets at the almanac rollover"
    ));
    // Undecidable — no names, no shared run: the vectors decide.
    assert!(!subjects_differ(
        "cache results in redis",
        "results are cached in redis for speed"
    ));
    assert!(!subjects_differ(
        "never store secrets",
        "always rotate keys"
    ));
}

#[test]
fn the_overlap_guard_separates_a_restated_claim_from_an_unrelated_fact() {
    let overlap = |a: &str, b: &str| {
        let subject = crate::engine::shared_subject(a, b).unwrap_or_default();
        crate::engine::content_overlap(a, b, &subject)
    };
    // A restated claim shares its parameter words.
    assert!(
        overlap(
            "Kelnor lease broker uses a retry budget of 7 attempts",
            "Kelnor lease broker is configured with a retry budget of 19 attempts"
        ) >= crate::policy::CONFLICT_NLI_OVERLAP
    );
    // Two facts about one subject share the subject and nothing else.
    assert!(
        overlap(
            "JetBrains plugin registers no file types; discovery via description text",
            "JetBrains plugin ships dual artifacts per release, selected by platform line"
        ) < crate::policy::CONFLICT_NLI_OVERLAP
    );
}

#[test]
fn the_title_path_refuses_an_unrelated_claim_about_the_same_subject() {
    let e = engine_with_nli();
    // FakeNli calls any "contra"+"contra" pair a contradiction, so only the
    // overlap guard stands between this pair and the queue.
    // The fake trigger word rides in the subject phrase, so the only words
    // left after it are the two unrelated claims.
    let (_, out) = write_pair(
        &e,
        "Kelnor contra broker uses a retry budget of 7",
        "Kelnor contra broker neg drops packets under load",
    );
    let WriteOutcome::Created { suspects, .. } = out else {
        panic!("distinct bodies must not match as a duplicate")
    };
    assert!(
        suspects.is_empty(),
        "an unrelated claim about the same subject is not a contradiction"
    );
}

#[test]
fn a_gate_outside_the_unit_interval_is_refused() {
    let e = engine();
    let mut cfg = e.graph_config();
    cfg.policy.conflict_nli_gate = Some(1.5);
    assert!(e.set_graph_config(&cfg).is_err());
    cfg.policy.conflict_nli_gate = Some(0.0);
    assert!(e.set_graph_config(&cfg).is_ok());
}
