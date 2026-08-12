//! Cascade bench (0.8.4): the instrument behind the history fall-through.
//!
//! Three questions, per 084_plan.md §6:
//! (a) how often does fall-through fire when the CURATED graph had the
//!     answer — the false fall-through rate (should be the posttune
//!     `answerable_warned` line, re-measured here with the section cost);
//! (b) can the history section displace a correct curated answer — by
//!     construction it can't (sectioned, never blended), so what's measured
//!     is the attention cost: how often a false fall-through drags a
//!     non-empty history section along;
//! (c) recall on queries whose answer exists ONLY in dialogue — the whole
//!     point of the layer: verdict fires (router), gold message found
//!     (pipeline), and the product of the two (end-to-end).
//!
//! The dialogue half is a second generated corpus (disjoint seed → disjoint
//! invented subjects) written as user/assistant Message nodes into a real
//! sibling history store — never into the curated graph.

use std::collections::HashMap;

use engram_core::{Durability, NewNode, NodeType, Source};
use serde::Serialize;

use crate::generate::{Phrasing, corpus_full};
use crate::run::{Config, embedder, reranker};

#[derive(Debug, Clone, Serialize)]
pub struct CascadeReport {
    pub embedder: String,
    pub reranker: String,
    pub embeddings_are_fake: bool,
    pub seed: u64,
    pub limit: usize,
    pub sizes: Vec<CascadeSizeReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CascadeSizeReport {
    /// Curated facts written (tested + distractors).
    pub graph: usize,
    /// Facts that exist only as dialogue in the history store.
    pub dialogue_facts: usize,
    pub weak_line: f64,
    /// (a) answerable curated questions asked / verdict said weak-or-none.
    pub curated_questions: usize,
    pub false_fallthrough: f64,
    /// (b) of the false fall-throughs, share where the history section came
    /// back non-empty — the attention each router mistake costs.
    pub fallthrough_noise: f64,
    /// (c) dialogue-only questions asked.
    pub history_questions: usize,
    /// Router: verdict weak-or-none on dialogue-only queries (fall-through
    /// allowed to fire). The complement is `missed_strong` — a curated
    /// verdict confident enough to suppress the one place the answer lives.
    pub fired: f64,
    pub missed_strong: f64,
    /// Pipeline: gold assistant message in the top-k history hits, given the
    /// router fired.
    pub history_recall_at_k: f64,
    /// Same, oblique phrasings only — the register that breaks retrievers.
    pub history_oblique_recall: f64,
    /// Router × pipeline: share of ALL dialogue-only questions answered.
    pub end_to_end: f64,
}

pub fn cascade(cfg: &Config) -> anyhow::Result<CascadeReport> {
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
        let mut engram = crate::arms::EngramArm::build(
            &c,
            embedder(model).0,
            if cfg.no_rerank { None } else { reranker().0 },
        )?;

        // The dialogue-only corpus: disjoint seed, no distractors of its own
        // (the curated graph IS the noise), capped so the history store stays
        // session-sized rather than corpus-sized.
        let h = corpus_full(
            size.min(40),
            0,
            cfg.seed.wrapping_add(7_777),
            &cfg.profile,
            &cfg.type_mix,
        );

        // A real sibling history store next to nothing: temp-dir tepin.
        let dir =
            std::env::temp_dir().join(format!("engram-cascade-{}-{}", std::process::id(), size));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        engram
            .engine_mut()
            .set_history_path(dir.join("history.tepin"));

        // Write each dialogue fact as one user/assistant exchange, eight
        // exchanges per session — the assistant turn carries the fact and is
        // the gold message.
        let mut gold: HashMap<String, String> = HashMap::new();
        let engine = engram.engine();
        let base = engram_core::parse_day("2026-01-05").unwrap_or(1);
        for (i, f) in h.facts.iter().enumerate() {
            let sid = format!("cascade-{}", i / 8);
            let ts = base + i as i64 * 120;
            let mk = |role: &str, text: &str, turn: u64| -> anyhow::Result<String> {
                let mut props = serde_json::Map::new();
                props.insert("role".into(), role.into());
                props.insert("turn".into(), turn.into());
                let node = engine
                    .add_history_node(NewNode {
                        node_type: NodeType::parse("Message")?,
                        title: text.chars().take(60).collect(),
                        body: Some(text.to_string()),
                        created_at: Some(ts + turn as i64),
                        durability: Durability::Stable,
                        source: if role == "user" {
                            Source::User
                        } else {
                            Source::Claude
                        },
                        session_id: Some(sid.clone()),
                        status: None,
                        code_refs: vec![],
                        tags: vec![],
                        version: None,
                        props: Some(props),
                    })?
                    .ok_or_else(|| anyhow::anyhow!("history layer closed"))?;
                Ok(node.id)
            };
            mk(
                "user",
                &format!("what do we know about {}?", f.subject),
                (i as u64 % 8) * 2,
            )?;
            let assistant = mk(
                "assistant",
                &format!("{} {}", f.title, f.body),
                (i as u64 % 8) * 2 + 1,
            )?;
            gold.insert(f.key.clone(), assistant);
        }

        // Converge auto-tune exactly like posttune — the shipped router is
        // the converged one.
        for _ in 0..16 {
            if engram.engine().auto_tune()?.is_none() {
                break;
            }
        }
        let weak_line = engram.engine().graph_config().policy.weak_evidence_top;

        let engine = engram.engine();
        let weakish = |verdict: Option<&str>| matches!(verdict, Some("weak") | Some("none"));

        // (a)+(b): the curated-answerable set.
        let mut curated_questions = 0usize;
        let mut false_ft = 0usize;
        let mut noisy_ft = 0usize;
        for q in c.questions() {
            curated_questions += 1;
            let hits = engine.search(&q.text, &[], cfg.limit)?;
            if weakish(engine.search_confidence(&hits)) {
                false_ft += 1;
                if !engine.search_history(&q.text, cfg.limit)?.is_empty() {
                    noisy_ft += 1;
                }
            }
        }

        // (c): the dialogue-only set.
        let mut history_questions = 0usize;
        let mut fired = 0usize;
        let mut strong = 0usize;
        let mut found = 0usize;
        let (mut oblique_fired, mut oblique_found) = (0usize, 0usize);
        for q in h.questions() {
            let Some(gold_key) = &q.gold else { continue };
            let Some(gold_msg) = gold.get(gold_key) else {
                continue;
            };
            history_questions += 1;
            let hits = engine.search(&q.text, &[], cfg.limit)?;
            if !weakish(engine.search_confidence(&hits)) {
                strong += 1;
                continue;
            }
            fired += 1;
            let hist = engine.search_history(&q.text, cfg.limit)?;
            let hit = hist.iter().any(|m| &m.message_id == gold_msg);
            if hit {
                found += 1;
            }
            if q.phrasing == Phrasing::Oblique {
                oblique_fired += 1;
                if hit {
                    oblique_found += 1;
                }
            }
        }

        let frac = |n: usize, d: usize| n as f64 / d.max(1) as f64;
        sizes.push(CascadeSizeReport {
            graph: c.facts.len(),
            dialogue_facts: h.facts.len(),
            weak_line,
            curated_questions,
            false_fallthrough: frac(false_ft, curated_questions),
            fallthrough_noise: frac(noisy_ft, false_ft),
            history_questions,
            fired: frac(fired, history_questions),
            missed_strong: frac(strong, history_questions),
            history_recall_at_k: frac(found, fired),
            history_oblique_recall: frac(oblique_found, oblique_fired),
            end_to_end: frac(found, history_questions),
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    Ok(CascadeReport {
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
