//! The time-window bench (0.8.7): what does scoping a search in time cost,
//! and how deep does the candidate pool have to be before it costs nothing?
//!
//! THE PROBLEM BEING PRICED. Neither retrieval channel is date-ordered.
//! sqlite-vec ranks by cosine and FTS5 by bm25; both are blind to
//! `created_at`. So a windowed search cannot ask the index for "the best
//! in-window match" — it can only take the top-N date-blind candidates and
//! throw away the ones outside the window. When the window is narrow, most of
//! that pool is discarded, and an in-window gold that ranked 80th overall
//! never enters the pool at all. The shipped mitigation is to deepen the pool
//! by `policy.window_overfetch` when a window is present. That constant was a
//! guess; this bench is what turns it into a number.
//!
//! THE DESIGN. One store, dates spread deterministically across two years,
//! and every tested question asked TWICE: once unwindowed, once inside a
//! window that is guaranteed to contain its gold. The unwindowed row is the
//! reference — the recall a caller would get by not scoping at all — so the
//! interesting quantity is not any single row's recall but the GAP between
//! the windowed rows and it. A window that contains the answer should never
//! score worse than no window; every point of gap is recall the pool depth
//! spent.
//!
//! Because the corpus size is known, so is the depth at which the pool covers
//! the entire graph — past that point the filter is exact and the row IS the
//! ceiling a date-aware index would reach. Reporting that row is what makes
//! the rest of the ladder readable: it says how much of the gap is pool depth
//! and how much is simply the window being a harder question.

use std::collections::HashMap;

use engram_core::{SearchFilter, timespec::TimeWindow};
use serde::Serialize;

use crate::arms::EngramArm;
use crate::generate::{Corpus, Phrasing, corpus_full};
use crate::run::{Config, embedder, reranker};

/// How far back the corpus is spread. Two years of capture dates is roughly
/// what a real project's graph looks like by the time anyone wants to search
/// it by date.
const SPREAD_DAYS: u64 = 720;

/// Coprime with `SPREAD_DAYS` (720 = 2^4·3^2·5), so `i * STRIDE % SPREAD_DAYS`
/// walks every residue exactly once — a uniform, reproducible, seed-free
/// spread with no clustering for a window to get lucky on.
const STRIDE: u64 = 7919;

const DAY: i64 = 86_400;

/// Window widths measured, in days. The narrow one is where the pool is
/// expected to run out; the wide one is the check that the effect is about
/// window narrowness rather than about windowing at all.
const WIDTHS: [i64; 2] = [30, 180];

/// Pool multipliers swept. The shipped default is 8.
const OVERFETCH: [usize; 5] = [1, 2, 4, 8, 16];

#[derive(Debug, Clone, Serialize, Default)]
pub struct Row {
    /// `policy.window_overfetch` for this row; `None` = the unwindowed
    /// reference.
    pub overfetch: Option<usize>,
    /// Candidates pulled per channel before the window filter — what the
    /// multiplier actually buys.
    pub pool: usize,
    /// Does that pool cover the whole graph? When it does, the window filter
    /// is exact and this row is the ceiling.
    pub pool_covers_graph: bool,
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    /// Recall@5 per phrasing — the oblique column is the one that matters,
    /// since a query naming its subject ranks its gold near the top anyway
    /// and never needs the depth.
    pub recall_at_5_lexical: f64,
    pub recall_at_5_paraphrase: f64,
    pub recall_at_5_oblique: f64,
    /// Mean hits delivered. A narrow window naturally delivers fewer, and
    /// that is the point of asking — but it should not deliver FEWER GOLDS.
    pub delivered_mean: f64,
    pub millis_per_query: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WidthReport {
    pub width_days: i64,
    /// Mean notes inside a window of this width — how much of the graph a
    /// caller is actually scoping to.
    pub in_window_mean: f64,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowSizeReport {
    pub tested: usize,
    pub distractors: usize,
    pub graph_nodes: usize,
    pub questions: usize,
    pub spread_days: u64,
    /// The unwindowed reference every windowed row is a delta against.
    pub reference: Row,
    pub widths: Vec<WidthReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowReport {
    pub embedder: String,
    pub reranker: String,
    pub embeddings_are_fake: bool,
    pub seed: u64,
    pub limit: usize,
    pub sizes: Vec<WindowSizeReport>,
}

/// Deterministic capture date for the i-th fact, as days before now.
fn backdate_for(i: usize) -> u64 {
    (i as u64).wrapping_mul(STRIDE) % SPREAD_DAYS
}

/// The window a caller who "roughly remembers when" would ask for: the
/// `width`-day block the gold's capture date falls in, aligned to the block
/// grid so it is a window someone could actually name ("that month"), not one
/// centred on the answer.
fn window_around(now: i64, backdate_days: u64, width: i64) -> TimeWindow {
    let created = now - backdate_days as i64 * DAY;
    let day = created.div_euclid(DAY);
    let block = day.div_euclid(width) * width;
    TimeWindow {
        after: Some(block * DAY),
        before: Some((block + width) * DAY),
    }
}

fn phrasing_slot(p: Phrasing) -> usize {
    match p {
        Phrasing::Lexical => 0,
        Phrasing::Paraphrase => 1,
        Phrasing::Oblique => 2,
    }
}

/// Score one configuration over every tested question.
fn score(
    arm: &EngramArm,
    corpus: &Corpus,
    limit: usize,
    // `None` = ask unwindowed.
    width: Option<i64>,
    now: i64,
) -> Row {
    let backdate: HashMap<&str, u64> = corpus
        .facts
        .iter()
        .map(|f| (f.key.as_str(), f.backdate_days))
        .collect();

    let (mut asked, mut hit1, mut hit5, mut delivered) = (0usize, 0usize, 0usize, 0usize);
    // Indexed rather than keyed: `Phrasing` is a corpus type, and making it
    // hashable for one bench is the tail wagging the dog.
    let mut per_phrasing = [(0usize, 0usize); 3];
    let started = std::time::Instant::now();

    for f in corpus.facts.iter().filter(|f| f.tested) {
        for q in &f.questions {
            let Some(gold) = q.gold.as_deref() else {
                continue;
            };
            let filter = match width {
                Some(w) => SearchFilter {
                    window: window_around(now, backdate.get(gold).copied().unwrap_or(0), w),
                    ..Default::default()
                },
                None => SearchFilter::default(),
            };
            // Errors are NOT swallowed: a search that fails scores zero and
            // looks exactly like a search that found nothing, which is how a
            // real defect hides inside a plausible-looking row.
            let hits = arm
                .engine()
                .search_filtered(&q.text, &[], limit, &filter)
                .unwrap_or_else(|e| panic!("search failed under this configuration: {e}"));
            asked += 1;
            delivered += hits.len();
            let keys: Vec<&str> = hits
                .iter()
                .filter_map(|h| arm.keys().get(&h.id).map(String::as_str))
                .collect();
            let rank = keys.iter().position(|k| *k == gold);
            if rank == Some(0) {
                hit1 += 1;
            }
            let got5 = rank.is_some_and(|r| r < 5);
            if got5 {
                hit5 += 1;
            }
            let e = &mut per_phrasing[phrasing_slot(q.phrasing)];
            e.0 += 1;
            e.1 += usize::from(got5);
        }
    }

    let n = asked.max(1) as f64;
    let by = |p: Phrasing| {
        let (asked, hit) = per_phrasing[phrasing_slot(p)];
        hit as f64 / asked.max(1) as f64
    };
    Row {
        recall_at_1: hit1 as f64 / n,
        recall_at_5: hit5 as f64 / n,
        recall_at_5_lexical: by(Phrasing::Lexical),
        recall_at_5_paraphrase: by(Phrasing::Paraphrase),
        recall_at_5_oblique: by(Phrasing::Oblique),
        delivered_mean: delivered as f64 / n,
        millis_per_query: started.elapsed().as_secs_f64() * 1000.0 / n,
        ..Default::default()
    }
}

/// Candidates `search_hybrid` pulls per channel, mirroring what
/// `Engine::search_filtered` computes — kept here (rather than exported from
/// core) so the bench states the arithmetic it is reasoning about out loud.
fn pool_size(limit: usize, reranked: bool, overfetch: usize) -> usize {
    let fetch = if reranked {
        (limit * 3).clamp(12, 50)
    } else {
        limit
    };
    (fetch * 4).max(20) * overfetch.max(1)
}

pub fn run(cfg: &Config) -> anyhow::Result<WindowReport> {
    let model = cfg.embed_model.as_deref();
    let (_, embedder_name) = embedder(model);
    let (_, reranker_name) = reranker();
    let mut sizes = Vec::new();

    for &size in &cfg.sizes {
        let distractors = size * cfg.distractor_ratio;
        let mut c = corpus_full(size, distractors, cfg.seed, &cfg.profile, &cfg.type_mix);
        // Spread the corpus over two years. The regular corpus is wall-clock
        // independent by design (every fact is written "now"), which makes a
        // time window meaningless — so dating it is the one thing this bench
        // must change about the shared corpus, and it changes nothing else.
        for (i, f) in c.facts.iter_mut().enumerate() {
            f.backdate_days = backdate_for(i);
        }

        let reranked = !cfg.no_rerank;
        let arm = EngramArm::build(
            &c,
            embedder(model).0,
            if reranked { reranker().0 } else { None },
        )?;
        let now = engram_core::now();
        let graph_nodes = c.facts.len();

        let questions = c
            .facts
            .iter()
            .filter(|f| f.tested)
            .map(|f| f.questions.iter().filter(|q| q.gold.is_some()).count())
            .sum();

        let mut reference = score(&arm, &c, cfg.limit, None, now);
        reference.pool = pool_size(cfg.limit, reranked, 1);
        reference.pool_covers_graph = reference.pool >= graph_nodes;

        let mut widths = Vec::new();
        for &width in &WIDTHS {
            // How much of the graph a window of this width actually holds —
            // the number that decides whether the pool can afford to be
            // date-blind.
            let per_block = c
                .facts
                .iter()
                .fold(HashMap::<i64, usize>::new(), |mut m, f| {
                    let day = (now - f.backdate_days as i64 * DAY).div_euclid(DAY);
                    *m.entry(day.div_euclid(width)).or_default() += 1;
                    m
                });
            let in_window_mean =
                per_block.values().sum::<usize>() as f64 / per_block.len().max(1) as f64;

            let mut rows = Vec::new();
            for &k in &OVERFETCH {
                arm.tune(|p| p.window_overfetch = k)?;
                let mut row = score(&arm, &c, cfg.limit, Some(width), now);
                row.overfetch = Some(k);
                row.pool = pool_size(cfg.limit, reranked, k);
                row.pool_covers_graph = row.pool >= graph_nodes;
                rows.push(row);
            }
            widths.push(WidthReport {
                width_days: width,
                in_window_mean,
                rows,
            });
        }
        // Leave the arm on the shipped default rather than the last swept
        // value — a later reader of this store should see the product.
        arm.tune(|p| p.window_overfetch = engram_core::policy::SEARCH_WINDOW_OVERFETCH)?;

        sizes.push(WindowSizeReport {
            tested: size,
            distractors,
            graph_nodes,
            questions,
            spread_days: SPREAD_DAYS,
            reference,
            widths,
        });
    }

    Ok(WindowReport {
        embedder: embedder_name.to_string(),
        reranker: if cfg.no_rerank {
            "none".into()
        } else {
            reranker_name.to_string()
        },
        embeddings_are_fake: embedder(model).0.is_fake(),
        seed: cfg.seed,
        limit: cfg.limit,
        sizes,
    })
}

/// Human-readable report. The gap column is the whole story: it is what the
/// window costs against asking the same question unscoped.
pub fn print(r: &WindowReport) {
    println!("\n=== time-window bench: what pool depth does a scoped search need? ===");
    println!(
        "embedder {} · reranker {} · seed {} · limit {}",
        r.embedder, r.reranker, r.seed, r.limit
    );
    if r.embeddings_are_fake {
        println!("WARNING: fake embeddings — these numbers measure plumbing, not retrieval.");
    }

    for s in &r.sizes {
        println!(
            "\n{} tested + {} distractors = {} notes, spread over {} days, {} questions",
            s.tested, s.distractors, s.graph_nodes, s.spread_days, s.questions
        );
        println!(
            "  reference (no window): R@1 {:.3}  R@5 {:.3}  [lex {:.2} para {:.2} obliq {:.2}]  {:.1} delivered  {:.1} ms/q",
            s.reference.recall_at_1,
            s.reference.recall_at_5,
            s.reference.recall_at_5_lexical,
            s.reference.recall_at_5_paraphrase,
            s.reference.recall_at_5_oblique,
            s.reference.delivered_mean,
            s.reference.millis_per_query,
        );
        for w in &s.widths {
            println!(
                "\n  window {} days (~{:.0} of {} notes in window)",
                w.width_days, w.in_window_mean, s.graph_nodes
            );
            println!(
                "    {:>4}  {:>5} {:>7}  {:>6} {:>6} {:>6}  {:>6} {:>6} {:>6}  {:>7}",
                "over", "pool", "covers", "R@1", "R@5", "gap", "lex", "para", "obliq", "ms/q"
            );
            for row in &w.rows {
                let shipped = row.overfetch == Some(engram_core::policy::SEARCH_WINDOW_OVERFETCH);
                println!(
                    "    {:>4}  {:>5} {:>7}  {:>6.3} {:>6.3} {:>+6.3}  {:>6.2} {:>6.2} {:>6.2}  {:>7.1}{}{}",
                    row.overfetch.unwrap_or(1),
                    row.pool,
                    if row.pool_covers_graph { "yes" } else { "no" },
                    row.recall_at_1,
                    row.recall_at_5,
                    row.recall_at_5 - s.reference.recall_at_5,
                    row.recall_at_5_lexical,
                    row.recall_at_5_paraphrase,
                    row.recall_at_5_oblique,
                    row.millis_per_query,
                    if shipped { "  <- shipped" } else { "" },
                    if row.pool_covers_graph && !shipped {
                        "  (ceiling)"
                    } else {
                        ""
                    },
                );
            }
        }
    }
    println!(
        "\n`gap` is R@5 minus the unwindowed reference. A window that CONTAINS its answer\n\
         should cost nothing; a negative gap is recall spent on the pool being date-blind.\n\
         A row whose pool covers the graph is what a date-aware index would deliver."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backdates_spread_over_every_day_without_clustering() {
        let days: std::collections::HashSet<u64> =
            (0..SPREAD_DAYS as usize).map(backdate_for).collect();
        assert_eq!(
            days.len(),
            SPREAD_DAYS as usize,
            "the stride must be coprime with the spread, or windows land on gaps"
        );
    }

    #[test]
    fn the_window_contains_its_gold_without_being_centred_on_it() {
        let now = 1_786_665_600; // 2026-08-14T00:00:00Z
        for backdate in [0u64, 1, 17, 300, 719] {
            for width in WIDTHS {
                let w = window_around(now, backdate, width);
                let created = now - backdate as i64 * DAY;
                assert!(w.contains(created), "gold must be inside its own window");
                // Block-aligned, so the answer sits at an arbitrary offset
                // inside it rather than at the centre — a window a person
                // could actually name.
                assert_eq!(w.before.unwrap() - w.after.unwrap(), width * DAY);
                assert_eq!(w.after.unwrap().rem_euclid(width * DAY), 0);
            }
        }
    }

    #[test]
    fn pool_size_matches_the_engine_arithmetic() {
        // limit 5, reranked: fetch = clamp(15, 12, 50) = 15 -> 60 per channel.
        assert_eq!(pool_size(5, true, 1), 60);
        assert_eq!(pool_size(5, true, 8), 480);
        // The floor holds for tiny limits.
        assert_eq!(pool_size(1, false, 1), 20);
    }
}
