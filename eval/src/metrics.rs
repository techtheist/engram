//! Scoring. Nothing here calls a model — every number is arithmetic over a
//! ranked list whose correct answer was known before the query was asked.

use serde::Serialize;

use crate::generate::{PHRASINGS, Phrasing};

#[derive(Debug, Clone)]
pub struct Outcome {
    pub phrasing: Phrasing,
    /// 1-based rank of the gold fact, `None` if it never appeared.
    pub rank: Option<usize>,
    /// Rank at which the gold fact became *available* — its own rank if it was
    /// retrieved directly, otherwise the rank of the hit whose 1-hop
    /// neighbourhood contained it. This is what the graph layer buys.
    pub assisted_rank: Option<usize>,
    pub tokens: usize,
    pub top_score: Option<f64>,
    /// A near-identically-named fact outranked the right one.
    pub twin_above: bool,
    /// Share of the delivered tokens that belonged to the answering record,
    /// when it was delivered at all. Attention is the budget: every token
    /// beyond the answer is distraction, so over-delivery is a graded false
    /// positive — recall says the answer was present, focus says what it was
    /// buried under.
    pub focus: Option<f64>,
    /// How many records the arm delivered for this question — the base the
    /// noise share is computed over.
    pub returned: usize,
    /// Dialogue turns between the answer and the nearest DELIVERED hit from
    /// the answer's own session — `Some(0)` when the answer itself was
    /// delivered, `None` when nothing delivered shares its session (and
    /// always `None` on a corpus built without `--history`, where no fact has
    /// one). This is what a caller would have to walk to reach the answer
    /// from the transcript instead of from the ranking.
    pub history_distance: Option<usize>,
    /// 1-based delivered position of the FIRST hit that shares the answer's
    /// session (the answer itself counts) — the rank at which a walk became
    /// possible. `None` when no delivered hit shares the session. Lets reach
    /// be read at a fixed depth (`reach@5`) the way recall is, so an arm that
    /// delivers ten records is not credited for breadth alone.
    pub history_rank: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Score {
    pub queries: usize,
    pub recall_at_1: f64,
    pub recall_at_3: f64,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub tokens_mean: f64,
    pub twin_confusion: f64,
    /// Share of questions where the gold fact was reachable ONLY through a
    /// neighbour — the graph was the sole path to it.
    pub neighbor_only: f64,
    /// Mean share of delivered tokens that were the answer, over the questions
    /// where the answer was delivered. `whole-file` holds every answer and
    /// still scores near zero here — presence is not the same as being
    /// readable, and this is the column that says so.
    pub focus: f64,
    /// Mean share of delivered RECORDS that were not the answer, over every
    /// question — a found answer among ten hits scores 0.9, a miss with ten
    /// hits scores 1.0 (everything delivered was a false positive), and an
    /// empty return scores 0.0, because saying nothing tells no lies. The one
    /// column where declining to answer is rewarded rather than invisible.
    pub noise: f64,
    /// Share of questions whose answer was delivered OR sat in a session a
    /// delivered hit came from — retrieval plus the transcript walk. Zero on
    /// every corpus built without `--history`.
    pub history_reach: f64,
    /// `history_reach` read at depth five: the answer ranked in the first
    /// five OR a session-mate did. The fair companion of `recall_at_5` — an
    /// arm that dumps ten records reaches more sessions than one that trims
    /// to three, and this column removes that breadth credit.
    pub history_reach_at_5: f64,
    /// Share of questions where the answer never ranked but a session-mate
    /// did: the history analogue of `neighbor_only`, and the only column the
    /// walk can claim for itself.
    pub history_only: f64,
    /// Mean dialogue distance, in turns, over exactly those `history_only`
    /// questions. Zero when there are none — a mean over nothing is not a
    /// short walk, and the reach column is what says whether it happened.
    pub history_distance: f64,
}

/// Dialogue turns between two notes of one transcript.
///
/// The model: an assistant writes one note per assistant turn, and a user turn
/// sits between consecutive assistant turns. So walking from the note at
/// 1-based turn `a` to the note at turn `b` of the same session costs
/// `2*|a-b| + 1` turns — every assistant turn from the delivered note back to
/// (and including) the answer's, with the user turns wedged between them.
/// Reaching the first note from the fifth reads nine: five assistant turns and
/// four user turns (the user's own convention; the strictly-between count of
/// seven is the other defensible one and is NOT what this column reports). A
/// note is zero turns from itself, which is what a directly delivered answer
/// scores.
///
/// Two notes from DIFFERENT sessions have no dialogue distance at all: there
/// is no transcript joining them, which is exactly the case the reach column
/// refuses to credit.
pub fn dialogue_distance(a: (&str, usize), b: (&str, usize)) -> Option<usize> {
    if a.0 != b.0 {
        return None;
    }
    let steps = a.1.abs_diff(b.1);
    Some(if steps == 0 { 0 } else { 2 * steps + 1 })
}

fn ratio(hits: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        hits as f64 / total as f64
    }
}

fn recall_at(outcomes: &[Outcome], k: usize) -> f64 {
    ratio(
        outcomes
            .iter()
            .filter(|o| o.rank.is_some_and(|r| r <= k))
            .count(),
        outcomes.len(),
    )
}

pub fn score(outcomes: &[Outcome]) -> Score {
    let n = outcomes.len();
    Score {
        queries: n,
        recall_at_1: recall_at(outcomes, 1),
        recall_at_3: recall_at(outcomes, 3),
        recall_at_5: recall_at(outcomes, 5),
        recall_at_10: recall_at(outcomes, 10),
        mrr: outcomes
            .iter()
            .map(|o| o.rank.map_or(0.0, |r| 1.0 / r as f64))
            .sum::<f64>()
            / n.max(1) as f64,
        tokens_mean: outcomes.iter().map(|o| o.tokens as f64).sum::<f64>() / n.max(1) as f64,
        twin_confusion: ratio(outcomes.iter().filter(|o| o.twin_above).count(), n),
        focus: {
            let delivered: Vec<f64> = outcomes.iter().filter_map(|o| o.focus).collect();
            delivered.iter().sum::<f64>() / delivered.len().max(1) as f64
        },
        noise: outcomes
            .iter()
            .map(|o| {
                if o.returned == 0 {
                    0.0
                } else {
                    (o.returned - usize::from(o.rank.is_some())) as f64 / o.returned as f64
                }
            })
            .sum::<f64>()
            / n.max(1) as f64,
        neighbor_only: ratio(
            outcomes
                .iter()
                .filter(|o| o.rank.is_none() && o.assisted_rank.is_some())
                .count(),
            n,
        ),
        history_reach: ratio(
            outcomes
                .iter()
                .filter(|o| o.history_distance.is_some())
                .count(),
            n,
        ),
        history_reach_at_5: ratio(
            outcomes
                .iter()
                .filter(|o| {
                    o.rank.is_some_and(|r| r <= 5) || o.history_rank.is_some_and(|r| r <= 5)
                })
                .count(),
            n,
        ),
        history_only: ratio(
            outcomes
                .iter()
                .filter(|o| o.rank.is_none() && o.history_distance.is_some())
                .count(),
            n,
        ),
        history_distance: {
            let walks: Vec<usize> = outcomes
                .iter()
                .filter(|o| o.rank.is_none())
                .filter_map(|o| o.history_distance)
                .collect();
            // Spelt out rather than divided by `len().max(1)`: f64's empty
            // sum is NEGATIVE zero, and a column reading `-0.0` turns "there
            // were no walks" into a typo hunt.
            if walks.is_empty() {
                0.0
            } else {
                walks.iter().sum::<usize>() as f64 / walks.len() as f64
            }
        },
    }
}

/// The same outcomes scored as the graph layer would deliver them: a fact
/// found through a neighbour counts as found.
///
/// Reported as its own arm rather than folded into `recall`, because a
/// neighbour reference carries a title and an id and nothing else — the caller
/// still has to fetch the node. It is a weaker kind of hit, and collapsing the
/// two would overstate what the graph does.
pub fn assisted(outcomes: &[Outcome]) -> Vec<Outcome> {
    outcomes
        .iter()
        .map(|o| Outcome {
            rank: o.assisted_rank,
            ..o.clone()
        })
        .collect()
}

pub fn by_phrasing(outcomes: &[Outcome]) -> Vec<(Phrasing, Score)> {
    PHRASINGS
        .iter()
        .map(|p| {
            let subset: Vec<Outcome> = outcomes
                .iter()
                .filter(|o| o.phrasing == *p)
                .cloned()
                .collect();
            (*p, score(&subset))
        })
        .collect()
}

/// How often each phrasing actually occurs in the questions a memory gets
/// asked. A **stated assumption**, and the single most decision-changing one
/// in the harness: the per-phrasing scores differ so much that the weighting
/// picks the winner. Measured uniformly (the default before this existed),
/// pure vector search beats the hybrid stack; at ten percent oblique the
/// hybrid stack wins. Neither is wrong — they answer different questions.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PhrasingMix {
    pub lexical: f64,
    pub paraphrase: f64,
    pub oblique: f64,
}

impl Default for PhrasingMix {
    /// A guess, not a measurement. Most questions name the thing they are
    /// about; only a minority describe something the asker cannot name.
    fn default() -> Self {
        Self {
            lexical: 0.45,
            paraphrase: 0.45,
            oblique: 0.10,
        }
    }
}

impl PhrasingMix {
    pub fn weight(&self, p: Phrasing) -> f64 {
        match p {
            Phrasing::Lexical => self.lexical,
            Phrasing::Paraphrase => self.paraphrase,
            Phrasing::Oblique => self.oblique,
        }
    }

    /// One headline number: recall@5 as this workload would experience it.
    pub fn weighted_recall(&self, by_phrasing: &[(Phrasing, Score)]) -> f64 {
        let total: f64 = PHRASINGS.iter().map(|p| self.weight(*p)).sum();
        if total <= 0.0 {
            return 0.0;
        }
        by_phrasing
            .iter()
            .map(|(p, s)| self.weight(*p) * s.recall_at_5)
            .sum::<f64>()
            / total
    }

    /// The oblique share at which two arms would tie — the number that says
    /// how much the assumption is doing.
    pub fn crossover(a: &[(Phrasing, Score)], b: &[(Phrasing, Score)]) -> Option<f64> {
        let at = |set: &[(Phrasing, Score)], p: Phrasing| {
            set.iter()
                .find(|(q, _)| *q == p)
                .map(|(_, s)| s.recall_at_5)
        };
        let named = |set: &[(Phrasing, Score)]| {
            Some((at(set, Phrasing::Lexical)? + at(set, Phrasing::Paraphrase)?) / 2.0)
        };
        let (na, nb) = (named(a)?, named(b)?);
        let (oa, ob) = (at(a, Phrasing::Oblique)?, at(b, Phrasing::Oblique)?);
        // na*(1-w) + oa*w == nb*(1-w) + ob*w
        let denom = (ob - oa) - (nb - na);
        if denom.abs() < 1e-9 {
            return None;
        }
        let w = (na - nb) / denom;
        (0.0..=1.0).contains(&w).then_some(w)
    }
}

/// Can any confidence threshold tell "we know this" from "we have never heard
/// of it"? A retriever that cannot is one that will happily furnish a
/// confident answer to a question it has no memory of.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Separation {
    /// Fraction of unanswerable questions that returned anything at all.
    pub false_positive_rate: f64,
    pub answerable_mean_score: f64,
    pub unanswerable_mean_score: f64,
    /// The threshold that best splits the two populations, and how well.
    pub best_threshold: f64,
    pub balanced_accuracy: f64,
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

pub fn separation(answerable: &[Option<f64>], unanswerable: &[Option<f64>]) -> Separation {
    // No hit at all is the correct response to an unknown question, so it
    // scores zero confidence rather than being dropped.
    let pos: Vec<f64> = answerable.iter().map(|s| s.unwrap_or(0.0)).collect();
    let neg: Vec<f64> = unanswerable.iter().map(|s| s.unwrap_or(0.0)).collect();

    let mut candidates: Vec<f64> = pos.iter().chain(neg.iter()).copied().collect();
    candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    candidates.dedup();

    let mut best = (0.0, 0.0);
    for &t in &candidates {
        let tpr = ratio(pos.iter().filter(|s| **s >= t).count(), pos.len());
        let tnr = ratio(neg.iter().filter(|s| **s < t).count(), neg.len());
        let bal = (tpr + tnr) / 2.0;
        if bal > best.1 {
            best = (t, bal);
        }
    }

    Separation {
        false_positive_rate: ratio(
            unanswerable.iter().filter(|s| s.is_some()).count(),
            unanswerable.len(),
        ),
        answerable_mean_score: mean(&pos),
        unanswerable_mean_score: mean(&neg),
        best_threshold: best.0,
        balanced_accuracy: best.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(rank: Option<usize>) -> Outcome {
        Outcome {
            phrasing: Phrasing::Lexical,
            rank,
            assisted_rank: rank,
            tokens: 100,
            top_score: rank.map(|_| 0.9),
            twin_above: false,
            focus: rank.map(|_| 0.25),
            returned: 10,
            history_distance: None,
            history_rank: None,
        }
    }

    #[test]
    fn noise_counts_misses_in_full_and_rewards_silence() {
        // Found among ten: nine of the ten delivered records were noise. A
        // miss with ten results delivered nothing but noise. An empty return
        // told no lies — the only outcome noise scores at zero.
        let s = score(&[outcome(Some(3)), outcome(None)]);
        assert!((s.noise - 0.95).abs() < 1e-9, "(0.9 + 1.0) / 2");
        let silent = Outcome {
            returned: 0,
            ..outcome(None)
        };
        assert_eq!(score(std::slice::from_ref(&silent)).noise, 0.0);
    }

    #[test]
    fn focus_averages_only_over_delivered_answers() {
        // A miss has no focus to report — averaging a zero into the column
        // would double-punish the miss recall already counts.
        let s = score(&[outcome(Some(1)), outcome(Some(2)), outcome(None)]);
        assert!((s.focus - 0.25).abs() < 1e-9);
        assert_eq!(score(&[outcome(None)]).focus, 0.0);
    }

    #[test]
    fn recall_and_mrr_count_ranks_correctly() {
        let s = score(&[
            outcome(Some(1)),
            outcome(Some(4)),
            outcome(None),
            outcome(Some(2)),
        ]);
        assert_eq!(s.queries, 4);
        assert_eq!(s.recall_at_1, 0.25);
        assert_eq!(s.recall_at_3, 0.5);
        assert_eq!(s.recall_at_5, 0.75);
        // 1/1 + 1/4 + 0 + 1/2 = 1.75 over 4
        assert!((s.mrr - 0.4375).abs() < 1e-9);
        assert_eq!(s.tokens_mean, 100.0);
    }

    #[test]
    fn empty_input_does_not_divide_by_zero() {
        let s = score(&[]);
        assert_eq!(s.queries, 0);
        assert_eq!(s.mrr, 0.0);
        assert_eq!(s.recall_at_1, 0.0);
    }

    #[test]
    fn perfectly_separated_populations_score_one() {
        let sep = separation(
            &[Some(0.9), Some(0.8), Some(0.85)],
            &[None, Some(0.1), None],
        );
        assert!((sep.balanced_accuracy - 1.0).abs() < 1e-9);
        assert!(sep.best_threshold > 0.1 && sep.best_threshold <= 0.8);
        assert!((sep.false_positive_rate - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn overlapping_populations_cannot_be_separated() {
        let sep = separation(&[Some(0.5), Some(0.5)], &[Some(0.5), Some(0.5)]);
        assert!(
            sep.balanced_accuracy <= 0.5001,
            "identical scores must not look separable: {}",
            sep.balanced_accuracy
        );
    }

    #[test]
    fn the_graph_layer_only_ever_adds() {
        // A neighbour can rescue a miss; it must never cost a direct hit.
        let outcomes = vec![
            Outcome {
                rank: None,
                assisted_rank: Some(2),
                ..outcome(None)
            },
            outcome(Some(1)),
        ];
        let direct = score(&outcomes);
        let assisted = score(&assisted(&outcomes));
        assert_eq!(direct.recall_at_5, 0.5);
        assert_eq!(assisted.recall_at_5, 1.0);
        assert_eq!(
            direct.neighbor_only, 0.5,
            "one of two came only via a neighbour"
        );
        assert!(assisted.mrr >= direct.mrr);
    }

    fn set(lex: f64, para: f64, obliq: f64) -> Vec<(Phrasing, Score)> {
        vec![
            (
                Phrasing::Lexical,
                Score {
                    recall_at_5: lex,
                    ..Default::default()
                },
            ),
            (
                Phrasing::Paraphrase,
                Score {
                    recall_at_5: para,
                    ..Default::default()
                },
            ),
            (
                Phrasing::Oblique,
                Score {
                    recall_at_5: obliq,
                    ..Default::default()
                },
            ),
        ]
    }

    #[test]
    fn the_weighting_decides_which_arm_wins() {
        // The measured 1000-node numbers. Under a uniform mix pure vectors
        // win; under a realistic one the hybrid stack wins. Same data.
        let engram = set(1.00, 1.00, 0.13);
        let rag = set(0.99, 0.94, 0.32);

        let uniform = PhrasingMix {
            lexical: 1.0,
            paraphrase: 1.0,
            oblique: 1.0,
        };
        assert!(
            uniform.weighted_recall(&rag) > uniform.weighted_recall(&engram),
            "asking oblique a third of the time favours pure vectors"
        );

        let realistic = PhrasingMix::default();
        assert!(
            realistic.weighted_recall(&engram) > realistic.weighted_recall(&rag),
            "at ten percent oblique the hybrid stack is ahead"
        );
    }

    #[test]
    fn crossover_reports_where_the_assumption_flips() {
        let w = PhrasingMix::crossover(&set(1.00, 1.00, 0.13), &set(0.99, 0.94, 0.32))
            .expect("these two arms do cross");
        assert!(
            (0.10..0.25).contains(&w),
            "crossover at {w}, expected around 16% oblique"
        );
        // Arms that never cross report nothing rather than a bogus number.
        assert!(PhrasingMix::crossover(&set(1.0, 1.0, 1.0), &set(0.5, 0.5, 0.5)).is_none());
    }

    #[test]
    fn dialogue_distance_counts_the_turns_between_two_notes() {
        // A note is zero turns from itself — the answer was delivered.
        assert_eq!(dialogue_distance(("h0000", 3), ("h0000", 3)), Some(0));
        // The fifth note against the first: five assistant turns including
        // both endpoints, with four user turns wedged between them, and
        // symmetric.
        assert_eq!(dialogue_distance(("h0000", 1), ("h0000", 5)), Some(9));
        assert_eq!(dialogue_distance(("h0000", 5), ("h0000", 1)), Some(9));
        // Adjacent notes: two assistant turns and the user turn between.
        assert_eq!(dialogue_distance(("h0000", 2), ("h0000", 3)), Some(3));
        // Different transcripts do not connect at any distance.
        assert_eq!(dialogue_distance(("h0000", 1), ("h0001", 1)), None);
    }

    #[test]
    fn the_history_columns_price_the_walk_and_stay_silent_without_one() {
        let walked = |rank: Option<usize>, d: Option<usize>| Outcome {
            history_distance: d,
            // The session-mate sits at rank 7 unless the answer itself was
            // delivered — so reach@5 credits only the direct hits here.
            history_rank: d.map(|dd| if dd == 0 { rank.unwrap_or(1) } else { 7 }),
            ..outcome(rank)
        };
        // Delivered directly (0), reached across nine turns, reached across
        // one, and never reached at all.
        let s = score(&[
            walked(Some(2), Some(0)),
            walked(None, Some(9)),
            walked(None, Some(1)),
            walked(None, None),
        ]);
        assert_eq!(s.history_reach, 0.75, "three of four were reachable");
        assert_eq!(s.history_only, 0.5, "two of four ONLY through the walk");
        assert_eq!(s.history_distance, 5.0, "(9 + 1) / 2 over the walked ones");
        assert_eq!(
            s.history_reach_at_5, 0.25,
            "at depth five only the direct hit counts — the session-mates sat at rank 7"
        );

        // A corpus with no sessions must leave every column at zero rather
        // than reporting a walk of length nothing.
        let silent = score(&[outcome(Some(1)), outcome(None)]);
        assert_eq!(silent.history_reach, 0.0);
        assert_eq!(silent.history_only, 0.0);
        assert_eq!(silent.history_distance, 0.0);
    }

    #[test]
    fn phrasing_split_covers_every_phrasing() {
        let outcomes = vec![
            Outcome {
                phrasing: Phrasing::Oblique,
                ..outcome(Some(1))
            },
            outcome(Some(3)),
        ];
        let split = by_phrasing(&outcomes);
        assert_eq!(split.len(), 3);
        let oblique = split.iter().find(|(p, _)| *p == Phrasing::Oblique).unwrap();
        assert_eq!(oblique.1.queries, 1);
        assert_eq!(oblique.1.recall_at_1, 1.0);
    }
}
