//! The suite: build a corpus, run every arm over every question, and score.
//!
//! Three parts, matching the three things a memory layer has to get right:
//!
//! 1. **recall** — the fact was written; can it be found again, and by a
//!    question that does not quote it?
//! 2. **logic** — when two statements conflict, is that visible? (`nli`)
//! 3. **restraint** — when nothing was ever written, does the retriever say
//!    so, or does it confidently return the nearest-looking thing?
//!
//! Part 3 is the one nobody publishes and the one that decides whether a
//! memory layer is safe to put in front of an agent.

use std::collections::HashMap;

use serde::Serialize;

use engram_core::{Embedder, Engine, FakeEmbedder, FakeNli, Nli};

use crate::arms::{
    Arm, ChanceArm, CuratedFileArm, Delivery, EngramArm, GrepArm, RagArm, WholeFileArm,
};
use crate::generate::{Corpus, DEFAULT_TYPE_MIX, Kind, Phrasing, corpus_full};
use crate::metrics::{Outcome, PhrasingMix, Score, Separation, by_phrasing, score, separation};
use crate::nli_eval::{NliReport, evaluate};
use crate::profile::Profile;

/// A believable hand-maintained memory file: a few thousand tokens, which is
/// what stays readable and roughly what this repo's own CLAUDE.md costs.
pub const DEFAULT_CURATED_BUDGET: usize = 3000;

#[derive(Clone)]
pub struct Config {
    /// Tested facts per run — the ones questions are asked about.
    pub sizes: Vec<usize>,
    /// Distractors written per tested fact. These are never asked about; they
    /// exist so recall is measured against a crowded graph rather than one
    /// where every stored fact is also somebody's right answer.
    pub distractor_ratio: usize,
    /// Per-kind question weighting — how often each type gets asked about.
    /// An assumption, printed in every report so no number travels without it.
    pub type_mix: Vec<(Kind, u32)>,
    /// The structural shape generated facts are filled to.
    pub profile: Profile,
    pub seed: u64,
    /// How many results an arm may return — the same budget for all of them.
    pub limit: usize,
    pub nli_budget: usize,
    /// Drop the cross-encoder from the engram arm. Not a product option — a
    /// diagnostic, for asking whether the precision layer is costing recall on
    /// queries it handles badly.
    pub no_rerank: bool,
    /// Zero every type's ranking prior. A diagnostic, like `no_rerank`.
    pub flat_priors: bool,
    /// How often each phrasing is assumed to occur. Decides the headline
    /// number, and therefore which arm wins.
    pub phrasing: PhrasingMix,
    /// Embedding model by name, from `EMBED_CHOICES`. `None` = whatever
    /// `FastEmbedder::new` loads, which is what the product loads.
    pub embed_model: Option<String>,
    /// Token budgets for the hand-maintained-file baseline — one arm per
    /// budget. Defaults to a realistic `CLAUDE.md`; add a second budget to
    /// bracket the crossover in a single run (the ladder scores 3k and 30k).
    pub curated_budgets: Vec<usize>,
    /// Rerank on `title + full body` instead of the keyword-window snippet
    /// (`policy.rerank_full_note`). A candidate, not the default: notes fit
    /// the cross-encoder window whole, and the snippet starves it on oblique
    /// queries whose evidence sentence shares no keyword with the query.
    pub rerank_full: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sizes: vec![50, 200],
            distractor_ratio: 2,
            type_mix: DEFAULT_TYPE_MIX.to_vec(),
            profile: Profile::default(),
            seed: 1,
            limit: 10,
            nli_budget: 300,
            no_rerank: false,
            flat_priors: false,
            phrasing: PhrasingMix::default(),
            embed_model: None,
            curated_budgets: vec![DEFAULT_CURATED_BUDGET],
            rerank_full: false,
        }
    }
}

/// One point on the tuning grid.
/// How hard search prunes its own results before returning them.
///
/// This axis exists because the comparison was unfair without it: the `rag`
/// arm returns a flat top-k, while engram drops anything below
/// `search_min_score` or below `search_relative_cut` of the top hit. Recall was
/// being measured against a stack wearing quality gates its competitor did not
/// wear — and the gates tighten as a corpus grows, because more competitors
/// raise the top score that the relative cut is a fraction of.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CutTier {
    /// No pruning at all — a flat top-k, exactly what `rag` does.
    None,
    /// What ships today.
    Default,
    Aggressive,
}

impl CutTier {
    fn values(self) -> (f64, f64) {
        match self {
            CutTier::None => (0.0, 0.0),
            CutTier::Default => (
                engram_core::policy::SEARCH_MIN_SCORE,
                engram_core::policy::SEARCH_RELATIVE_CUT,
            ),
            CutTier::Aggressive => (0.2, 0.5),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SweepPoint {
    pub keyword_weight: f64,
    /// Cosine below which a vector match contributes nothing.
    ///
    /// This is an axis rather than a constant because of what the floor does
    /// when the keyword channel is also silent: `search_hybrid` skips any
    /// candidate whose relevance computes to zero, so a floored vector hit with
    /// no FTS match is not demoted, it is *deleted* — before ranking, before
    /// the cut, before the reranker ever sees it. A query that never names its
    /// subject is exactly the case that produces both conditions at once.
    pub semantic_floor: f64,
    pub cut: CutTier,
    /// Recall@5 as the assumed workload would experience it.
    pub weighted_recall: f64,
    pub lexical: f64,
    pub paraphrase: f64,
    pub oblique: f64,
    pub tokens_mean: f64,
    /// True for the values the product ships today.
    pub is_default: bool,
}

/// What the pure-vector arm scores on the same corpus — the number every
/// sweep point is trying to beat, printed beside the grid so a point can
/// never look good in isolation.
#[derive(Debug, Clone, Serialize)]
pub struct SweepReference {
    pub lexical: f64,
    pub paraphrase: f64,
    pub oblique: f64,
    pub weighted_recall: f64,
    pub tokens_mean: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SweepReport {
    pub points: Vec<SweepPoint>,
    pub rag: SweepReference,
}

/// Sweep the fusion balance against the semantic floor over one corpus.
///
/// Reports every point rather than just the winner: raising the semantic share
/// buys oblique recall and spends precision on named queries, and which side of
/// that trade to sit on is a decision about the workload, not an optimum the
/// harness can find on its own.
///
/// The cut tier is held at `Default` rather than swept. It had its turn as an
/// axis and produced recall identical to the shipped values at every keyword
/// weight — a refuted lever costs a third of the grid to re-refute, and the
/// floor has never been measured at all.
pub fn sweep(cfg: &Config) -> anyhow::Result<SweepReport> {
    let model = cfg.embed_model.as_deref();
    let size = *cfg.sizes.first().expect("at least one size");
    let c = corpus_full(
        size,
        size * cfg.distractor_ratio,
        cfg.seed,
        &cfg.profile,
        &cfg.type_mix,
    );
    let twins = twin_map(&c);
    let rerank = if cfg.no_rerank { None } else { reranker().0 };
    let engram = EngramArm::build(&c, embedder(model).0, rerank)?;
    let defaults = engram.engine().graph_config().policy;

    let at5 = |split: &[(Phrasing, Score)], p: Phrasing| {
        split
            .iter()
            .find(|(q, _)| *q == p)
            .map(|(_, s)| s.recall_at_5)
            .unwrap_or_default()
    };

    // Measured once, from the same built store, so the grid is read against
    // the thing it has to beat rather than against its own best row.
    let rag = RagArm::new(&engram, embedder(model).0);
    let (rag_outcomes, _) = measure(&rag, &c, cfg.limit, &twins);
    let rag_split = by_phrasing(&rag_outcomes);
    let reference = SweepReference {
        lexical: at5(&rag_split, Phrasing::Lexical),
        paraphrase: at5(&rag_split, Phrasing::Paraphrase),
        oblique: at5(&rag_split, Phrasing::Oblique),
        weighted_recall: cfg.phrasing.weighted_recall(&rag_split),
        tokens_mean: score(&rag_outcomes).tokens_mean,
    };

    // Twelve points, not thirty-five. Every point re-runs every question
    // through the cross-encoder, so the grid is the whole cost of a sweep:
    // 35 points over 343 questions was ~360k reranker calls and had produced
    // nothing after forty minutes. Coarse and finishable beats fine and
    // abandoned — refine around the winner afterwards if it matters.
    let mut out = Vec::new();
    let floors = [0.0, 0.2, 0.4, 0.6];
    let weights = [0.0, 0.25, 0.5];
    let total = floors.len() * weights.len();
    for floor in floors {
        for kw in weights {
            eprintln!(
                "  sweeping {}/{total}: keyword_weight={kw} semantic_floor={floor}",
                out.len() + 1
            );
            // The cut is re-set every point rather than left alone, so each
            // row states its whole configuration instead of inheriting one.
            let (min_score, relative_cut) = CutTier::Default.values();
            engram.tune(|p| {
                p.keyword_weight = kw;
                p.semantic_floor = floor;
                p.search_min_score = min_score;
                p.search_relative_cut = relative_cut;
            })?;
            let (outcomes, _) = measure(&engram, &c, cfg.limit, &twins);
            let split = by_phrasing(&outcomes);
            out.push(SweepPoint {
                keyword_weight: kw,
                semantic_floor: floor,
                cut: CutTier::Default,
                weighted_recall: cfg.phrasing.weighted_recall(&split),
                lexical: at5(&split, Phrasing::Lexical),
                paraphrase: at5(&split, Phrasing::Paraphrase),
                oblique: at5(&split, Phrasing::Oblique),
                tokens_mean: score(&outcomes).tokens_mean,
                is_default: (kw - defaults.keyword_weight).abs() < 1e-9
                    && (floor - defaults.semantic_floor).abs() < 1e-9,
            });
        }
    }
    out.sort_by(|a, b| b.weighted_recall.total_cmp(&a.weighted_recall));
    Ok(SweepReport {
        points: out,
        rag: reference,
    })
}

// ----------------------------------------------------------- contradictions

/// How the contradiction layer performs as a *pipeline*, not as a model.
///
/// `nli_eval` scores the NLI model on isolated sentence pairs. That is the
/// wrong unit for a product decision: in `check_claim` the model only ever
/// judges what retrieval handed it, so a claim whose target was never
/// retrieved is a miss the model never had a chance at. Reporting those two
/// failures separately is the whole point — one is fixed by better retrieval,
/// the other by a better model, and only the second is an argument for
/// swapping the default.
#[derive(Debug, Clone, Serialize)]
pub struct ContradictionReport {
    pub model: String,
    /// Restatements that contradict a stored note.
    pub contradictions: usize,
    /// ...where the contradicted note came back in the `contradicts` bucket.
    pub caught: usize,
    /// ...where retrieval never surfaced the note, so NLI never saw it.
    pub missed_by_retrieval: usize,
    /// ...where the note WAS retrieved and the model did not call it a
    /// contradiction. This is the number a better model would move.
    pub missed_by_judgment: usize,
    /// Restatements that agree with a stored note, and how many landed in
    /// `supports`.
    pub entailments: usize,
    pub supported: usize,
    /// ...where retrieval never surfaced the note it restates.
    pub agree_missed_by_retrieval: usize,
    /// ...where the note WAS retrieved and the model declined to call it
    /// entailment. Splits the unconfirmed remainder the same way the
    /// contradiction rows do, so "80% confirmed" can be read as a model
    /// property rather than a pipeline one.
    pub agree_judged_neutral: usize,
    /// ...where the model called an agreeing restatement a CONTRADICTION.
    /// The only actively harmful cell in this table: it does not merely fail
    /// to confirm, it asserts the opposite.
    pub agree_judged_contradiction: usize,
    /// Statements about unrelated subjects, and how many wrongly produced a
    /// contradiction against something. The poisoning metric: a layer that
    /// flags everything is worse than one that flags nothing, because every
    /// false alarm costs a human judgment.
    pub neutrals: usize,
    pub false_alarms: usize,
    /// What a confidence gate would do, if one were applied.
    ///
    /// `check_claim` buckets on the raw NLI label today — no floor, no gate —
    /// while its sibling `audit_conflicts` holds a similarity floor precisely
    /// because MNLI-class models presuppose co-reference and call unrelated
    /// same-shaped titles confident contradictions. Whether that guard is
    /// missing here or merely unnecessary is decided by whether the two
    /// populations separate at all, so the sweep is reported rather than
    /// assumed either way.
    pub gates: Vec<GatePoint>,
    /// The floor the product ships, so the sweep can mark which row is live.
    pub shipped_gate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatePoint {
    /// Minimum contradiction confidence required to report a contradiction.
    pub min_confidence: f64,
    pub catch_rate: f64,
    pub false_alarm_rate: f64,
    /// Agreeing restatements reported as contradictions, at this gate. Swept
    /// alongside the unrelated-claim rate because it is the same failure with
    /// a worse consequence: the layer does not merely misfire at a stranger,
    /// it asserts conflict against the note the claim was echoing.
    pub agree_alarm_rate: f64,
}

impl ContradictionReport {
    pub fn catch_rate(&self) -> f64 {
        ratio(self.caught, self.contradictions)
    }
    pub fn false_alarm_rate(&self) -> f64 {
        ratio(self.false_alarms, self.neutrals)
    }
    /// Of the contradictions that got away, the share the model could have
    /// caught — the ceiling on what changing the NLI model can buy.
    pub fn judgment_share_of_misses(&self) -> f64 {
        ratio(
            self.missed_by_judgment,
            self.missed_by_retrieval + self.missed_by_judgment,
        )
    }
}

fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

/// Write the corpus to a real graph, then push every generated restatement
/// through `Engine::check_claim` — the exact call the MCP tool makes.
pub fn contradictions(cfg: &Config) -> anyhow::Result<ContradictionReport> {
    let model = cfg.embed_model.as_deref();
    let size = *cfg.sizes.first().expect("at least one size");
    let c = corpus_full(
        size,
        size * cfg.distractor_ratio,
        cfg.seed,
        &cfg.profile,
        &cfg.type_mix,
    );
    let (nli_model, nli_name) = nli();
    let mut engram = EngramArm::build(&c, embedder(model).0, reranker().0)?;
    engram.engine_mut().set_nli(nli_model);

    // Open the product's own gate all the way, so the sweep below can see the
    // raw distribution. Leaving `claim_contradiction_min_confidence` at its
    // shipped value would make this harness blind underneath its own default —
    // the instrument used to CHOOSE the threshold cannot be clamped by it, or
    // the threshold can never be revisited.
    let shipped_floor = engram
        .engine()
        .graph_config()
        .policy
        .claim_contradiction_min_confidence;
    engram.tune(|p| p.claim_contradiction_min_confidence = 0.0)?;

    // premise -> the fact it restates. Pairs carry the note's title verbatim,
    // which is unique by construction.
    let by_title: HashMap<&str, &str> = c
        .facts
        .iter()
        .map(|f| (f.title.as_str(), f.key.as_str()))
        .collect();

    let mut r = ContradictionReport {
        model: nli_name,
        contradictions: 0,
        caught: 0,
        missed_by_retrieval: 0,
        missed_by_judgment: 0,
        entailments: 0,
        supported: 0,
        agree_missed_by_retrieval: 0,
        agree_judged_neutral: 0,
        agree_judged_contradiction: 0,
        neutrals: 0,
        false_alarms: 0,
        gates: Vec::new(),
        shipped_gate: shipped_floor,
    };
    // Peak contradiction confidence per claim, kept so a gate can be swept
    // after the fact instead of re-running the model once per threshold.
    let mut caught_at: Vec<f64> = Vec::new();
    let mut alarmed_at: Vec<f64> = Vec::new();
    let mut agree_alarmed_at: Vec<f64> = Vec::new();

    let budget = cfg.nli_budget;
    for pair in c.pairs.iter().take(budget) {
        let Ok(report) = engram.engine().check_claim(&pair.hypothesis, 8) else {
            continue;
        };
        let target = by_title.get(pair.premise.as_str()).copied();
        let keys = engram.keys();
        let in_bucket = |bucket: &[engram_core::ClaimVerdict]| -> bool {
            match target {
                Some(t) => bucket
                    .iter()
                    .any(|v| keys.get(&v.id).map(String::as_str) == Some(t)),
                None => false,
            }
        };
        let retrieved = in_bucket(&report.supports)
            || in_bucket(&report.contradicts)
            || in_bucket(&report.silent);

        match pair.gold {
            crate::generate::NliLabel::Contradiction => {
                r.contradictions += 1;
                if in_bucket(&report.contradicts) {
                    r.caught += 1;
                    let hit = report
                        .contradicts
                        .iter()
                        .filter(|v| keys.get(&v.id).map(String::as_str) == target)
                        .map(|v| v.contradiction as f64)
                        .fold(0.0, f64::max);
                    caught_at.push(hit);
                } else if retrieved {
                    r.missed_by_judgment += 1;
                } else {
                    r.missed_by_retrieval += 1;
                }
            }
            crate::generate::NliLabel::Entailment => {
                r.entailments += 1;
                if in_bucket(&report.supports) {
                    r.supported += 1;
                } else if in_bucket(&report.contradicts) {
                    r.agree_judged_contradiction += 1;
                    agree_alarmed_at.push(
                        report
                            .contradicts
                            .iter()
                            .filter(|v| keys.get(&v.id).map(String::as_str) == target)
                            .map(|v| v.contradiction as f64)
                            .fold(0.0, f64::max),
                    );
                } else if retrieved {
                    r.agree_judged_neutral += 1;
                } else {
                    r.agree_missed_by_retrieval += 1;
                }
            }
            crate::generate::NliLabel::Neutral => {
                r.neutrals += 1;
                // A neutral restatement names a subject nothing was written
                // about, so ANY contradiction verdict is a false alarm — it
                // does not matter which node it fired against.
                if !report.contradicts.is_empty() {
                    r.false_alarms += 1;
                    alarmed_at.push(
                        report
                            .contradicts
                            .iter()
                            .map(|v| v.contradiction as f64)
                            .fold(0.0, f64::max),
                    );
                }
            }
        }
    }
    // A gate can only ever drop reports, so both rates fall together; the
    // question is whether the false-alarm rate falls faster.
    let mut points = vec![0.0, 0.5, 0.7, 0.8, 0.9, 0.95, 0.99];
    if !points
        .iter()
        .any(|p: &f64| (p - shipped_floor).abs() < 1e-9)
    {
        points.push(shipped_floor);
        points.sort_by(f64::total_cmp);
    }
    for min in points {
        r.gates.push(GatePoint {
            min_confidence: min,
            catch_rate: ratio(
                caught_at.iter().filter(|c| **c >= min).count(),
                r.contradictions,
            ),
            false_alarm_rate: ratio(alarmed_at.iter().filter(|c| **c >= min).count(), r.neutrals),
            agree_alarm_rate: ratio(
                agree_alarmed_at.iter().filter(|c| **c >= min).count(),
                r.entailments,
            ),
        });
    }
    Ok(r)
}

// ------------------------------------------------- real graph (suspect queue)

/// One judged pair from a real graph, with what the model says about it now.
#[derive(Debug, Clone, Serialize)]
pub struct JudgedPair {
    /// The human's verdict, recovered from the edge the judgment created:
    /// `conflict` | `replaces` | `dismiss`.
    pub verdict: String,
    pub similarity: f64,
    /// What the sweep's own hint call returns today.
    pub label: String,
    pub score: f64,
    pub a_title: String,
    pub b_title: String,
    /// Whether the two nodes carry an edge between them today. The sweep skips
    /// linked pairs, so a flagged pair that IS linked is noise the product
    /// would not have produced.
    pub linked: bool,
    /// The hint the row was queued with, when it carries one. Only a
    /// `contradiction` hint biases this measurement: it means the previous
    /// model picked the pair for the trait being scored. An `entailment` hint
    /// comes from the duplicate sweep, which selects on the opposite label.
    pub queued_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RealGraphReport {
    pub graph: String,
    pub model: String,
    pub nodes: i64,
    /// Every suspect row on the graph, and how many carry a human verdict.
    pub suspects: usize,
    pub pending: usize,
    /// Scored pairs: every judged suspect, plus every `conflicts-with` edge
    /// the queue never raised.
    pub judged: usize,
    /// Judged rows a previous model had already called a contradiction — the
    /// only selection bias that touches the false-alarm rate below.
    pub contradiction_hinted: usize,
    pub pairs: Vec<JudgedPair>,
    /// Sweep gate sweep: what each threshold would do to this queue.
    pub gates: Vec<RealGate>,
    pub shipped_gate: f64,
    pub similarity_floor: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RealGate {
    pub min_confidence: f64,
    /// Pairs a human dismissed that the model calls a contradiction — the
    /// queue's false-alarm rate on real project prose.
    pub dismissed_flagged: usize,
    pub dismissed: usize,
    /// The same rate over dismissed rows no previous model preselected for
    /// contradiction — the unbiased read, when the two differ.
    pub unbiased_flagged: usize,
    pub unbiased: usize,
    /// ...and dropping pairs the two nodes are already linked by, which the
    /// sweep skips before it ever calls the model. This is the queue as the
    /// product would actually run it.
    pub as_queued_flagged: usize,
    pub as_queued: usize,
    /// Confirmed conflicts the model calls a contradiction.
    pub conflicts_flagged: usize,
    pub conflicts: usize,
}

/// Score the *suspect queue's* judgment against a real graph's judged history.
///
/// The corpus is small and nobody chose it: every pair a human has ever ruled
/// on in this graph, on real project prose, already past the similarity floor
/// that guards the sweep. That makes it the right instrument for
/// `nli_sweep_min_confidence` and the wrong one for a catch rate — the
/// positive class is whatever the project happened to contradict itself about.
///
/// Reads the graph through the same `nli_hint` the sweep calls, so what is
/// measured is the product's judgment and not a reimplementation of it. Point
/// it at a COPY: it opens the store read-write, and a live daemon owns the
/// original.
pub fn real_graph(path: &str) -> anyhow::Result<RealGraphReport> {
    use engram_core::{SuspectStatus, open_store};

    let store = open_store(path)?;
    let suspects = store.all_suspects()?;
    let nodes = store.stats().map(|s| s.nodes).unwrap_or(0);
    let floor = store.config().policy.conflict_suspect_similarity;
    let shipped_gate = store.config().policy.nli_sweep_min_confidence;

    let (nli_model, model) = nli();
    // The embedder is never consulted: `nli_hint` reads titles and bodies.
    let mut engine = Engine::with_store(store, Box::new(FakeEmbedder::default()));
    engine.set_nli(nli_model);
    let store = engine.store();

    let mut pairs = Vec::new();
    let mut contradiction_hinted = 0;
    for s in &suspects {
        if s.status == SuspectStatus::Suspected {
            continue; // no verdict yet — nothing to score against
        }
        let (Some(a), Some(b)) = (store.get_node(&s.a_id)?, store.get_node(&s.b_id)?) else {
            continue;
        };
        // Confirmed pairs: recover WHICH verdict from the edge it created.
        let verdict = match s.status {
            SuspectStatus::Dismissed => "dismiss".to_string(),
            _ => {
                let edge = store
                    .edges_out(&a.id)?
                    .into_iter()
                    .chain(store.edges_in(&a.id)?)
                    .find(|e| e.from_id == b.id || e.to_id == b.id);
                match edge.as_ref().map(|e| e.edge_type.as_str()) {
                    Some("conflicts-with") => "conflict".to_string(),
                    Some("replaces") => "replaces".to_string(),
                    _ => continue, // confirmed but the edge is gone — unscorable
                }
            }
        };
        let (label, score, _) = engine
            .nli_hint(&a, &b)
            .unwrap_or(("unavailable", 0.0, None));
        if s.nli_label.as_deref() == Some("contradiction") {
            contradiction_hinted += 1;
        }
        pairs.push(JudgedPair {
            verdict,
            similarity: s.similarity,
            label: label.to_string(),
            score,
            a_title: a.title.clone(),
            b_title: b.title.clone(),
            linked: store.pair_linked(&a.id, &b.id)?,
            queued_hint: s.nli_label.clone(),
        });
    }

    // Every human-authored `conflicts-with` edge is a positive case, whether
    // or not it ever passed through the queue. Without this the corpus has no
    // positive class at all and can only ever measure the cost side.
    let queued: Vec<(&str, &str)> = suspects
        .iter()
        .map(|s| (s.a_id.as_str(), s.b_id.as_str()))
        .collect();
    for e in store.all_edges()? {
        if e.edge_type.as_str() != "conflicts-with" {
            continue;
        }
        if queued
            .iter()
            .any(|(a, b)| (*a == e.from_id && *b == e.to_id) || (*a == e.to_id && *b == e.from_id))
        {
            continue; // already scored above
        }
        let (Some(a), Some(b)) = (store.get_node(&e.from_id)?, store.get_node(&e.to_id)?) else {
            continue;
        };
        let (label, score, _) = engine
            .nli_hint(&a, &b)
            .unwrap_or(("unavailable", 0.0, None));
        pairs.push(JudgedPair {
            verdict: "conflict".to_string(),
            similarity: f64::NAN, // never queued, so no stored similarity
            label: label.to_string(),
            score,
            a_title: a.title.clone(),
            b_title: b.title.clone(),
            linked: true,
            queued_hint: None,
        });
    }

    let contra = |p: &&JudgedPair| p.label == "contradiction";
    let unbiased = |p: &&JudgedPair| p.queued_hint.as_deref() != Some("contradiction");
    let mut gates = Vec::new();
    for min in [0.0, 0.5, 0.7, 0.8, 0.9, 0.95, 0.99] {
        gates.push(RealGate {
            min_confidence: min,
            dismissed_flagged: pairs
                .iter()
                .filter(|p| p.verdict == "dismiss")
                .filter(contra)
                .filter(|p| p.score >= min)
                .count(),
            dismissed: pairs.iter().filter(|p| p.verdict == "dismiss").count(),
            unbiased_flagged: pairs
                .iter()
                .filter(|p| p.verdict == "dismiss")
                .filter(unbiased)
                .filter(contra)
                .filter(|p| p.score >= min)
                .count(),
            unbiased: pairs
                .iter()
                .filter(|p| p.verdict == "dismiss")
                .filter(unbiased)
                .count(),
            as_queued_flagged: pairs
                .iter()
                .filter(|p| p.verdict == "dismiss" && !p.linked)
                .filter(unbiased)
                .filter(contra)
                .filter(|p| p.score >= min)
                .count(),
            as_queued: pairs
                .iter()
                .filter(|p| p.verdict == "dismiss" && !p.linked)
                .filter(unbiased)
                .count(),
            conflicts_flagged: pairs
                .iter()
                .filter(|p| p.verdict == "conflict")
                .filter(contra)
                .filter(|p| p.score >= min)
                .count(),
            conflicts: pairs.iter().filter(|p| p.verdict == "conflict").count(),
        });
    }

    Ok(RealGraphReport {
        graph: path.to_string(),
        model,
        nodes,
        suspects: suspects.len(),
        pending: suspects
            .iter()
            .filter(|s| s.status == SuspectStatus::Suspected)
            .count(),
        judged: pairs.len(),
        contradiction_hinted,
        pairs,
        gates,
        shipped_gate,
        similarity_floor: floor,
    })
}

// ------------------------------------------------------------------- bench

#[derive(Debug, Clone, Serialize)]
pub struct BenchRow {
    pub label: String,
    pub weighted_recall: f64,
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub lexical: f64,
    pub paraphrase: f64,
    pub oblique: f64,
    pub tokens_mean: f64,
    /// How well any confidence threshold separates answerable questions from
    /// ones with no written answer. Carried because a strategy that wins recall
    /// by returning more of everything should not be able to hide it here.
    pub separation: f64,
    pub false_positive_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub runtime: Runtime,
    pub graph: usize,
    pub edges: usize,
    pub questions: usize,
    pub rows: Vec<BenchRow>,
}

/// Run every candidate retrieval strategy over one corpus, one built store,
/// one set of embeddings.
///
/// Sharing the store is what makes this affordable and what makes it fair:
/// embedding the corpus is nearly the whole cost, and a strategy that had its
/// own store could differ from another by an indexing accident rather than by
/// its ranking.
pub fn bench(cfg: &Config) -> anyhow::Result<BenchReport> {
    use crate::variants::{Fusion, RerankMode, Strategy, VariantArm};

    let model = cfg.embed_model.as_deref();
    let size = *cfg.sizes.first().expect("at least one size");
    let c = corpus_full(
        size,
        size * cfg.distractor_ratio,
        cfg.seed,
        &cfg.profile,
        &cfg.type_mix,
    );
    let twins = twin_map(&c);
    let (embedder_name, _) = (embedder(model).1, ());
    let (rerank, rerank_name) = reranker();
    let engram = EngramArm::build(
        &c,
        embedder(model).0,
        if cfg.no_rerank { None } else { rerank },
    )?;
    let shared_rerank = reranker().0;
    let rerank_ref = shared_rerank.as_deref();

    let mut rows = Vec::new();
    let mut push = |label: &str, outcomes: &[Outcome], sep: Separation| {
        let split = by_phrasing(outcomes);
        let at = |p: Phrasing| {
            split
                .iter()
                .find(|(q, _)| *q == p)
                .map(|(_, s)| s.recall_at_5)
                .unwrap_or_default()
        };
        let overall = score(outcomes);
        eprintln!(
            "  {:<42} weighted {:.3}  lex {:.2}  para {:.2}  obliq {:.2}  tok {:.0}",
            label,
            cfg.phrasing.weighted_recall(&split),
            at(Phrasing::Lexical),
            at(Phrasing::Paraphrase),
            at(Phrasing::Oblique),
            overall.tokens_mean,
        );
        rows.push(BenchRow {
            label: label.to_string(),
            weighted_recall: cfg.phrasing.weighted_recall(&split),
            recall_at_1: overall.recall_at_1,
            recall_at_5: overall.recall_at_5,
            lexical: at(Phrasing::Lexical),
            paraphrase: at(Phrasing::Paraphrase),
            oblique: at(Phrasing::Oblique),
            tokens_mean: overall.tokens_mean,
            separation: sep.balanced_accuracy,
            false_positive_rate: sep.false_positive_rate,
        });
    };

    // The two references, measured on the same store: what ships, and what it
    // has to beat.
    let rag = RagArm::new(&engram, embedder(model).0);
    let (o, s) = measure(&rag, &c, cfg.limit, &twins);
    push("rag (pure vectors)", &o, s);
    let (o, s) = measure(&engram, &c, cfg.limit, &twins);
    push("engram (shipped)", &o, s);

    // The ladder. Each row changes exactly ONE thing from the row above it, so
    // a movement is attributable rather than merely observed.
    //
    // It is ordered to kill hypotheses in sequence. Row A should reproduce
    // `rag` — if it does not, the gap is not in ranking at all and every row
    // below is measuring the wrong thing. Row B adds only the cross-encoder.
    // Row C changes only what that cross-encoder is allowed to read.
    // Trimmed to the rows that still ask a live question. Four candidates were
    // measured and settled on 2026-07-27 and are not re-run: rerank depth 60
    // (no better than 30), the bge query instruction (no effect at all),
    // full-body reranking (worse than the excerpt), and graph spreading into
    // the ranking (wrecks it — see the note in the graph; the edges do carry
    // signal, but attaching neighbours AFTER ranking is the way to collect it).
    // Three reference rows, then the grid.
    //
    // The references bound it from below and above: `A` is what pure vectors
    // score, and `B`/`C` are what the cross-encoder does on top of them under
    // each authority. They use `VectorOnly`, which is NOT the same as the grid
    // at keyword_weight 0 — `VectorOnly` also skips the FTS candidate pool and
    // the semantic floor, so the two differ by more than one number and are
    // kept apart rather than conflated.
    let mut candidates = vec![
        Strategy {
            fusion: Fusion::VectorOnly,
            rerank_depth: 0,
            ..Strategy::shipped()
        }
        .label("A vector-only, no rerank (= rag)"),
        Strategy {
            fusion: Fusion::VectorOnly,
            rerank_depth: 30,
            ..Strategy::shipped()
        }
        .label("B  vector-only, reranker DECIDES"),
        Strategy {
            fusion: Fusion::VectorOnly,
            rerank_depth: 30,
            rerank_mode: RerankMode::RrfBlend { k: 10.0 },
            ..Strategy::shipped()
        }
        .label("C  vector-only, reranker VOTES"),
    ];

    // The grid: the product's own fusion at four keyword weights, under both
    // rerank authorities.
    //
    // It is a grid rather than two ladders because the previous pass measured
    // each knob alone and both looked dead — the sweep found keyword_weight
    // flat under `Replace`, and the blend measured negative at the shipped
    // weight. Those are consistent with an interaction, and an interaction is
    // invisible to any experiment that moves one knob at a time. 0.50 is what
    // ships; 0.00 is the same fusion with the channel silent, which is the
    // honest floor for "drop BM25" as the product would actually do it.
    for kw in [0.0, 0.15, 0.30, 0.50] {
        for (mode, name) in [
            (RerankMode::Replace, "DECIDES"),
            (RerankMode::RrfBlend { k: 10.0 }, "VOTES"),
        ] {
            // Both halves of the shipped pair, so the marker keeps pointing at
            // the product rather than at whichever cell it used to be.
            let ships_votes = engram_core::policy::RERANK_VOTE_K.is_some();
            let shipped = (kw - engram_core::policy::SEARCH_KEYWORD_WEIGHT).abs() < 1e-9
                && matches!(mode, RerankMode::RrfBlend { .. }) == ships_votes;
            candidates.push(
                Strategy {
                    fusion: Fusion::Weighted {
                        keyword_weight: kw,
                        semantic_floor: engram_core::policy::SEARCH_SEMANTIC_FLOOR,
                    },
                    rerank_depth: 30,
                    rerank_mode: mode,
                    ..Strategy::shipped()
                }
                .label(&format!(
                    "kw {kw:.2} · reranker {name}{}",
                    if shipped { "   <- ships today" } else { "" }
                )),
            );
        }
    }

    for strategy in candidates {
        let label = strategy.label.clone();
        let arm = VariantArm::new(
            engram.engine().store(),
            engram.keys().clone(),
            embedder(model).0,
            rerank_ref,
            strategy,
        )?;
        let (o, s) = measure(&arm, &c, cfg.limit, &twins);
        push(&label, &o, s);
    }

    Ok(BenchReport {
        runtime: Runtime {
            embeddings_are_fake: embedder_name.contains("(fake)"),
            embedder: embedder_name,
            reranker: rerank_name,
            nli: "not run".to_string(),
            seed: cfg.seed,
            limit: cfg.limit,
            type_mix: cfg.type_mix.clone(),
            profile: cfg.profile.clone(),
            phrasing: cfg.phrasing,
        },
        graph: c.facts.len(),
        edges: engram.edges_written,
        questions: c.questions().count(),
        rows,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Runtime {
    pub embedder: String,
    /// The reranker in the engram arm, or "none" — without it the arm is not
    /// the stack `serve`/`mcp` actually run.
    pub reranker: String,
    /// True = the numbers below describe plumbing, not meaning. Printed loudly
    /// so a fake-embedder run can never be quoted as a result.
    pub embeddings_are_fake: bool,
    pub nli: String,
    pub seed: u64,
    pub limit: usize,
    /// The weighting the questions were drawn under.
    pub type_mix: Vec<(Kind, u32)>,
    pub profile: Profile,
    pub phrasing: PhrasingMix,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhrasingScore {
    pub phrasing: Phrasing,
    pub score: Score,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArmReport {
    pub arm: String,
    /// Tokens this arm costs every session before anything is asked.
    pub standing_tokens: usize,
    pub overall: Score,
    pub by_phrasing: Vec<PhrasingScore>,
    pub separation: Separation,
}

#[derive(Debug, Clone, Serialize)]
pub struct SizeReport {
    /// Facts questions are asked about.
    pub size: usize,
    /// Facts written as noise and never asked about.
    pub distractors: usize,
    /// Everything in the graph: `size + distractors`.
    pub graph: usize,
    /// Edges actually written. Zero means the graph layer was not measured.
    pub edges: usize,
    pub questions: usize,
    pub unanswerable: usize,
    /// One entry per curated budget: how many facts the file had room for —
    /// the ceiling on what that budget can answer.
    pub curated: Vec<CuratedStat>,
    pub arms: Vec<ArmReport>,
    pub nli: NliReport,
}

/// What one curated-file budget could hold at one graph size.
#[derive(Debug, Clone, Serialize)]
pub struct CuratedStat {
    /// The arm row this stat belongs to.
    pub arm: String,
    pub budget: usize,
    pub held: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub runtime: Runtime,
    pub sizes: Vec<SizeReport>,
}

/// An embedding model that can be asked for by name at the command line.
///
/// Deliberately a short list of things already provisioned under
/// `~/.cache/engram`, not an arbitrary path: swapping the embedder changes
/// what every threshold in `policy.rs` means, so the set of models worth
/// comparing should be small enough to keep all of their numbers in view.
pub struct EmbedChoice {
    pub name: &'static str,
    pub dim: usize,
    /// bge-family models pool on CLS; the MiniLM/sentence-transformers family
    /// on the token mean. Getting this wrong produces vectors that are not
    /// wrong enough to crash and not right enough to retrieve.
    pub mean_pooling: bool,
}

pub const EMBED_CHOICES: [EmbedChoice; 3] = [
    EmbedChoice {
        name: "bge-small-en-v1.5",
        dim: 384,
        mean_pooling: false,
    },
    EmbedChoice {
        name: "bge-base-en-v1.5",
        dim: 768,
        mean_pooling: false,
    },
    EmbedChoice {
        name: "all-MiniLM-L6-v2",
        dim: 384,
        mean_pooling: true,
    },
];

/// Real embeddings when the feature is on and the model is on disk; the fake
/// bag-of-bytes otherwise. Which one ran is always reported.
pub fn embedder(model: Option<&str>) -> (Box<dyn Embedder>, String) {
    #[cfg(feature = "fastembed")]
    {
        if let Some(want) = model
            && let Some(choice) = EMBED_CHOICES.iter().find(|c| c.name == want)
        {
            match engram_core::cortex::cache_dir(choice.name).ok_or_else(|| {
                engram_core::Error::Embedding("no home directory for the model cache".into())
            }) {
                Ok(dir) => match engram_core::FastEmbedder::from_spec(
                    choice.name,
                    &dir,
                    choice.dim,
                    choice.mean_pooling,
                ) {
                    Ok(e) => return (Box::new(e), choice.name.to_string()),
                    Err(err) => {
                        eprintln!("! {want} unavailable ({err}); falling back to the default")
                    }
                },
                Err(err) => eprintln!("! {want} unavailable ({err}); falling back to the default"),
            }
        }
        match engram_core::FastEmbedder::new() {
            Ok(e) => {
                let name = e.name().to_string();
                return (Box::new(e), name);
            }
            Err(err) => eprintln!("! real embedder unavailable ({err}); falling back to fake"),
        }
    }
    let _ = model;
    let e = FakeEmbedder::default();
    let name = format!("{} (fake)", e.name());
    (Box::new(e), name)
}

/// The precision layer `serve` and `mcp` load. If it is missing the engram arm
/// is not the shipped stack, so the report names it either way.
pub fn reranker() -> (Option<Box<dyn engram_core::Reranker>>, String) {
    #[cfg(feature = "fastembed")]
    {
        match engram_core::FastReranker::new() {
            Ok(r) => {
                return (Some(Box::new(r)), "jina-reranker-v1-turbo-en".to_string());
            }
            Err(err) => eprintln!("! reranker unavailable ({err}); engram arm runs hybrid-only"),
        }
    }
    (None, "none".to_string())
}

/// The logic layer, and the NAME of whatever actually loaded.
///
/// Not a constant: `ENGRAM_NLI_DIR` swaps the model without a rebuild, which
/// is how candidates are compared. A report that hardcoded the default's name
/// would label every candidate's results as the incumbent's.
fn nli() -> (Box<dyn Nli>, String) {
    #[cfg(feature = "fastembed")]
    {
        let name = std::env::var("ENGRAM_NLI_DIR")
            .ok()
            .and_then(|d| {
                std::path::Path::new(&d)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
            })
            // The default must track the product: a hardcoded string here
            // mislabeled every report for a release after the mobilebert swap.
            .unwrap_or_else(|| engram_core::nli::NLI_MODEL_NAME.to_string());
        match engram_core::FastNli::new() {
            Ok(n) => return (Box::new(n), name),
            Err(err) => eprintln!("! real NLI unavailable ({err}); falling back to fake"),
        }
    }
    (Box::new(FakeNli), "fake".to_string())
}

// ------------------------------------------------------------ delivery floor

/// One candidate delivery floor, scored over recorded retrievals.
#[derive(Debug, Clone, Serialize)]
pub struct FloorPoint {
    pub floor: f64,
    pub recall_at_5: f64,
    pub oblique_recall_at_5: f64,
    /// Share of answerable questions where the floor left nothing — the
    /// recall paid for restraint.
    pub declined_answerable: f64,
    /// Share of control questions (no written answer) where nothing cleared
    /// the floor — the abstention the product gains.
    pub controls_declined: f64,
    pub noise: f64,
    pub focus: f64,
    pub mean_returned: f64,
    pub tokens_mean: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FloorSizeReport {
    pub graph: usize,
    pub questions: usize,
    pub controls: usize,
    pub points: Vec<FloorPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FloorReport {
    pub embedder: String,
    pub reranker: String,
    pub embeddings_are_fake: bool,
    pub seed: u64,
    pub limit: usize,
    pub sizes: Vec<FloorSizeReport>,
}

/// Sweep a delivery floor over the engram arm: retrieve once per question,
/// then score every candidate floor as arithmetic over the recorded hits.
///
/// The floor being sought serves both faces of calibrated delivery at once:
/// on a question with no written answer it should leave nothing (an explicit
/// "no memory" instead of confident noise), and on an answerable one it
/// should trim the weak tail without dropping the answer. The candidate grid
/// comes from the observed score distribution rather than a guessed scale —
/// engram scores are relevance-and-trust products whose range is an
/// implementation detail this sweep must not assume.
pub fn floor_sweep(cfg: &Config) -> anyhow::Result<FloorReport> {
    struct Rec {
        gold_pos: Option<usize>,
        phrasing: Phrasing,
        scores: Vec<f64>,
        entry_tokens: Vec<usize>,
    }

    let model = cfg.embed_model.as_deref();
    let (_, embedder_name) = embedder(model);
    let mut sizes = Vec::new();

    for &size in &cfg.sizes {
        let c = corpus_full(
            size,
            size * cfg.distractor_ratio,
            cfg.seed,
            &cfg.profile,
            &cfg.type_mix,
        );
        let engram = EngramArm::build(
            &c,
            embedder(model).0,
            if cfg.no_rerank { None } else { reranker().0 },
        )?;
        // The engine ships its own delivery trims since 0.8.2. This recorder
        // needs RAW curves — the rows below apply their own floors, so a
        // trimmed recording would double-cut and flatter whatever the engine
        // already does.
        engram.tune(|p| {
            p.delivery_floor = 0.0;
            p.knee_cliff = None;
            p.rerank_full_note = cfg.rerank_full;
        })?;

        let mut recs = Vec::new();
        for q in c.questions() {
            let Some(gold) = q.gold.as_ref() else {
                continue;
            };
            let r = engram.retrieve(&q.text, cfg.limit);
            let Delivery::Ranked(keys) = &r.delivery else {
                unreachable!("engram ranks")
            };
            recs.push(Rec {
                gold_pos: keys.iter().position(|k| k == gold),
                phrasing: q.phrasing,
                entry_tokens: r.rendered.iter().map(|e| crate::arms::tokens(e)).collect(),
                scores: r.scores,
            });
        }
        let control_scores: Vec<Vec<f64>> = c
            .unanswerable
            .iter()
            .map(|q| engram.retrieve(&q.text, cfg.limit).scores)
            .collect();

        let mut all: Vec<f64> = recs
            .iter()
            .flat_map(|r| r.scores.iter().copied())
            .chain(control_scores.iter().flatten().copied())
            .collect();
        all.sort_by(f64::total_cmp);
        let mut floors = vec![0.0];
        const STEPS: usize = 20;
        for i in 1..=STEPS {
            if all.is_empty() {
                break;
            }
            let q = all[(i * (all.len() - 1)) / STEPS];
            if floors.last().is_none_or(|l| q - l > 1e-9) {
                floors.push(q);
            }
        }

        let mut points = Vec::new();
        for &floor in &floors {
            let (mut hit5, mut obl_hit, mut obl_n, mut declined) = (0usize, 0usize, 0usize, 0usize);
            let (mut noise_sum, mut focus_sum) = (0.0f64, 0.0f64);
            let (mut focus_n, mut returned_sum, mut tokens_sum) = (0usize, 0usize, 0usize);
            for r in &recs {
                let kept: Vec<usize> = (0..r.scores.len())
                    .filter(|&i| r.scores[i] >= floor)
                    .collect();
                let kept_tokens: usize = kept.iter().filter_map(|&i| r.entry_tokens.get(i)).sum();
                returned_sum += kept.len();
                tokens_sum += kept_tokens;
                if r.phrasing == Phrasing::Oblique {
                    obl_n += 1;
                }
                if kept.is_empty() {
                    declined += 1;
                    continue;
                }
                let kept_rank = r.gold_pos.and_then(|g| kept.iter().position(|&i| i == g));
                match kept_rank {
                    Some(kr) => {
                        if kr < 5 {
                            hit5 += 1;
                            if r.phrasing == Phrasing::Oblique {
                                obl_hit += 1;
                            }
                        }
                        noise_sum += (kept.len() - 1) as f64 / kept.len() as f64;
                        if let Some(g) = r.gold_pos
                            && let Some(t) = r.entry_tokens.get(g)
                        {
                            focus_sum += *t as f64 / kept_tokens.max(1) as f64;
                            focus_n += 1;
                        }
                    }
                    None => noise_sum += 1.0,
                }
            }
            let n = recs.len().max(1);
            points.push(FloorPoint {
                floor,
                recall_at_5: hit5 as f64 / n as f64,
                oblique_recall_at_5: obl_hit as f64 / obl_n.max(1) as f64,
                declined_answerable: declined as f64 / n as f64,
                controls_declined: control_scores
                    .iter()
                    .filter(|s| s.iter().all(|v| *v < floor))
                    .count() as f64
                    / control_scores.len().max(1) as f64,
                noise: noise_sum / n as f64,
                focus: focus_sum / focus_n.max(1) as f64,
                mean_returned: returned_sum as f64 / n as f64,
                tokens_mean: tokens_sum as f64 / n as f64,
            });
        }

        sizes.push(FloorSizeReport {
            graph: c.facts.len(),
            questions: recs.len(),
            controls: control_scores.len(),
            points,
        });
    }

    Ok(FloorReport {
        embeddings_are_fake: embedder_name.contains("(fake)"),
        embedder: embedder_name,
        reranker: if cfg.no_rerank {
            "disabled (--no-rerank)".to_string()
        } else {
            reranker().1
        },
        seed: cfg.seed,
        limit: cfg.limit,
        sizes,
    })
}

// -------------------------------------------------------------- post-tune

/// The shipped post-tune stack, measured end to end at one size.
#[derive(Debug, Clone, Serialize)]
pub struct PostTuneSizeReport {
    pub graph: usize,
    pub edges: usize,
    pub questions: usize,
    pub controls: usize,
    /// What auto-tune's weak-line dial fitted on this graph's own phantom
    /// probes — the calibration the product actually runs, not the bench's
    /// idealized split-conformal one.
    pub weak_line: f64,
    pub auto_tune_note: Option<String>,
    pub standing_tokens: usize,
    /// Hits scored alone (hybrid) …
    pub overall: Score,
    /// …and with the 1-hop graph credit, matching the arms table's
    /// `engram-full` row.
    pub assisted: Score,
    pub by_phrasing: Vec<PhrasingScore>,
    pub weighted_recall: f64,
    /// Controls answered with a top score at/above the weak line — answered
    /// WITHOUT the "likely not in memory" recommendation: the honest FP.
    pub controls_unwarned: f64,
    /// Controls that came back empty (the `none` verdict).
    pub controls_empty: f64,
    /// Answerable questions whose delivery carried the recommendation.
    pub answerable_warned: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PostTuneReport {
    pub embedder: String,
    pub reranker: String,
    pub embeddings_are_fake: bool,
    pub seed: u64,
    pub limit: usize,
    pub sizes: Vec<PostTuneSizeReport>,
}

/// Measure the shipped 0.8.2 delivery stack — knee trim on, the weak line
/// calibrated by `Engine::auto_tune`'s phantom-probe dial exactly as a
/// session boundary would — as one engram-only pass per size.
///
/// This is the arms table's post-tune row. The pre-tune rows do not need
/// re-measuring: the same engine with `knee_cliff = null` and the fixed weak
/// line IS the 0.8.0 stack the existing tables recorded. FP here follows the
/// recommendation regime the product ships: candidates are never cut, so a
/// control answered under the warning counts as honest and only an
/// unwarned answer is a false positive.
pub fn posttune(cfg: &Config) -> anyhow::Result<PostTuneReport> {
    let model = cfg.embed_model.as_deref();
    let (_, embedder_name) = embedder(model);
    let mut sizes = Vec::new();

    for &size in &cfg.sizes {
        let c = corpus_full(
            size,
            size * cfg.distractor_ratio,
            cfg.seed,
            &cfg.profile,
            &cfg.type_mix,
        );
        let twins = twin_map(&c);
        let engram = EngramArm::build(
            &c,
            embedder(model).0,
            if cfg.no_rerank { None } else { reranker().0 },
        )?;
        if cfg.rerank_full {
            engram.tune(|p| p.rerank_full_note = true)?;
        }
        // Auto-tune is damped (half the distance per pass), and a real
        // deployment runs it at every session boundary — so the measured
        // stack is the converged line, not the first half-step. Sixteen
        // passes bounds the loop far above any real convergence.
        let mut tuned = None;
        for _ in 0..16 {
            match engram.engine().auto_tune()? {
                Some(note) => tuned = Some(note),
                None => break,
            }
        }
        let weak_line = engram.engine().graph_config().policy.weak_evidence_top;
        eprintln!(
            "  {size}: {} (weak line {weak_line:.3}, converged)",
            tuned.as_deref().unwrap_or("auto-tune left the defaults")
        );

        let (outcomes, _) = measure(&engram, &c, cfg.limit, &twins);
        let mut unwarned = 0usize;
        let mut empty = 0usize;
        for q in &c.unanswerable {
            let r = engram.retrieve(&q.text, cfg.limit);
            let answered = match &r.delivery {
                Delivery::Ranked(keys) => !keys.is_empty(),
                Delivery::Dump(_) => true,
            };
            if !answered {
                empty += 1;
            } else if r.top_score.unwrap_or(0.0) >= weak_line {
                unwarned += 1;
            }
        }
        let warned = outcomes
            .iter()
            .filter(|o| o.returned > 0 && o.top_score.unwrap_or(0.0) < weak_line)
            .count();

        let overall = score(&outcomes);
        let assisted_outcomes = crate::metrics::assisted(&outcomes);
        let mut assisted = score(&assisted_outcomes);
        // Assisted ranks are pre-filled, so neighbour-only would read as
        // zero; carry the real figure like the arms table does.
        assisted.neighbor_only = overall.neighbor_only;
        let split = by_phrasing(&assisted_outcomes);

        sizes.push(PostTuneSizeReport {
            graph: c.facts.len(),
            edges: engram.edges_written,
            questions: outcomes.len(),
            controls: c.unanswerable.len(),
            weak_line,
            auto_tune_note: tuned,
            standing_tokens: engram.standing_cost(),
            overall,
            assisted,
            weighted_recall: cfg.phrasing.weighted_recall(&split),
            by_phrasing: split
                .into_iter()
                .map(|(phrasing, score)| PhrasingScore { phrasing, score })
                .collect(),
            controls_unwarned: unwarned as f64 / c.unanswerable.len().max(1) as f64,
            controls_empty: empty as f64 / c.unanswerable.len().max(1) as f64,
            answerable_warned: warned as f64 / outcomes.len().max(1) as f64,
        });
    }

    Ok(PostTuneReport {
        embeddings_are_fake: embedder_name.contains("(fake)"),
        embedder: embedder_name,
        reranker: if cfg.no_rerank {
            "disabled (--no-rerank)".to_string()
        } else {
            reranker().1
        },
        seed: cfg.seed,
        limit: cfg.limit,
        sizes,
    })
}

// ------------------------------------------------------------- trick bench

/// One candidate delivery strategy, scored over recorded retrievals.
#[derive(Debug, Clone, Serialize)]
pub struct TrickRow {
    pub strategy: String,
    pub recall_at_5: f64,
    pub oblique_recall_at_5: f64,
    pub focus: f64,
    pub noise: f64,
    pub mean_returned: f64,
    pub tokens_mean: f64,
    /// Share of answerable questions the strategy left empty.
    pub declined_answerable: f64,
    /// Share of HELD-OUT control questions still answered — the abstention
    /// score, measured on probes the calibration never saw.
    pub false_positive_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrickSizeReport {
    pub graph: usize,
    pub questions: usize,
    pub controls_calibration: usize,
    pub controls_eval: usize,
    /// The conformal thresholds fitted on the calibration half.
    pub conformal_q90: f64,
    pub conformal_q95: f64,
    /// The q90 threshold read as a per-graph WEAK line instead of a gate:
    /// share of answerable questions whose top clears it (labeled strong)…
    pub label_answerable_strong: f64,
    /// …and share of held-out controls that would be flagged weak/none.
    /// Together they say whether calibrating `weak_evidence_top` from
    /// phantom probes beats the fixed 0.85.
    pub label_controls_flagged: f64,
    /// The recommendation regime (user ruling 2026-08-03: the pessimistic
    /// signal never cuts — it prepends "likely not in memory" and the
    /// candidates stay). Scored per calibration quantile: a warned control is
    /// a CORRECT outcome, an unwarned one is the remaining false positive,
    /// and a warned answerable question is the false-alarm cost — recall is
    /// untouched by construction.
    pub label_grid: Vec<LabelPoint>,
    pub rows: Vec<TrickRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LabelPoint {
    pub quantile: f64,
    pub threshold: f64,
    /// Held-out controls answered WITHOUT the recommendation — the honest FP.
    pub controls_unwarned: f64,
    /// Answerable questions carrying the false warning (answer still delivered).
    pub answerable_warned: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TricksReport {
    pub embedder: String,
    pub reranker: String,
    pub embeddings_are_fake: bool,
    pub seed: u64,
    pub limit: usize,
    pub sizes: Vec<TrickSizeReport>,
}

/// The literature's training-free cut: find the largest relative drop in the
/// DESC-sorted score curve (the knee between the relevance head and the noise
/// tail — Tail-Aware Adaptive-k, arXiv 2606.11907, simplified to the knee
/// without the EVT validation pass). Returns the minimum score to KEEP, or
/// None when the curve has no cliff worth acting on.
fn knee_threshold(sorted_desc: &[f64]) -> Option<f64> {
    if sorted_desc.len() <= 1 {
        return None;
    }
    let (mut best_at, mut best_drop) = (0usize, 0.0f64);
    for i in 1..sorted_desc.len() {
        let prev = sorted_desc[i - 1].max(1e-9);
        let drop = (sorted_desc[i - 1] - sorted_desc[i]) / prev;
        if drop > best_drop {
            best_drop = drop;
            best_at = i;
        }
    }
    // A flat curve has no knee; cutting at its noise-level maximum drop would
    // amputate the head one query in three. A quarter of the running score is
    // a real cliff.
    (best_drop >= 0.25).then(|| sorted_desc[best_at - 1])
}

fn quantile(sorted_asc: &[f64], q: f64) -> f64 {
    if sorted_asc.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_asc.len() - 1) as f64 * q).ceil() as usize;
    sorted_asc[idx.min(sorted_asc.len() - 1)]
}

/// The research bench for the 0.8.1 cycle: candidate delivery strategies —
/// fixed floors, relative-to-top trims, the knee cut, and split-conformal
/// abstention calibrated on synthetic never-written probes — all scored from
/// ONE recorded retrieval pass per size. Nothing here ships; the table
/// decides what is worth proposing.
pub fn tricks(cfg: &Config) -> anyhow::Result<TricksReport> {
    struct Rec {
        gold_pos: Option<usize>,
        phrasing: Phrasing,
        scores: Vec<f64>,
        entry_tokens: Vec<usize>,
    }
    /// One strategy = a trim rule composed of up to three floors (absolute,
    /// relative-to-top, knee) plus an optional conformal abstention gate.
    /// `flat_only` is the MIXED signal (user direction 2026-08-03): a
    /// pessimistic calibrated score says "probably not there", but a real
    /// answer's curve usually still has a cliff after its head while a
    /// no-answer curve is flat — so abstain only when the score is low AND
    /// the curve is shapeless.
    /// `buffer` rescues up to B hits the KNEE cut (never ones under the
    /// absolute/relative floors) — Adaptive-k's fix (arXiv 2506.08479) for
    /// exactly our measured failure: the knee sometimes amputates a
    /// neighbor-credit carrier sitting just past the cliff.
    struct Strategy {
        name: &'static str,
        abs_floor: f64,
        rel: f64,
        knee: bool,
        abstain_q: Option<f64>,
        flat_only: bool,
        buffer: usize,
    }
    const S: fn(&'static str, f64, f64, bool, Option<f64>, bool, usize) -> Strategy =
        |name, abs_floor, rel, knee, abstain_q, flat_only, buffer| Strategy {
            name,
            abs_floor,
            rel,
            knee,
            abstain_q,
            flat_only,
            buffer,
        };
    let strategies: Vec<Strategy> = vec![
        S("shipped delivery", 0.22, 0.0, false, None, false, 0),
        S("relative .5*top", 0.22, 0.5, false, None, false, 0),
        S("relative .6*top", 0.22, 0.6, false, None, false, 0),
        S("relative .75*top", 0.22, 0.75, false, None, false, 0),
        S("knee cut", 0.22, 0.0, true, None, false, 0),
        S("knee + buf1", 0.22, 0.0, true, None, false, 1),
        S("knee + buf2", 0.22, 0.0, true, None, false, 2),
        S("knee + buf3", 0.22, 0.0, true, None, false, 3),
        S("knee + buf5", 0.22, 0.0, true, None, false, 5),
        S("rel .6 + q50", 0.22, 0.6, false, Some(0.50), false, 0),
        S("rel .6 + q75", 0.22, 0.6, false, Some(0.75), false, 0),
        S("knee + q75", 0.22, 0.0, true, Some(0.75), false, 0),
        S("knee + q90", 0.22, 0.0, true, Some(0.90), false, 0),
        S("knee + q90 flat-only", 0.22, 0.0, true, Some(0.90), true, 0),
        S(
            "knee + q90 flat-only + buf2",
            0.22,
            0.0,
            true,
            Some(0.90),
            true,
            2,
        ),
        S(
            "rel .6 + q90 flat-only",
            0.22,
            0.6,
            false,
            Some(0.90),
            true,
            0,
        ),
        S(
            "rel .6 + q95 flat-only",
            0.22,
            0.6,
            false,
            Some(0.95),
            true,
            0,
        ),
    ];

    let model = cfg.embed_model.as_deref();
    let (_, embedder_name) = embedder(model);
    let mut sizes = Vec::new();

    for &size in &cfg.sizes {
        let c = corpus_full(
            size,
            size * cfg.distractor_ratio,
            cfg.seed,
            &cfg.profile,
            &cfg.type_mix,
        );
        let engram = EngramArm::build(
            &c,
            embedder(model).0,
            if cfg.no_rerank { None } else { reranker().0 },
        )?;
        // The engine ships its own delivery trims since 0.8.2. This recorder
        // needs RAW curves — the rows below apply their own floors, so a
        // trimmed recording would double-cut and flatter whatever the engine
        // already does.
        engram.tune(|p| {
            p.delivery_floor = 0.0;
            p.knee_cliff = None;
            p.rerank_full_note = cfg.rerank_full;
        })?;

        let mut recs = Vec::new();
        for q in c.questions() {
            let Some(gold) = q.gold.as_ref() else {
                continue;
            };
            let r = engram.retrieve(&q.text, cfg.limit);
            let Delivery::Ranked(keys) = &r.delivery else {
                unreachable!("engram ranks")
            };
            recs.push(Rec {
                gold_pos: keys.iter().position(|k| k == gold),
                phrasing: q.phrasing,
                entry_tokens: r.rendered.iter().map(|e| crate::arms::tokens(e)).collect(),
                scores: r.scores,
            });
        }
        let control_scores: Vec<Vec<f64>> = c
            .unanswerable
            .iter()
            .map(|q| engram.retrieve(&q.text, cfg.limit).scores)
            .collect();
        // Deterministic split: even indices calibrate, odd indices evaluate.
        // The threshold is never scored on the probes that set it.
        let (cal, eval): (Vec<_>, Vec<_>) = control_scores
            .iter()
            .enumerate()
            .partition(|(i, _)| i % 2 == 0);
        let top_of = |s: &[f64]| s.iter().fold(0.0f64, |a, b| a.max(*b));
        let mut cal_tops: Vec<f64> = cal.into_iter().map(|(_, s)| top_of(s)).collect();
        cal_tops.sort_by(f64::total_cmp);
        let eval_curves: Vec<&Vec<f64>> = eval.into_iter().map(|(_, s)| s).collect();
        let (q90, q95) = (quantile(&cal_tops, 0.90), quantile(&cal_tops, 0.95));

        // Per-strategy: sorted curve → knee; floor = max of the three rules;
        // abstain when the conformal gate says "not there" (optionally only
        // on flat curves — the mixed signal). The buffer rescues up to B
        // hits in delivered order that the KNEE cut but the base floors
        // would have kept — a knee-cut carrier gets back in, a hit the
        // measured-free absolute floor rejected stays out.
        let kept_indices = |s: &Strategy, scores: &[f64]| -> Vec<usize> {
            let mut sorted = scores.to_vec();
            sorted.sort_by(|a, b| b.total_cmp(a));
            let top = sorted.first().copied().unwrap_or(0.0);
            let base = s.abs_floor.max(s.rel * top);
            let mut floor = base;
            if s.knee
                && let Some(k) = knee_threshold(&sorted)
            {
                floor = floor.max(k);
            }
            let mut kept = Vec::new();
            let mut buffered = 0usize;
            for (i, &v) in scores.iter().enumerate() {
                if v >= floor {
                    kept.push(i);
                } else if v >= base && buffered < s.buffer {
                    kept.push(i);
                    buffered += 1;
                }
            }
            kept
        };
        let declines = |s: &Strategy, abstain_at: Option<f64>, scores: &[f64]| -> bool {
            if scores.is_empty() {
                return true;
            }
            let Some(t) = abstain_at else { return false };
            let top = scores.iter().fold(0.0f64, |a, b| a.max(*b));
            if top >= t {
                return false;
            }
            if !s.flat_only {
                return true;
            }
            let mut sorted = scores.to_vec();
            sorted.sort_by(|a, b| b.total_cmp(a));
            knee_threshold(&sorted).is_none()
        };

        let mut rows = Vec::new();
        for s in &strategies {
            let abstain_at = s.abstain_q.map(|q| quantile(&cal_tops, q));
            let (mut hit5, mut obl_hit, mut obl_n, mut declined) = (0usize, 0usize, 0usize, 0usize);
            let (mut noise_sum, mut focus_sum) = (0.0f64, 0.0f64);
            let (mut focus_n, mut returned_sum, mut tokens_sum) = (0usize, 0usize, 0usize);
            for r in &recs {
                if r.phrasing == Phrasing::Oblique {
                    obl_n += 1;
                }
                if declines(s, abstain_at, &r.scores) {
                    declined += 1;
                    continue;
                }
                let kept = kept_indices(s, &r.scores);
                if kept.is_empty() {
                    declined += 1;
                    continue;
                }
                let kept_tokens: usize = kept.iter().filter_map(|&i| r.entry_tokens.get(i)).sum();
                returned_sum += kept.len();
                tokens_sum += kept_tokens;
                let kept_rank = r.gold_pos.and_then(|g| kept.iter().position(|&i| i == g));
                match kept_rank {
                    Some(kr) => {
                        if kr < 5 {
                            hit5 += 1;
                            if r.phrasing == Phrasing::Oblique {
                                obl_hit += 1;
                            }
                        }
                        noise_sum += (kept.len() - 1) as f64 / kept.len() as f64;
                        if let Some(g) = r.gold_pos
                            && let Some(t) = r.entry_tokens.get(g)
                        {
                            focus_sum += *t as f64 / kept_tokens.max(1) as f64;
                            focus_n += 1;
                        }
                    }
                    None => noise_sum += 1.0,
                }
            }
            let n = recs.len().max(1);
            rows.push(TrickRow {
                strategy: s.name.to_string(),
                recall_at_5: hit5 as f64 / n as f64,
                oblique_recall_at_5: obl_hit as f64 / obl_n.max(1) as f64,
                focus: focus_sum / focus_n.max(1) as f64,
                noise: noise_sum / n as f64,
                mean_returned: returned_sum as f64 / n as f64,
                tokens_mean: tokens_sum as f64 / n as f64,
                declined_answerable: declined as f64 / n as f64,
                // Answered = the strategy's full pipeline leaves at least one
                // hit on this held-out control curve.
                false_positive_rate: eval_curves
                    .iter()
                    .filter(|scores| {
                        if declines(s, abstain_at, scores) {
                            return false;
                        }
                        !kept_indices(s, scores).is_empty()
                    })
                    .count() as f64
                    / eval_curves.len().max(1) as f64,
            });
        }

        sizes.push(TrickSizeReport {
            graph: c.facts.len(),
            questions: recs.len(),
            controls_calibration: cal_tops.len(),
            controls_eval: eval_curves.len(),
            conformal_q90: q90,
            conformal_q95: q95,
            label_answerable_strong: recs.iter().filter(|r| top_of(&r.scores) >= q90).count()
                as f64
                / recs.len().max(1) as f64,
            label_controls_flagged: eval_curves.iter().filter(|s| top_of(s) < q90).count() as f64
                / eval_curves.len().max(1) as f64,
            label_grid: [0.5, 0.6, 0.7, 0.75, 0.8, 0.85, 0.9, 0.95]
                .iter()
                .map(|&q| {
                    let t = quantile(&cal_tops, q);
                    LabelPoint {
                        quantile: q,
                        threshold: t,
                        controls_unwarned: eval_curves.iter().filter(|s| top_of(s) >= t).count()
                            as f64
                            / eval_curves.len().max(1) as f64,
                        answerable_warned: recs.iter().filter(|r| top_of(&r.scores) < t).count()
                            as f64
                            / recs.len().max(1) as f64,
                    }
                })
                .collect(),
            rows,
        });
    }

    Ok(TricksReport {
        embeddings_are_fake: embedder_name.contains("(fake)"),
        embedder: embedder_name,
        reranker: if cfg.no_rerank {
            "disabled (--no-rerank)".to_string()
        } else {
            reranker().1
        },
        seed: cfg.seed,
        limit: cfg.limit,
        sizes,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct QppFeatureAuc {
    pub feature: &'static str,
    /// P(answerable feature > control feature), rank AUC. 0.5 = blind;
    /// below 0.5 = the signal points the other way (controls score higher —
    /// e.g. entropy, where a real answer's curve is PEAKED, not flat).
    pub auc: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QppZPoint {
    pub z: f64,
    /// Share of controls whose pool-bottom z clears the threshold —
    /// the false-positive rate this gate would leave unwarned.
    pub controls_unwarned: f64,
    /// Share of answerable questions the gate would warn on.
    pub answerable_warned: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QppGumbelPoint {
    /// Significance level: unwarned when P(null max > top score) <= p.
    pub p: f64,
    pub controls_unwarned: f64,
    pub answerable_warned: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QppSizeReport {
    pub graph: usize,
    pub answerable: usize,
    pub controls: usize,
    /// The shipped reference: the phantom-probe weak line auto-tune fits on
    /// this same store, and what it does to these same curves.
    pub weak_line: f64,
    pub weak_line_controls_unwarned: f64,
    pub weak_line_answerable_warned: f64,
    pub features: Vec<QppFeatureAuc>,
    /// Pool-bottom z (free, but the pool bottom is a top-k survivor —
    /// pre-filtered, crowd-inflated). Kept as the baseline the random
    /// background has to beat.
    pub z_sweep: Vec<QppZPoint>,
    /// Random-background z: the query reranked against a seeded sample of
    /// notes retrieval never filtered — the query's own null, in its own
    /// register.
    pub z_rand_sweep: Vec<QppZPoint>,
    /// Gumbel p-value of the top score under the fitted null-max
    /// distribution (Karlin–Altschul / BLAST E-value shape).
    pub gumbel_sweep: Vec<QppGumbelPoint>,
    /// Local-crowd z: top score against the shoulder (ranks 15..30) of a
    /// 30-deep retrieve — the null that carries the query's own crowding.
    pub z_shoulder_sweep: Vec<QppZPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QppReport {
    pub embeddings_are_fake: bool,
    pub embedder: String,
    pub reranker: String,
    pub seed: u64,
    pub limit: usize,
    pub sizes: Vec<QppSizeReport>,
}

const QPP_FEATURES: [&str; 16] = [
    "top1",
    "gap12",
    "gap_median",
    "std",
    "entropy_t1",
    "entropy_t01",
    "nqc",
    "wig",
    "smv",
    "preknee_count",
    "knee_flat",
    "z_bottom",
    "z_rand",
    "gumbel_nlogp",
    "coherence",
    "z_shoulder",
];

/// Post-retrieval QPP features over one DESC-sorted delivered score curve.
/// Every signal is per-query arithmetic — the register lesson says absolute
/// score scales don't transfer between graphs, so each feature is either
/// relative to the curve's own background (bottom half of the delivered
/// pool) or a pure shape statistic. `None` when the curve is empty.
fn qpp_features(scores: &[f64]) -> Option<[f64; 12]> {
    if scores.is_empty() {
        return None;
    }
    let mut s = scores.to_vec();
    s.sort_by(|a, b| b.total_cmp(a));
    let k = s.len();
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let std = |v: &[f64]| {
        let m = mean(v);
        (v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / v.len() as f64).sqrt()
    };
    let entropy = |v: &[f64], t: f64| {
        if v.len() < 2 {
            return 0.0;
        }
        let m = v.iter().fold(f64::MIN, |a, &b| a.max(b));
        let exps: Vec<f64> = v.iter().map(|x| ((x - m) / t).exp()).collect();
        let z: f64 = exps.iter().sum();
        let h: f64 = exps
            .iter()
            .map(|e| {
                let p = e / z;
                if p > 0.0 { -p * p.ln() } else { 0.0 }
            })
            .sum();
        h / (v.len() as f64).ln()
    };

    let top1 = s[0];
    let gap12 = if k > 1 { s[0] - s[1] } else { s[0] };
    let gap_median = s[0] - s[k / 2];
    // Background = the bottom half of the delivered pool: what "nothing in
    // particular" scores for THIS query in THIS register, for free.
    let bottom = &s[k / 2..];
    let (mu_bg, sd_bg) = (mean(bottom), std(bottom).max(1e-6));
    let head = &s[..k.min(5)];
    let (mu_head, sd_head) = (mean(head), std(head));
    let nqc = sd_head / mu_bg.max(1e-6);
    let wig = mu_head - mu_bg;
    let smv = head
        .iter()
        .map(|&x| x * (x.max(1e-9) / mu_head.max(1e-9)).ln().abs())
        .sum::<f64>()
        / head.len() as f64
        / mu_bg.max(1e-6);
    let knee = knee_threshold(&s);
    let preknee_count = match knee {
        Some(t) => s.iter().filter(|&&v| v >= t).count() as f64,
        None => k as f64,
    };
    let knee_flat = if knee.is_none() { 1.0 } else { 0.0 };
    let z_bottom = (s[0] - mu_bg) / sd_bg;

    Some([
        top1,
        gap12,
        gap_median,
        std(&s),
        entropy(&s, 1.0),
        entropy(&s, 0.1),
        nqc,
        wig,
        smv,
        preknee_count,
        knee_flat,
        z_bottom,
    ])
}

/// Rank AUC: P(a > b) + 0.5·P(a == b) over all cross pairs.
fn rank_auc(a: &[f64], b: &[f64]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.5;
    }
    let mut wins = 0.0f64;
    for &x in a {
        for &y in b {
            if x > y {
                wins += 1.0;
            } else if x == y {
                wins += 0.5;
            }
        }
    }
    wins / (a.len() as f64 * b.len() as f64)
}

/// The QPP bench (2026-08 cycle, tier 1): can a per-QUERY signal computed
/// from the score curve itself separate answerable from never-written where
/// the per-GRAPH phantom line cannot? The register lesson predicts yes: an
/// in-register unanswerable query inflates every absolute score, but the
/// curve's SHAPE stays flat, and a background-relative z cancels the
/// register term by construction. Research only — nothing here ships.
pub fn qpp(cfg: &Config) -> anyhow::Result<QppReport> {
    anyhow::ensure!(
        !cfg.no_rerank,
        "--qpp reads the cross-encoder's calibrated scale; it means nothing with --no-rerank"
    );
    let model = cfg.embed_model.as_deref();
    let (_, embedder_name) = embedder(model);
    let mut sizes = Vec::new();

    for &size in &cfg.sizes {
        let c = corpus_full(
            size,
            size * cfg.distractor_ratio,
            cfg.seed,
            &cfg.profile,
            &cfg.type_mix,
        );
        let engram = EngramArm::build(&c, embedder(model).0, reranker().0)?;
        // The shipped reference first, on the shipped config: what the
        // phantom-probe dial converges to for this store (moves are damped,
        // so one pass is only half the journey).
        for _ in 0..16 {
            if engram.engine().auto_tune()?.is_none() {
                break;
            }
        }
        let weak_line = engram.engine().graph_config().policy.weak_evidence_top;
        // Then raw curves — the features read the whole delivered pool, so
        // the engine's own trims must not pre-shape it. Trust's rank tilt is
        // zeroed too: the background null scores plain sigmoid(logit), and
        // the top score has to be measured on the same ruler.
        engram.tune(|p| {
            p.delivery_floor = 0.0;
            p.knee_cliff = None;
            p.rerank_trust_weight = 0.0;
            p.rerank_full_note = cfg.rerank_full;
        })?;

        // The per-query null: a seeded sample of notes retrieval never saw
        // as candidates, reranked against every query. The pool bottom is a
        // top-k survivor — pre-filtered and crowd-inflated — while this
        // sample answers "what does an arbitrary note in this graph's
        // register score for THIS query".
        let (bg_rerank, _) = reranker();
        let bg_rerank = bg_rerank.expect("--qpp requires the reranker");
        let mut order: Vec<usize> = (0..c.facts.len()).collect();
        crate::rng::Rng::new(cfg.seed ^ (size as u64) << 17).shuffle(&mut order);
        let bg_docs: Vec<String> = order
            .into_iter()
            .take(32)
            .map(|i| format!("{}\n{}", c.facts[i].title, c.facts[i].body))
            .collect();
        let coh_embed = embedder(model).0;

        struct QCurve {
            scores: Vec<f64>,
            z_rand: f64,
            gumbel_p: f64,
            coherence: f64,
            /// z of the top score against the SHOULDER of a 30-deep retrieve
            /// (ranks 15..30): the one null that carries the query's own
            /// local crowding — a random sample never contains the topical
            /// near-miss cluster that actually inflates control scores.
            z_shoulder: f64,
        }
        let sigmoid = |l: f32| 1.0 / (1.0 + (-l as f64).exp());
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
        let std = |v: &[f64]| {
            let m = mean(v);
            (v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / v.len().max(1) as f64).sqrt()
        };
        let cosine = |a: &[f32], b: &[f32]| {
            let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
            (dot / (na * nb).max(1e-9)) as f64
        };
        let curve = |text: &str| -> QCurve {
            // A 30-deep retrieve: the head (top cfg.limit) feeds the curve
            // features, the shoulder feeds the local-crowd null.
            let deep = engram.retrieve(text, cfg.limit.max(30));
            let mut full = deep.scores.clone();
            full.sort_by(|a, b| b.total_cmp(a));
            let r_scores: Vec<f64> = full.iter().take(cfg.limit).copied().collect();
            let top1 = full.first().copied().unwrap_or(0.0);
            let z_shoulder = if full.len() >= 20 {
                let sh = &full[15..full.len().min(30)];
                let (m, s) = (mean(sh), std(sh).max(1e-6));
                (top1 - m) / s
            } else {
                0.0
            };
            let r = deep;
            let bg: Vec<f64> = bg_rerank
                .rank(text, &bg_docs)
                .map(|ls| ls.into_iter().map(sigmoid).collect())
                .unwrap_or_default();
            let (z_rand, gumbel_p) = if bg.len() >= 8 {
                let z = (top1 - mean(&bg)) / std(&bg).max(1e-6);
                // Null maxima: the max of each 4-note block, Gumbel fitted
                // by method of moments (β = σ√6/π, μ = m − 0.5772β), then
                // P(null max > top1) — the BLAST E-value shape.
                let maxima: Vec<f64> = bg
                    .chunks(4)
                    .filter(|ch| ch.len() == 4)
                    .map(|ch| ch.iter().fold(f64::MIN, |a, &b| a.max(b)))
                    .collect();
                let beta = (std(&maxima) * 6.0f64.sqrt() / std::f64::consts::PI).max(1e-6);
                let mu = mean(&maxima) - 0.5772 * beta;
                let p = 1.0 - (-(-(top1 - mu) / beta).exp()).exp();
                (z, p.clamp(0.0, 1.0))
            } else {
                (0.0, 1.0)
            };
            // Coherence: mean pairwise cosine of the top hits' embeddings.
            // A real answer's head huddles around the answer; a
            // never-written question's head is topical scatter — or so the
            // hypothesis goes; the AUC row is the verdict.
            let head: Vec<Vec<f32>> = r
                .rendered
                .iter()
                .take(5)
                .filter_map(|t| coh_embed.embed_one(t).ok())
                .collect();
            let mut coh = 0.0;
            if head.len() >= 2 {
                let mut n = 0usize;
                for i in 0..head.len() {
                    for j in i + 1..head.len() {
                        coh += cosine(&head[i], &head[j]);
                        n += 1;
                    }
                }
                coh /= n as f64;
            }
            QCurve {
                scores: r_scores,
                z_rand,
                gumbel_p,
                coherence: coh,
                z_shoulder,
            }
        };

        let answerable: Vec<QCurve> = c
            .questions()
            .filter(|q| q.gold.is_some())
            .map(|q| curve(&q.text))
            .collect();
        let controls: Vec<QCurve> = c.unanswerable.iter().map(|q| curve(&q.text)).collect();
        eprintln!(
            "  {size}: weak line {weak_line:.3}, {} answerable / {} control curves",
            answerable.len(),
            controls.len()
        );

        let feats = |q: &QCurve| -> Option<Vec<f64>> {
            let base = qpp_features(&q.scores)?;
            let mut v = base.to_vec();
            v.push(q.z_rand);
            v.push(-(q.gumbel_p.max(1e-12)).ln());
            v.push(q.coherence);
            v.push(q.z_shoulder);
            Some(v)
        };
        let a_feats: Vec<Vec<f64>> = answerable.iter().filter_map(feats).collect();
        let c_feats: Vec<Vec<f64>> = controls.iter().filter_map(feats).collect();
        let features = (0..QPP_FEATURES.len())
            .map(|j| {
                let a: Vec<f64> = a_feats.iter().map(|f| f[j]).collect();
                let b: Vec<f64> = c_feats.iter().map(|f| f[j]).collect();
                QppFeatureAuc {
                    feature: QPP_FEATURES[j],
                    auc: rank_auc(&a, &b),
                }
            })
            .collect();

        // Gates, all priced like the weak line: unwarned controls (FP) vs
        // warned answerable, an empty curve counting as warned.
        let z_of = |q: &QCurve| qpp_features(&q.scores).map(|f| f[11]);
        let z_sweep = [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0]
            .iter()
            .map(|&t| QppZPoint {
                z: t,
                controls_unwarned: controls
                    .iter()
                    .filter(|q| z_of(q).is_some_and(|z| z >= t))
                    .count() as f64
                    / controls.len().max(1) as f64,
                answerable_warned: answerable
                    .iter()
                    .filter(|q| !z_of(q).is_some_and(|z| z >= t))
                    .count() as f64
                    / answerable.len().max(1) as f64,
            })
            .collect();
        let z_rand_sweep = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0]
            .iter()
            .map(|&t| QppZPoint {
                z: t,
                controls_unwarned: controls
                    .iter()
                    .filter(|q| !q.scores.is_empty() && q.z_rand >= t)
                    .count() as f64
                    / controls.len().max(1) as f64,
                answerable_warned: answerable
                    .iter()
                    .filter(|q| q.scores.is_empty() || q.z_rand < t)
                    .count() as f64
                    / answerable.len().max(1) as f64,
            })
            .collect();
        let z_shoulder_sweep = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0]
            .iter()
            .map(|&t| QppZPoint {
                z: t,
                controls_unwarned: controls
                    .iter()
                    .filter(|q| !q.scores.is_empty() && q.z_shoulder >= t)
                    .count() as f64
                    / controls.len().max(1) as f64,
                answerable_warned: answerable
                    .iter()
                    .filter(|q| q.scores.is_empty() || q.z_shoulder < t)
                    .count() as f64
                    / answerable.len().max(1) as f64,
            })
            .collect();
        let gumbel_sweep = [0.5, 0.25, 0.1, 0.05, 0.01]
            .iter()
            .map(|&p| QppGumbelPoint {
                p,
                controls_unwarned: controls
                    .iter()
                    .filter(|q| !q.scores.is_empty() && q.gumbel_p <= p)
                    .count() as f64
                    / controls.len().max(1) as f64,
                answerable_warned: answerable
                    .iter()
                    .filter(|q| q.scores.is_empty() || q.gumbel_p > p)
                    .count() as f64
                    / answerable.len().max(1) as f64,
            })
            .collect();

        let top_of = |q: &QCurve| q.scores.iter().fold(0.0f64, |a, &b| a.max(b));
        sizes.push(QppSizeReport {
            graph: c.facts.len(),
            answerable: answerable.len(),
            controls: controls.len(),
            weak_line,
            weak_line_controls_unwarned: controls
                .iter()
                .filter(|q| !q.scores.is_empty() && top_of(q) >= weak_line)
                .count() as f64
                / controls.len().max(1) as f64,
            weak_line_answerable_warned: answerable
                .iter()
                .filter(|q| q.scores.is_empty() || top_of(q) < weak_line)
                .count() as f64
                / answerable.len().max(1) as f64,
            features,
            z_sweep,
            z_rand_sweep,
            gumbel_sweep,
            z_shoulder_sweep,
        });
    }

    Ok(QppReport {
        embeddings_are_fake: embedder_name.contains("(fake)"),
        embedder: embedder_name,
        reranker: reranker().1,
        seed: cfg.seed,
        limit: cfg.limit,
        sizes,
    })
}

/// key -> the keys of facts named one syllable away from it.
fn twin_map(c: &Corpus) -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    for f in &c.facts {
        if let Some(src) = &f.twin_of {
            m.entry(f.key.clone()).or_default().push(src.clone());
            m.entry(src.clone()).or_default().push(f.key.clone());
        }
    }
    m
}

/// A single curated arm keeps its historical name; several budgets in one run
/// each carry their budget so the rows can be told apart.
fn curated_arm_name(budget: usize, single: bool) -> String {
    if single {
        "curated-file".to_string()
    } else {
        format!("curated-{budget}")
    }
}

fn report(name: &str, standing: usize, outcomes: &[Outcome], separation: Separation) -> ArmReport {
    ArmReport {
        arm: name.to_string(),
        standing_tokens: standing,
        overall: score(outcomes),
        by_phrasing: by_phrasing(outcomes)
            .into_iter()
            .map(|(phrasing, score)| PhrasingScore { phrasing, score })
            .collect(),
        separation,
    }
}

fn measure(
    arm: &dyn Arm,
    c: &Corpus,
    limit: usize,
    twins: &HashMap<String, Vec<String>>,
) -> (Vec<Outcome>, Separation) {
    let mut outcomes = Vec::new();
    let mut answerable_scores = Vec::new();

    for q in c.questions() {
        let Some(gold) = q.gold.as_ref() else {
            continue;
        };
        let r = arm.retrieve(&q.text, limit);
        let (rank, twin_above) = match &r.delivery {
            // No ranking: the fact is either inside the dump or it is not.
            // Position within it is not a rank, so a present fact scores 1 —
            // but a curated file that pruned the fact scores a miss, which is
            // the whole reason this variant carries its keys.
            Delivery::Dump(keys) => (keys.contains(gold).then_some(1), false),
            Delivery::Ranked(keys) => {
                let rank = keys.iter().position(|k| k == gold).map(|i| i + 1);
                let twin_above = twins.get(gold).is_some_and(|ts| {
                    keys.iter()
                        .position(|k| ts.contains(k))
                        .is_some_and(|t| rank.is_none_or(|r| t + 1 < r))
                });
                (rank, twin_above)
            }
        };
        // The graph layer: if the gold never ranked, was it one hop from
        // something that did?
        let via_neighbor = r
            .neighbors
            .iter()
            .filter(|(k, _)| k == gold)
            .map(|(_, at)| *at)
            .min();
        // Attention accounting: which delivered entry was the answer, and how
        // much of the delivered text was everything else. Rank indexes the
        // ranked arms' rendered entries; a dump's entries parallel its keys.
        let delivered_at = match &r.delivery {
            Delivery::Dump(keys) => keys.iter().position(|k| k == gold),
            Delivery::Ranked(_) => rank.map(|rk| rk - 1),
        };
        let focus = delivered_at
            .and_then(|i| r.rendered.get(i))
            .map(|entry| crate::arms::tokens(entry) as f64 / r.tokens.max(1) as f64);
        let returned = match &r.delivery {
            Delivery::Dump(keys) | Delivery::Ranked(keys) => keys.len(),
        };
        answerable_scores.push(r.top_score);
        outcomes.push(Outcome {
            phrasing: q.phrasing,
            rank,
            assisted_rank: rank.or(via_neighbor),
            tokens: r.tokens,
            top_score: r.top_score,
            twin_above,
            focus,
            returned,
        });
    }

    let unanswerable_scores: Vec<Option<f64>> = c
        .unanswerable
        .iter()
        .map(|q| {
            let r = arm.retrieve(&q.text, limit);
            match &r.delivery {
                // A dump always "returns" its whole payload, so it can never
                // decline — that is the honest reading, not a harness quirk.
                Delivery::Dump(_) => Some(1.0),
                Delivery::Ranked(keys) if keys.is_empty() => None,
                Delivery::Ranked(_) => r.top_score.or(Some(1.0)),
            }
        })
        .collect();

    (
        outcomes,
        separation(&answerable_scores, &unanswerable_scores),
    )
}

pub fn run(cfg: &Config) -> anyhow::Result<Report> {
    let model = cfg.embed_model.as_deref();
    let (_, embedder_name) = embedder(model);
    let (nli_model, nli_name) = nli();
    let mut sizes = Vec::new();

    for &size in &cfg.sizes {
        let c = corpus_full(
            size,
            size * cfg.distractor_ratio,
            cfg.seed,
            &cfg.profile,
            &cfg.type_mix,
        );
        let twins = twin_map(&c);
        let (emb, _) = embedder(model);

        let whole = WholeFileArm::new(&c);
        let curated: Vec<CuratedFileArm> = cfg
            .curated_budgets
            .iter()
            .map(|b| CuratedFileArm::new(&c, *b))
            .collect();
        let chance = ChanceArm::new(&c);
        let grep = GrepArm::new(&c);
        let engram = EngramArm::build(&c, emb, if cfg.no_rerank { None } else { reranker().0 })?;
        if cfg.flat_priors {
            engram.flatten_type_priors()?;
        }
        if cfg.rerank_full {
            engram.tune(|p| p.rerank_full_note = true)?;
        }
        let rag = RagArm::new(&engram, embedder(model).0);

        // The ladder, weakest first. `engram-hybrid` and `engram-full` are two
        // readings of one run: the same hits scored without and with the 1-hop
        // neighbourhood, which isolates what the graph adds for free.
        let mut arms = Vec::new();
        for a in [
            &chance as &dyn Arm,
            &grep as &dyn Arm,
            &rag as &dyn Arm,
            &engram as &dyn Arm,
        ] {
            let (outcomes, sep) = measure(a, &c, cfg.limit, &twins);
            let name = if a.name() == "engram" {
                "engram-hybrid"
            } else {
                a.name()
            };
            arms.push(report(name, a.standing_cost(), &outcomes, sep.clone()));
            if a.name() == "engram" {
                let mut full = report(
                    "engram-full",
                    a.standing_cost(),
                    &crate::metrics::assisted(&outcomes),
                    sep,
                );
                // Assisted outcomes have their rank already filled in, so
                // neighbour-only would compute to zero and read as "the graph
                // did nothing". Carry the real figure across from the
                // unassisted scoring, where it means something.
                full.overall.neighbor_only = arms
                    .last()
                    .map(|h| h.overall.neighbor_only)
                    .unwrap_or_default();
                arms.push(full);
            }
        }
        // The always-in-context baselines, weakest first: the file a human
        // actually maintains (once per budget), then the dump nobody maintains.
        for cur in &curated {
            let (curated_outcomes, curated_sep) = measure(cur, &c, cfg.limit, &twins);
            arms.push(report(
                &curated_arm_name(cur.budget(), curated.len() == 1),
                cur.standing_cost(),
                &curated_outcomes,
                curated_sep,
            ));
        }
        let (whole_outcomes, whole_sep) = measure(&whole, &c, cfg.limit, &twins);
        arms.push(report(
            "whole-file",
            whole.standing_cost(),
            &whole_outcomes,
            whole_sep,
        ));

        sizes.push(SizeReport {
            size,
            distractors: c.distractors(),
            graph: c.facts.len(),
            edges: engram.edges_written,
            questions: c.questions().count(),
            unanswerable: c.unanswerable.len(),
            curated: curated
                .iter()
                .map(|cur| CuratedStat {
                    arm: curated_arm_name(cur.budget(), curated.len() == 1),
                    budget: cur.budget(),
                    held: cur.held(),
                })
                .collect(),
            arms,
            nli: evaluate(nli_model.as_ref(), &nli_name, &c.pairs, cfg.nli_budget)?,
        });
    }

    Ok(Report {
        runtime: Runtime {
            embeddings_are_fake: embedder_name.contains("(fake)"),
            embedder: embedder_name,
            reranker: if cfg.no_rerank {
                "disabled (--no-rerank)".to_string()
            } else {
                reranker().1
            },
            nli: nli_name,
            seed: cfg.seed,
            limit: cfg.limit,
            type_mix: cfg.type_mix.clone(),
            profile: cfg.profile.clone(),
            phrasing: cfg.phrasing,
        },
        sizes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm<'a>(r: &'a Report, name: &str) -> &'a ArmReport {
        r.sizes[0]
            .arms
            .iter()
            .find(|a| a.arm == name)
            .unwrap_or_else(|| panic!("no {name} arm"))
    }

    fn smoke() -> Report {
        run(&Config {
            sizes: vec![40],
            distractor_ratio: 2,
            type_mix: DEFAULT_TYPE_MIX.to_vec(),
            profile: Profile::default(),
            seed: 1,
            limit: 10,
            nli_budget: 30,
            no_rerank: false,
            flat_priors: false,
            phrasing: PhrasingMix::default(),
            embed_model: None,
            curated_budgets: vec![DEFAULT_CURATED_BUDGET],
            rerank_full: false,
        })
        .unwrap()
    }

    #[test]
    fn the_suite_runs_end_to_end() {
        let r = smoke();
        let s = &r.sizes[0];
        assert_eq!(s.arms.len(), 7, "the full ablation ladder");
        assert!(s.questions > 0 && s.questions <= 40 * 3);
        assert_eq!(s.size, 40);
        assert_eq!(s.distractors, 80, "two distractors per tested fact");
        assert!(s.edges > 0, "a graph memory must be measured on a graph");
        assert_eq!(s.graph, 120);
        assert!(s.unanswerable > 0);
        for arm in &s.arms {
            assert_eq!(arm.overall.queries, s.questions);
            assert_eq!(arm.by_phrasing.len(), 3);
        }
    }

    #[test]
    fn the_whole_file_arm_is_perfect_and_expensive() {
        // The baseline everyone actually uses: nothing is ever missed, and
        // the entire file is paid for on every question. If this ever stops
        // being true the comparison has lost its anchor.
        let r = smoke();
        let whole = arm(&r, "whole-file");
        assert_eq!(whole.overall.recall_at_1, 1.0);
        assert!(whole.standing_tokens > 0);

        let engram = arm(&r, "engram-hybrid");
        assert!(
            engram.overall.tokens_mean < whole.overall.tokens_mean,
            "engram must deliver less than the whole file: {} vs {}",
            engram.overall.tokens_mean,
            whole.overall.tokens_mean
        );
    }

    #[test]
    fn focus_prices_attention_not_presence() {
        // The whole file delivers every answer and buries every answer: its
        // recall is 1.00 and its focus is one record over the entire graph. A
        // ranked arm delivers a handful of entries, so whenever it finds the
        // answer at all, the answer is a visible share of what arrived. If
        // these two ever converge, the focus column has stopped measuring.
        let r = smoke();
        let whole = arm(&r, "whole-file");
        assert!(
            whole.overall.focus > 0.0 && whole.overall.focus < 0.05,
            "one record in a 120-fact dump cannot be {:.3} of the text",
            whole.overall.focus
        );
        let grep = arm(&r, "grep");
        assert!(
            grep.overall.focus > whole.overall.focus * 5.0,
            "a ranked arm must concentrate attention: grep {:.3} vs whole {:.3}",
            grep.overall.focus,
            whole.overall.focus
        );
        // The curated file is a dump too — small, but still a dump: focus is
        // its entry over the whole file, bounded by how many entries fit.
        let curated = arm(&r, "curated-file");
        assert!(curated.overall.focus > whole.overall.focus);
    }

    #[test]
    fn a_full_dump_can_never_decline_an_unknown_question() {
        let r = smoke();
        assert_eq!(arm(&r, "whole-file").separation.false_positive_rate, 1.0);
    }

    #[test]
    fn the_chance_arm_is_a_floor_not_a_competitor() {
        // If a real arm ever fails to beat this on lexical questions, the
        // finding is that the arm is broken — not that retrieval is hard.
        let r = smoke();
        let chance = arm(&r, "chance");
        let engram = arm(&r, "engram-hybrid");
        let lexical = |a: &ArmReport| {
            a.by_phrasing
                .iter()
                .find(|p| p.phrasing == Phrasing::Lexical)
                .unwrap()
                .score
                .recall_at_5
        };
        assert!(
            lexical(engram) > lexical(chance) * 2.0,
            "engram {} vs chance {} on lexical questions",
            lexical(engram),
            lexical(chance)
        );
    }

    #[test]
    fn the_curated_file_is_bounded_by_its_budget_not_by_ranking() {
        // The claim this arm exists to make: a hand-maintained file answers
        // what it kept and nothing else. Its recall must therefore track how
        // much of the graph fits, and must NOT vary by phrasing — a fact in
        // the file is found however the question is worded, which is exactly
        // why it is a fair baseline and not a strawman.
        let r = smoke();
        let s = &r.sizes[0];
        let held = s.curated[0].held;
        assert!(
            held < s.graph,
            "a budget that holds everything is not a curated file"
        );
        let arm = arm(&r, "curated-file");
        let ceiling = held as f64 / s.graph as f64;
        assert!(
            arm.overall.recall_at_5 <= ceiling + 0.15,
            "recall {} exceeds what {} of {} facts could support",
            arm.overall.recall_at_5,
            held,
            s.graph
        );
        let spread = arm
            .by_phrasing
            .iter()
            .map(|p| p.score.recall_at_5)
            .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        assert!(
            spread.1 - spread.0 < 0.2,
            "a static file should not care how the question is phrased: {spread:?}"
        );
    }

    #[test]
    fn lexical_questions_are_findable_by_every_retrieving_arm() {
        // Deliberately the weakest assertion that still catches a broken
        // harness: quoting a fact's own words must retrieve it. Semantic
        // claims are not asserted here — they depend on the real embedder.
        // `chance` is excluded because it does not retrieve; that is its job.
        let r = smoke();
        // Three exclusions, each for its own reason. `chance` does not
        // retrieve. `rag` is pure vector search, and under the fake embedder
        // its vectors are a bag of bytes, so a lexical guarantee is not its to
        // make. `curated-file` does not retrieve either — it holds whatever
        // fits its budget, so missing most questions is the measurement rather
        // than a regression, and asserting otherwise would only be asserting
        // that the budget is large.
        for arm in r.sizes[0].arms.iter().filter(|a| {
            !matches!(a.arm.as_str(), "chance" | "rag") && !a.arm.starts_with("curated")
        }) {
            let lexical = arm
                .by_phrasing
                .iter()
                .find(|p| p.phrasing == Phrasing::Lexical)
                .unwrap();
            assert!(
                lexical.score.recall_at_10 > 0.9,
                "{} lost lexical recall: {}",
                arm.arm,
                lexical.score.recall_at_10
            );
        }
    }

    #[test]
    fn reruns_produce_the_same_ranking() {
        // Ranking, recall and delivered cost must be identical run to run.
        //
        // Two things deliberately are NOT asserted equal, because they depend
        // on wall-clock rather than on the seed: raw hit scores (trust decays
        // with age) and the brief's exact size (its section ordering is
        // recency-sensitive, so which record lands last inside the char budget
        // can change between two runs seconds apart). A drifting brief size is
        // real behaviour, not harness noise — it is bounded, which is the only
        // claim made about it.
        let (a, b) = (smoke(), smoke());
        for (x, y) in a.sizes[0].arms.iter().zip(&b.sizes[0].arms) {
            assert_eq!(x.arm, y.arm);
            assert_eq!(x.overall.recall_at_1, y.overall.recall_at_1);
            assert_eq!(x.overall.recall_at_5, y.overall.recall_at_5);
            assert_eq!(x.overall.mrr, y.overall.mrr);
            assert_eq!(x.overall.tokens_mean, y.overall.tokens_mean);
            let drift = x.standing_tokens.abs_diff(y.standing_tokens) as f64
                / x.standing_tokens.max(1) as f64;
            assert!(
                drift < 0.02,
                "{} standing cost moved {:.1}% between identical runs",
                x.arm,
                drift * 100.0
            );
        }
        assert_eq!(a.sizes[0].nli.accuracy, b.sizes[0].nli.accuracy);
    }
}
