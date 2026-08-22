//! The session-diversity bench (0.8.10): when one session's restatements and
//! the complementary evidence of other sessions compete for the same delivery
//! slots, what does a rank-based session demotion buy, and what does it cost
//! the questions that never needed it?
//!
//! THE PROBLEM BEING PRICED. A multi-session subject — decided in one
//! session, broken in another, explained in a third — is retrieved by ONE
//! query whose candidates are all strong matches. The cut to `limit` is
//! relevance-blind to provenance, so a session that said the same thing four
//! times can fill four slots while another session's only statement of its
//! aspect falls off the list. `policy.session_diversity_demote` charges each
//! additional same-session hit a rank demotion at the cut; this bench turns
//! that knob from a guess into a number.
//!
//! THE DESIGN. The regular corpus (the crowd, spread across a session pool
//! the way real graphs accumulate a few notes per working session) plus
//! generated clusters: one subject, one aspect fact per session — different
//! kinds, so the aspects are complementary claims, not restatements — and
//! several entailed recaps beside each aspect in the SAME session. The
//! aggregation question names the subject, so every cluster note is a strong
//! candidate and the ranking has to CHOOSE which sessions fill the slots.
//! Coverage — how many of the cluster's sessions contributed at least one
//! delivered note — is the metric; an aspect and its recaps count the same,
//! because either surfaces that session's information.
//!
//! Every row also re-asks the REGULAR tested questions: those have one gold
//! in one session, so any recall the demotion spends there is the feature's
//! real price, printed beside its gain.

use serde::Serialize;

use crate::arms::EngramArm;
use crate::generate::{Phrasing, SessionCluster, SessionSpec, corpus_sessions};
use crate::run::{Config, embedder, reranker};

/// Demotion values swept, in rank positions per repeated session. 0 is the
/// shipped-truncate reference every other row is a delta against.
const DEMOTE: [f64; 6] = [0.0, 1.0, 2.0, 3.0, 5.0, 8.0];

/// Sessions per cluster — one complementary aspect each.
const SESSIONS_PER_CLUSTER: usize = 3;
/// Same-session restatements written beside each aspect.
const RECAPS_PER_SESSION: usize = 4;
/// Sessions the regular corpus is spread across.
const SESSION_POOL: usize = 40;

#[derive(Debug, Clone, Serialize, Default)]
pub struct Row {
    /// `policy.session_diversity_demote` for this row; 0 = shipped truncate.
    pub demote: f64,
    /// Mean share of a cluster's sessions with at least one note in the top
    /// 3 / 5 / full delivered list.
    pub coverage_at_3: f64,
    pub coverage_at_5: f64,
    pub coverage_at_limit: f64,
    /// Share of clusters whose EVERY session reached the delivered list.
    pub full_coverage: f64,
    /// Mean distinct cluster sessions among delivered cluster notes.
    pub sessions_mean: f64,
    /// Mean delivered hits belonging to the asked cluster — how much of the
    /// list the subject owns either way.
    pub cluster_hits_mean: f64,
    /// The price columns: the regular single-gold questions re-asked under
    /// the same knob.
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub recall_at_5_oblique: f64,
    pub millis_per_query: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionsSizeReport {
    pub tested: usize,
    pub distractors: usize,
    pub graph_nodes: usize,
    pub clusters: usize,
    pub sessions_per_cluster: usize,
    pub recaps_per_session: usize,
    pub session_pool: usize,
    pub regular_questions: usize,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionsReport {
    pub embedder: String,
    pub reranker: String,
    pub embeddings_are_fake: bool,
    pub seed: u64,
    pub limit: usize,
    pub sizes: Vec<SessionsSizeReport>,
}

fn coverage(covered: &std::collections::HashSet<usize>, total: usize) -> f64 {
    covered.len() as f64 / total.max(1) as f64
}

/// Score one knob setting over the clusters and the regular questions.
fn score(arm: &EngramArm, clusters: &[SessionCluster], regular: &RegularSet, limit: usize) -> Row {
    use std::collections::{HashMap, HashSet};
    let started = std::time::Instant::now();
    let mut asked = 0usize;

    let (mut cov3, mut cov5, mut covl, mut full, mut distinct, mut own) =
        (0.0f64, 0.0f64, 0.0f64, 0usize, 0.0f64, 0.0f64);
    for cl in clusters {
        let member: HashMap<&str, usize> =
            cl.members.iter().map(|(k, s)| (k.as_str(), *s)).collect();
        // Errors are NOT swallowed — a failed search scoring zero is how a
        // real defect hides inside a plausible row (the 0.8.7 lesson).
        let hits = arm
            .engine()
            .search(&cl.question, &[], limit)
            .unwrap_or_else(|e| panic!("search failed under this configuration: {e}"));
        asked += 1;
        let ordinals: Vec<Option<usize>> = hits
            .iter()
            .map(|h| {
                arm.keys()
                    .get(&h.id)
                    .and_then(|k| member.get(k.as_str()).copied())
            })
            .collect();
        let covered_at =
            |k: usize| -> HashSet<usize> { ordinals.iter().take(k).flatten().copied().collect() };
        let total = cl.aspects.len();
        cov3 += coverage(&covered_at(3), total);
        cov5 += coverage(&covered_at(5), total);
        let all = covered_at(hits.len());
        covl += coverage(&all, total);
        full += usize::from(all.len() == total);
        distinct += all.len() as f64;
        own += ordinals.iter().flatten().count() as f64;
    }
    let n = clusters.len().max(1) as f64;

    // The regular questions, exactly as the ladder asks them.
    let (mut r_asked, mut hit1, mut hit5) = (0usize, 0usize, 0usize);
    let (mut ob_asked, mut ob_hit5) = (0usize, 0usize);
    for (text, phrasing, gold) in &regular.questions {
        let hits = arm
            .engine()
            .search(text, &[], limit)
            .unwrap_or_else(|e| panic!("search failed under this configuration: {e}"));
        asked += 1;
        r_asked += 1;
        let rank = hits
            .iter()
            .filter_map(|h| arm.keys().get(&h.id))
            .position(|k| k == gold);
        if rank == Some(0) {
            hit1 += 1;
        }
        let got5 = rank.is_some_and(|r| r < 5);
        if got5 {
            hit5 += 1;
        }
        if *phrasing == Phrasing::Oblique {
            ob_asked += 1;
            ob_hit5 += usize::from(got5);
        }
    }

    Row {
        coverage_at_3: cov3 / n,
        coverage_at_5: cov5 / n,
        coverage_at_limit: covl / n,
        full_coverage: full as f64 / n,
        sessions_mean: distinct / n,
        cluster_hits_mean: own / n,
        recall_at_1: hit1 as f64 / r_asked.max(1) as f64,
        recall_at_5: hit5 as f64 / r_asked.max(1) as f64,
        recall_at_5_oblique: ob_hit5 as f64 / ob_asked.max(1) as f64,
        millis_per_query: started.elapsed().as_secs_f64() * 1000.0 / asked.max(1) as f64,
        ..Default::default()
    }
}

struct RegularSet {
    questions: Vec<(String, Phrasing, String)>,
}

pub fn run(cfg: &Config) -> anyhow::Result<SessionsReport> {
    let model = cfg.embed_model.as_deref();
    let (_, embedder_name) = embedder(model);
    let (_, reranker_name) = reranker();
    let mut sizes = Vec::new();

    for &size in &cfg.sizes {
        let distractors = size * cfg.distractor_ratio;
        let spec = SessionSpec {
            clusters: (size / 20).clamp(8, 40),
            sessions_per_cluster: SESSIONS_PER_CLUSTER,
            recaps_per_session: RECAPS_PER_SESSION,
            session_pool: SESSION_POOL,
        };
        let (c, clusters) = corpus_sessions(
            size,
            distractors,
            cfg.seed,
            &cfg.profile,
            &cfg.type_mix,
            spec,
        );
        let regular = RegularSet {
            questions: c
                .facts
                .iter()
                .filter(|f| f.tested)
                .flat_map(|f| f.questions.iter())
                .filter_map(|q| {
                    q.gold
                        .as_ref()
                        .map(|g| (q.text.clone(), q.phrasing, g.clone()))
                })
                .collect(),
        };

        let arm = EngramArm::build(
            &c,
            embedder(model).0,
            if cfg.no_rerank { None } else { reranker().0 },
        )?;

        let mut rows = Vec::new();
        for &d in &DEMOTE {
            arm.tune(|p| p.session_diversity_demote = d)?;
            let mut row = score(&arm, &clusters, &regular, cfg.limit);
            row.demote = d;
            rows.push(row);
        }
        // Leave the store on the shipped default, not the last swept value.
        arm.tune(|p| p.session_diversity_demote = engram_core::policy::SESSION_DIVERSITY_DEMOTE)?;

        sizes.push(SessionsSizeReport {
            tested: size,
            distractors,
            graph_nodes: c.facts.len(),
            clusters: clusters.len(),
            sessions_per_cluster: SESSIONS_PER_CLUSTER,
            recaps_per_session: RECAPS_PER_SESSION,
            session_pool: SESSION_POOL,
            regular_questions: regular.questions.len(),
            rows,
        });
    }

    Ok(SessionsReport {
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

/// Human-readable report: the coverage columns are the gain, the recall
/// columns are the price, and a knob only ships if the first move without
/// the second.
pub fn print(r: &SessionsReport) {
    println!("\n=== session-diversity bench: who gets the delivery slots? ===");
    println!(
        "embedder {} · reranker {} · seed {} · limit {}",
        r.embedder, r.reranker, r.seed, r.limit
    );
    if r.embeddings_are_fake {
        println!("WARNING: fake embeddings — these numbers measure plumbing, not retrieval.");
    }

    for s in &r.sizes {
        println!(
            "\n{} tested + {} distractors + {} clusters ({} sessions x (1 aspect + {} recaps)) = {} notes, {} regular questions",
            s.tested,
            s.distractors,
            s.clusters,
            s.sessions_per_cluster,
            s.recaps_per_session,
            s.graph_nodes,
            s.regular_questions,
        );
        println!(
            "    {:>6}  {:>6} {:>6} {:>6} {:>6}  {:>5} {:>5}  {:>6} {:>6} {:>6}  {:>7}",
            "demote",
            "cov@3",
            "cov@5",
            "cov@L",
            "full",
            "sess",
            "own",
            "R@1",
            "R@5",
            "obliq",
            "ms/q"
        );
        for row in &s.rows {
            println!(
                "    {:>6}  {:>6.3} {:>6.3} {:>6.3} {:>6.3}  {:>5.2} {:>5.2}  {:>6.3} {:>6.3} {:>6.3}  {:>7.1}{}",
                row.demote,
                row.coverage_at_3,
                row.coverage_at_5,
                row.coverage_at_limit,
                row.full_coverage,
                row.sessions_mean,
                row.cluster_hits_mean,
                row.recall_at_1,
                row.recall_at_5,
                row.recall_at_5_oblique,
                row.millis_per_query,
                if row.demote == 0.0 {
                    "  <- reference"
                } else {
                    ""
                },
            );
        }
    }
    println!(
        "\n`cov@k` is the mean share of a cluster's sessions represented in the top k.\n\
         The recall columns re-ask the regular single-gold questions under the same\n\
         knob — that is the feature's price, and it must not move."
    );
}
