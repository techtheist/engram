//! Scoring the logic layer.
//!
//! The conflict-similarity floor and the sweep's confidence gate were both
//! tuned by hand-judging a few dozen live suspects. That is the only evidence
//! there has ever been that the NLI layer helps. Generated pairs carry their
//! label by construction, so the same question can be asked over thousands of
//! pairs *in this domain's register* for nothing.
//!
//! It judges through `judge_pair(..).hint()` — the exact call the engine makes
//! when it stamps a suspect — so the number describes production, not a
//! laboratory.

use serde::Serialize;

use engram_core::Nli;

use crate::generate::{NliLabel, Pair};

const LABELS: [NliLabel; 3] = [
    NliLabel::Entailment,
    NliLabel::Neutral,
    NliLabel::Contradiction,
];

#[derive(Debug, Clone, Serialize)]
pub struct LabelScore {
    pub label: &'static str,
    pub support: usize,
    pub precision: f64,
    pub recall: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NliReport {
    pub model: String,
    pub pairs_examined: usize,
    pub pairs_total: usize,
    /// True when the pair budget cut the run short — PLAN §7A: no silent caps.
    pub truncated: bool,
    pub accuracy: f64,
    pub per_label: Vec<LabelScore>,
    /// `confusion[gold][predicted]`, both indexed entailment/neutral/contradiction.
    pub confusion: [[usize; 3]; 3],
    /// Mean confidence the model reported for the label it chose.
    pub mean_confidence: f64,
}

fn index(label: NliLabel) -> usize {
    match label {
        NliLabel::Entailment => 0,
        NliLabel::Neutral => 1,
        NliLabel::Contradiction => 2,
    }
}

fn from_hint(hint: &str) -> NliLabel {
    match hint {
        "contradiction" => NliLabel::Contradiction,
        "entailment" => NliLabel::Entailment,
        _ => NliLabel::Neutral,
    }
}

pub fn evaluate(
    nli: &dyn Nli,
    model: &str,
    pairs: &[Pair],
    budget: usize,
) -> anyhow::Result<NliReport> {
    let examined = pairs.len().min(budget);
    let mut confusion = [[0usize; 3]; 3];
    let mut confidence = 0.0f64;

    for p in &pairs[..examined] {
        let (hint, score) = nli.judge_pair(&p.premise, &p.hypothesis)?.hint();
        confusion[index(p.gold)][index(from_hint(hint))] += 1;
        confidence += score as f64;
    }

    let correct: usize = (0..3).map(|i| confusion[i][i]).sum();
    let per_label = LABELS
        .iter()
        .map(|&l| {
            let i = index(l);
            let support: usize = confusion[i].iter().sum();
            let predicted: usize = (0..3).map(|g| confusion[g][i]).sum();
            LabelScore {
                label: l.as_str(),
                support,
                precision: if predicted == 0 {
                    0.0
                } else {
                    confusion[i][i] as f64 / predicted as f64
                },
                recall: if support == 0 {
                    0.0
                } else {
                    confusion[i][i] as f64 / support as f64
                },
            }
        })
        .collect();

    Ok(NliReport {
        model: model.to_string(),
        pairs_examined: examined,
        pairs_total: pairs.len(),
        truncated: examined < pairs.len(),
        accuracy: if examined == 0 {
            0.0
        } else {
            correct as f64 / examined as f64
        },
        per_label,
        confusion,
        mean_confidence: if examined == 0 {
            0.0
        } else {
            confidence / examined as f64
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::corpus;
    use engram_core::FakeNli;

    #[test]
    fn every_pair_lands_in_the_confusion_matrix() {
        let c = corpus(20, 40, 1);
        let r = evaluate(&FakeNli, "fake", &c.pairs, usize::MAX).unwrap();
        let total: usize = r.confusion.iter().flatten().sum();
        assert_eq!(total, c.pairs.len());
        assert_eq!(r.pairs_examined, c.pairs.len());
        assert!(!r.truncated);
        assert!((0.0..=1.0).contains(&r.accuracy));
    }

    #[test]
    fn the_budget_is_reported_never_silent() {
        let c = corpus(20, 40, 1);
        let r = evaluate(&FakeNli, "fake", &c.pairs, 10).unwrap();
        assert_eq!(r.pairs_examined, 10);
        assert_eq!(r.pairs_total, c.pairs.len());
        assert!(r.truncated);
    }

    #[test]
    fn support_matches_the_generated_labels() {
        let c = corpus(15, 30, 3);
        let r = evaluate(&FakeNli, "fake", &c.pairs, usize::MAX).unwrap();
        for l in r.per_label.iter() {
            assert_eq!(l.support, c.tested(), "one pair per tested fact per label");
            assert!((0.0..=1.0).contains(&l.precision));
            assert!((0.0..=1.0).contains(&l.recall));
        }
    }
}
