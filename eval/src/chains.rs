//! The supersession-chain bench: ADR-shaped history, measured.
//!
//! Every decision here gets re-decided: N generations of the same subject,
//! each `replaces`-ing the last, exactly the shape a real Architecture
//! Decision Record trail has. The regular suite cannot ask about this — its
//! corpus bans state-mutating verbs so recall stays interpretable — and this
//! mode exists to ask precisely the three questions those verbs are for:
//!
//! 1. **current** — asked about the subject, does the LIVE head come back,
//!    and does a retired generation ever arrive beside it? (The
//!    "contradiction corpus pollutes retrieval" claim, measured.)
//! 2. **history** — is every retired generation gone from search (even when
//!    queried with its own title verbatim) while staying reachable through
//!    the `replaces` chain and fetchable by id? Retirement is not removal,
//!    and both halves have to hold at once.
//! 3. **flat ablation** — the same graph without the supersession edges:
//!    every generation live, only recency and ranking to pick the head.
//!    What archiving buys is the difference between these two tables.

use serde::Serialize;

use crate::arms::{Arm, CuratedFileArm, Delivery, EngramArm, GrepArm, RagArm, WholeFileArm};
use crate::generate::{Chain, Corpus, corpus_chained};
use crate::run::{Config, DEFAULT_CURATED_BUDGET, embedder, reranker};

/// One arm's showing over the chain questions.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ChainArmScore {
    pub questions: usize,
    /// The live head among the first 1 / 5 delivered.
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    /// Share of questions where a RETIRED generation of the question's own
    /// chain was delivered at all — the pollution number.
    pub pollution: f64,
    /// Ranked arms: where any of the chain's generations were delivered, how
    /// often the head outranked every retired sibling. Dump arms (a file in
    /// context) carry no ordering, so for them this is the share of questions
    /// answered UNAMBIGUOUSLY — the head present with no retired generation
    /// beside it, which a file that holds the whole history can never do.
    pub head_first: f64,
    pub tokens_mean: f64,
}

/// A named baseline row beside the two engram stores.
#[derive(Debug, Clone, Serialize)]
pub struct ChainBaseline {
    pub arm: String,
    pub score: ChainArmScore,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainsSizeReport {
    pub graph: usize,
    pub chains: usize,
    pub chain_len: usize,
    pub questions: usize,
    pub supersessions_written: usize,
    /// The product's behaviour: chains written with live `replaces` edges,
    /// every retired generation archived at write time.
    pub superseded: ChainArmScore,
    /// The ablation: identical facts, no supersession edges, everything live.
    pub flat: ChainArmScore,
    /// The stacks with no supersession concept at all: rag (pure vectors over
    /// the flat store), grep, the curated file, the whole file.
    pub baselines: Vec<ChainBaseline>,
    /// Share of chains whose every retired generation is reachable from the
    /// head by walking `replaces` edges.
    pub history_reachable: f64,
    /// Share of retired generations that search still returns when asked
    /// with the generation's own title verbatim — the strongest possible
    /// query for it. Anything above zero is an archival leak.
    pub retired_searchable: f64,
    /// Share of retired generations fetchable by id AND marked archived —
    /// retirement must not be removal.
    pub retired_fetchable: f64,
    /// Of the answered current-state questions, how often the winning hit
    /// carried a retired generation in its 1-hop neighbours — history arriving
    /// through the graph without polluting the ranking.
    pub neighbors_carry_history: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainsReport {
    pub embedder: String,
    pub reranker: String,
    pub embeddings_are_fake: bool,
    pub seed: u64,
    pub limit: usize,
    pub sizes: Vec<ChainsSizeReport>,
}

fn score_arm(arm: &dyn Arm, chains: &[Chain], limit: usize) -> ChainArmScore {
    let mut s = ChainArmScore::default();
    let (mut hit1, mut hit5, mut polluted, mut ranked_any, mut head_first) = (0, 0, 0, 0, 0);
    let mut tokens = 0usize;
    for ch in chains {
        let retired = ch.retired();
        for q in &ch.questions {
            s.questions += 1;
            let r = arm.retrieve(&q.text, limit);
            tokens += r.tokens;
            match &r.delivery {
                Delivery::Ranked(keys) => {
                    let head_rank = keys.iter().position(|k| k == ch.head());
                    if head_rank == Some(0) {
                        hit1 += 1;
                    }
                    if head_rank.is_some_and(|r| r < 5) {
                        hit5 += 1;
                    }
                    if keys.iter().any(|k| retired.contains(k)) {
                        polluted += 1;
                    }
                    let first_gen = keys.iter().find(|k| ch.keys.contains(*k));
                    if let Some(first) = first_gen {
                        ranked_any += 1;
                        if first == ch.head() {
                            head_first += 1;
                        }
                    }
                }
                // A dump has no ranking: present = delivered (both recall
                // columns), and "head-first" degrades to "unambiguous" — the
                // head there with no retired generation beside it, which a
                // file holding the whole history structurally cannot be.
                Delivery::Dump(keys) => {
                    let has_head = keys.iter().any(|k| k == ch.head());
                    let has_retired = keys.iter().any(|k| retired.contains(k));
                    if has_head {
                        hit1 += 1;
                        hit5 += 1;
                    }
                    if has_retired {
                        polluted += 1;
                    }
                    if has_head || has_retired {
                        ranked_any += 1;
                        if has_head && !has_retired {
                            head_first += 1;
                        }
                    }
                }
            }
        }
    }
    let n = s.questions.max(1) as f64;
    s.recall_at_1 = hit1 as f64 / n;
    s.recall_at_5 = hit5 as f64 / n;
    s.pollution = polluted as f64 / n;
    s.head_first = head_first as f64 / ranked_any.max(1) as f64;
    s.tokens_mean = tokens as f64 / n;
    s
}

/// Run the chain bench: one superseded store, one flat ablation store, the
/// mechanism checks in between.
pub fn run(cfg: &Config, n_chains: usize, chain_len: usize) -> anyhow::Result<ChainsReport> {
    let model = cfg.embed_model.as_deref();
    let (_, embedder_name) = embedder(model);
    let mut sizes = Vec::new();

    for &size in &cfg.sizes {
        let chains_here = n_chains.max(1);
        let c = corpus_chained(
            size,
            size * cfg.distractor_ratio,
            cfg.seed,
            &cfg.profile,
            &cfg.type_mix,
            chains_here,
            chain_len,
        );

        let arm = EngramArm::build(
            &c,
            embedder(model).0,
            if cfg.no_rerank { None } else { reranker().0 },
        )?;
        // key -> node id, for the store-level mechanism checks.
        let id_of: std::collections::HashMap<&str, &str> = arm
            .keys()
            .iter()
            .map(|(id, key)| (key.as_str(), id.as_str()))
            .collect();
        let store = arm.engine().store();

        let superseded = score_arm(&arm, &c.chains, cfg.limit);

        // History: from the head, `replaces` edges must reach every retired
        // generation — the walk `timeline` performs, done on the raw store so
        // a broken walk cannot hide behind a friendly API.
        let mut chains_reachable = 0;
        for ch in &c.chains {
            let mut reached: Vec<String> = Vec::new();
            let mut cur = id_of[ch.head()].to_string();
            loop {
                let next = store
                    .edges_out(&cur)?
                    .into_iter()
                    .find(|e| e.edge_type.as_str() == "replaces")
                    .map(|e| e.to_id);
                match next {
                    Some(id) if !reached.contains(&id) => {
                        reached.push(id.clone());
                        cur = id;
                    }
                    _ => break,
                }
            }
            let all = ch
                .retired()
                .iter()
                .all(|k| reached.iter().any(|id| id == id_of[k.as_str()]));
            if all {
                chains_reachable += 1;
            }
        }

        // Retirement: verbatim-title search must come back empty-handed for
        // every retired generation, while get-by-id still serves it, archived.
        let (mut searchable, mut fetchable, mut retired_total) = (0, 0, 0);
        for ch in &c.chains {
            for key in ch.retired() {
                retired_total += 1;
                let title = &c.fact(key).expect("chain fact exists").title;
                let r = arm.retrieve(title, cfg.limit);
                let Delivery::Ranked(keys) = &r.delivery else {
                    unreachable!("engram ranks")
                };
                if keys.iter().any(|k| k == key) {
                    searchable += 1;
                }
                if store
                    .get_node(id_of[key.as_str()])?
                    .is_some_and(|n| n.valid_until.is_some())
                {
                    fetchable += 1;
                }
            }
        }

        // Delivered history: an answered question whose winning hit carries a
        // retired generation as a 1-hop neighbour — the story arriving without
        // the ranking paying for it.
        let (mut answered, mut with_history) = (0, 0);
        for ch in &c.chains {
            for q in &ch.questions {
                let r = arm.retrieve(&q.text, cfg.limit);
                let Delivery::Ranked(keys) = &r.delivery else {
                    unreachable!("engram ranks")
                };
                if keys.iter().any(|k| k == ch.head()) {
                    answered += 1;
                    if r.neighbors.iter().any(|(k, _)| ch.retired().contains(k)) {
                        with_history += 1;
                    }
                }
            }
        }

        // The ablation: same facts, no supersession — every generation stays
        // live and the ranking is on its own.
        let mut flat_corpus: Corpus = c.clone();
        flat_corpus.chains.clear();
        let flat_arm = EngramArm::build(
            &flat_corpus,
            embedder(model).0,
            if cfg.no_rerank { None } else { reranker().0 },
        )?;
        let flat = score_arm(&flat_arm, &c.chains, cfg.limit);

        // The baselines nobody hands a supersession verb: pure vectors over
        // the flat store (a conventional RAG stack has no concept of retired),
        // keyword search over the flat file, the hand-curated file, and the
        // whole file in context. For all of them, every generation is just
        // another record.
        let rag = RagArm::new(&flat_arm, embedder(model).0);
        let grep = GrepArm::new(&c);
        let curated = CuratedFileArm::new(&c, DEFAULT_CURATED_BUDGET);
        let whole = WholeFileArm::new(&c);
        let baselines: Vec<ChainBaseline> = [
            ("rag", &rag as &dyn Arm),
            ("grep", &grep as &dyn Arm),
            ("curated-file", &curated as &dyn Arm),
            ("whole-file", &whole as &dyn Arm),
        ]
        .into_iter()
        .map(|(name, arm)| ChainBaseline {
            arm: name.to_string(),
            score: score_arm(arm, &c.chains, cfg.limit),
        })
        .collect();

        eprintln!(
            "  {size}: {} chains x {} generations — superseded R@5 {:.2} pollution {:.2} | flat R@5 {:.2} pollution {:.2}",
            chains_here,
            chain_len,
            superseded.recall_at_5,
            superseded.pollution,
            flat.recall_at_5,
            flat.pollution,
        );

        sizes.push(ChainsSizeReport {
            graph: c.facts.len(),
            chains: chains_here,
            chain_len,
            questions: superseded.questions,
            supersessions_written: arm.supersessions_written,
            superseded,
            flat,
            baselines,
            history_reachable: chains_reachable as f64 / c.chains.len().max(1) as f64,
            retired_searchable: searchable as f64 / retired_total.max(1) as f64,
            retired_fetchable: fetchable as f64 / retired_total.max(1) as f64,
            neighbors_carry_history: with_history as f64 / answered.max(1) as f64,
        });
    }

    Ok(ChainsReport {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::DEFAULT_TYPE_MIX;
    use crate::metrics::PhrasingMix;
    use crate::profile::Profile;

    fn tiny_cfg() -> Config {
        Config {
            sizes: vec![30],
            distractor_ratio: 2,
            type_mix: DEFAULT_TYPE_MIX.to_vec(),
            profile: Profile::default(),
            seed: 7,
            limit: 10,
            nli_budget: 0,
            no_rerank: true,
            flat_priors: false,
            phrasing: PhrasingMix::default(),
            embed_model: None,
            curated_budgets: vec![],
            rerank_full: false,
        }
    }

    #[test]
    fn supersession_retires_from_search_but_never_from_the_story() {
        let r = run(&tiny_cfg(), 4, 3).unwrap();
        let s = &r.sizes[0];
        assert_eq!(s.chains, 4);
        assert_eq!(s.questions, 12);
        assert_eq!(s.supersessions_written, 8, "two replaces edges per chain");
        // The two halves of "retired means retired": gone from search even
        // when asked with its own title, still there by id and by link.
        assert_eq!(s.retired_searchable, 0.0, "an archived generation ranked");
        assert_eq!(s.retired_fetchable, 1.0, "retirement must not be removal");
        assert_eq!(s.history_reachable, 1.0, "the replaces chain broke");
        // Supersession removes the retired side from the corpus retrieval
        // sees, so pollution is structural, not probabilistic.
        assert_eq!(s.superseded.pollution, 0.0);
        // The whole file holds every generation of every chain: it always
        // "finds" the head, always delivers the retired history beside it,
        // and can never present an unambiguous current answer.
        let whole = s
            .baselines
            .iter()
            .find(|b| b.arm == "whole-file")
            .expect("whole-file baseline runs");
        assert_eq!(whole.score.recall_at_1, 1.0);
        assert_eq!(whole.score.pollution, 1.0);
        assert_eq!(whole.score.head_first, 0.0);
        // The lexical path works under the fake embedder, so the head is
        // findable at all — this is a harness check, not a quality claim.
        assert!(s.superseded.recall_at_5 > 0.0);
    }

    #[test]
    fn the_flat_ablation_actually_leaves_every_generation_live() {
        // Without supersession the retired generations are ordinary notes:
        // lexically identical competitors to the head. The lexical question
        // quotes subject and parameter, which every generation shares, so at
        // least some questions must deliver an older generation — if none do,
        // the ablation arm quietly wrote the replaces edges after all.
        let r = run(&tiny_cfg(), 4, 3).unwrap();
        let s = &r.sizes[0];
        assert!(
            s.flat.pollution > 0.0,
            "no older generation ever delivered — is the flat store really flat?"
        );
    }
}
