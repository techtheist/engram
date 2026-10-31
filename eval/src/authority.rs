//! The authority bench (0.9.10): do endorsements move ranking?
//!
//! People endorse notes: confirm ("still true"), approve, pin. The trust
//! ladder records it, and the question is whether search listens. Each case
//! is a pair of TWINS written into a realistic background graph: the same
//! body and the same subject, with a different value in the title ("…retry
//! budget is 3 attempts" vs "…is 5 attempts"). One twin carries the
//! endorsement the scenario names, and the question names only the subject.
//! When both twins read the same to a retriever, the endorsement is the only
//! thing that should break the tie.
//!
//! Some scenarios also hand the unendorsed twin a ranking edge that is not
//! authority: a fresher capture stamp (`vs_fresh`) or a type with a higher
//! rank prior (`vs_kind`). An endorsement that loses to those is decoration.
//!
//! Every scenario is scored under each ranking variant, so one run prices the
//! 0.9.10 levers side by side against the shipped stack. The engram-only
//! shape of the metric is deliberate: no other arm has an endorsement
//! concept to measure.
//!
//! The same shape was first measured by KnowledgeDrift's authority family
//! (2026-09-14, engram Problem 00d7plcxvpjx). This mode rebuilds it inside
//! engram-eval, with its own templates, so the product repo can price a
//! change without the external bench.

use serde::Serialize;

use engram_core::{Durability, Engine, NewNode, NodeType, Source};

use crate::arms::EngramArm;
use crate::generate::corpus;
use crate::run::{Config, embedder, reranker};

/// One endorsement shape.
#[derive(Debug, Clone, Copy)]
struct Scenario {
    name: &'static str,
    /// What the winner gets.
    winner: Endorsement,
    /// What the other twin gets.
    loser: Endorsement,
    /// The other twin is captured recently; the winner two months earlier.
    loser_fresher: bool,
    /// The other twin is the type with the higher rank prior.
    loser_kind: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Endorsement {
    None,
    Confirm,
    Approve,
    Pin,
}

const SCENARIOS: &[Scenario] = &[
    // The control: nothing to break the tie — should sit near 0.5.
    sc(
        "control",
        Endorsement::None,
        Endorsement::None,
        false,
        false,
    ),
    sc(
        "confirm_vs_none",
        Endorsement::Confirm,
        Endorsement::None,
        false,
        false,
    ),
    sc(
        "approve_vs_none",
        Endorsement::Approve,
        Endorsement::None,
        false,
        false,
    ),
    sc(
        "pin_vs_none",
        Endorsement::Pin,
        Endorsement::None,
        false,
        false,
    ),
    sc(
        "approve_vs_confirm",
        Endorsement::Approve,
        Endorsement::Confirm,
        false,
        false,
    ),
    sc(
        "pin_vs_approve",
        Endorsement::Pin,
        Endorsement::Approve,
        false,
        false,
    ),
    sc(
        "confirm_vs_fresh",
        Endorsement::Confirm,
        Endorsement::None,
        true,
        false,
    ),
    sc(
        "approve_vs_fresh",
        Endorsement::Approve,
        Endorsement::None,
        true,
        false,
    ),
    sc(
        "confirm_vs_kind",
        Endorsement::Confirm,
        Endorsement::None,
        false,
        true,
    ),
    sc(
        "approve_vs_kind",
        Endorsement::Approve,
        Endorsement::None,
        false,
        true,
    ),
    sc(
        "pin_vs_fresh_kind",
        Endorsement::Pin,
        Endorsement::None,
        true,
        true,
    ),
    // The endorsement clock: both twins on the same rung, the winner
    // endorsed LAST — the tie trust cannot break (KnowledgeDrift
    // latest_confirm / latest_approve / latest_pin, engram Insight
    // 00d7plf0wpjx).
    sc(
        "latest_confirm",
        Endorsement::Confirm,
        Endorsement::Confirm,
        false,
        false,
    ),
    sc(
        "latest_approve",
        Endorsement::Approve,
        Endorsement::Approve,
        false,
        false,
    ),
    sc(
        "latest_pin",
        Endorsement::Pin,
        Endorsement::Pin,
        false,
        false,
    ),
];

const fn sc(
    name: &'static str,
    winner: Endorsement,
    loser: Endorsement,
    loser_fresher: bool,
    loser_kind: bool,
) -> Scenario {
    Scenario {
        name,
        winner,
        loser,
        loser_fresher,
        loser_kind,
    }
}

const COMPONENTS: &[&str] = &[
    "payments service",
    "auth gateway",
    "search indexer",
    "billing worker",
    "notification relay",
    "session store",
    "image pipeline",
    "report scheduler",
];

/// (property, two values) — the twins differ only in which value they state.
const PROPERTIES: &[(&str, &str, &str)] = &[
    ("retry budget", "3 attempts", "5 attempts"),
    ("request timeout", "800 milliseconds", "2 seconds"),
    ("cache TTL", "10 minutes", "1 hour"),
    ("maximum batch size", "64 items", "256 items"),
];

/// A ranking variant: the policy with or without the twin order.
#[derive(Debug, Clone, Serialize)]
pub struct Variant {
    pub name: String,
    pub twin_order: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioScore {
    pub scenario: String,
    pub cases: usize,
    /// The endorsed twin ranked above its sibling (or the sibling was not
    /// delivered), among cases where at least one twin was delivered.
    pub endorsed_first: f64,
    /// Share of cases where neither twin was delivered at all.
    pub missed: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VariantReport {
    pub variant: Variant,
    pub scenarios: Vec<ScenarioScore>,
    /// Mean `endorsed_first` over every scenario except the control.
    pub mean_endorsed_first: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorityReport {
    pub embedder: String,
    pub reranker: String,
    pub embeddings_are_fake: bool,
    pub seed: u64,
    pub limit: usize,
    pub background: usize,
    pub cases_per_scenario: usize,
    pub variants: Vec<VariantReport>,
}

/// The variants every run prices.
pub fn default_variants() -> Vec<Variant> {
    // A third lever — the rank vote's reranker half reading the
    // trust-weighted score — measured identical to shipped in every cell
    // (2026-09-27 receipt) and was removed rather than kept as a dead knob.
    let v = |name: &str, twin_order| Variant {
        name: name.into(),
        twin_order,
    };
    vec![
        v("no twin order", None),
        v("twins@0.90", Some(0.90)),
        v("twins@0.95", Some(0.95)),
    ]
}

pub fn run(cfg: &Config) -> anyhow::Result<AuthorityReport> {
    let model = cfg.embed_model.as_deref();
    let (_, embedder_name) = embedder(model);
    let size = cfg.sizes.first().copied().unwrap_or(500);
    // The background: a regular corpus, every fact written, nothing asked.
    let background = corpus(size, 0, cfg.seed);
    let arm = EngramArm::build(
        &background,
        embedder(model).0,
        if cfg.no_rerank { None } else { reranker().0 },
    )?;
    let engine = arm.engine();
    let (high_kind, low_kind) = kinds_by_prior(engine)?;
    let shipped = engine.graph_config();

    let mut variants = Vec::new();
    for variant in default_variants() {
        let mut gc = shipped.clone();
        gc.policy.twin_trust_order = variant.twin_order;
        engine.set_graph_config(&gc)?;
        let mut scenarios = Vec::new();
        for s in SCENARIOS {
            scenarios.push(score_scenario(engine, s, &high_kind, &low_kind, cfg.limit)?);
        }
        let endorsed: Vec<f64> = scenarios
            .iter()
            .filter(|s| s.scenario != "control")
            .map(|s| s.endorsed_first)
            .collect();
        let mean = endorsed.iter().sum::<f64>() / endorsed.len().max(1) as f64;
        eprintln!("  {:<24} mean endorsed-first {mean:.2}", variant.name);
        variants.push(VariantReport {
            variant,
            scenarios,
            mean_endorsed_first: mean,
        });
    }
    engine.set_graph_config(&shipped)?;

    Ok(AuthorityReport {
        embeddings_are_fake: embedder_name.contains("(fake)"),
        embedder: embedder_name,
        reranker: if cfg.no_rerank {
            "disabled (--no-rerank)".to_string()
        } else {
            reranker().1
        },
        seed: cfg.seed,
        limit: cfg.limit,
        background: background.facts.len(),
        cases_per_scenario: COMPONENTS.len() * PROPERTIES.len(),
        variants,
    })
}

/// The two reasoning types, higher rank prior first — the `vs_kind` loser
/// gets the higher one.
fn kinds_by_prior(engine: &Engine) -> anyhow::Result<(NodeType, NodeType)> {
    let gc = engine.graph_config();
    let prior = |name: &str| {
        gc.type_def(name)
            .map(|t| t.roles.rank_prior)
            .unwrap_or_default()
    };
    let (d, i) = (NodeType::parse("Decision")?, NodeType::parse("Insight")?);
    Ok(if prior("Decision") >= prior("Insight") {
        (d, i)
    } else {
        (i, d)
    })
}

/// Write one scenario's twins, ask every question, remove the twins again.
fn score_scenario(
    engine: &Engine,
    s: &Scenario,
    high_kind: &NodeType,
    low_kind: &NodeType,
    limit: usize,
) -> anyhow::Result<ScenarioScore> {
    const DAY: i64 = 86_400;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let (mut delivered, mut first, mut cases) = (0usize, 0usize, 0usize);
    let mut written = Vec::new();
    for (ci, component) in COMPONENTS.iter().enumerate() {
        for (pi, (property, a, b)) in PROPERTIES.iter().enumerate() {
            cases += 1;
            // Alternate which value the winner states, so a value the
            // retriever happens to prefer cannot pose as authority.
            let (win_value, lose_value) = if (ci + pi) % 2 == 0 { (a, b) } else { (b, a) };
            let body = format!(
                "Set for the {component} during the hardening pass after the spring load \
                 tests; applies to production and staging alike, and the runbook links to it."
            );
            let title = |v: &str| format!("The {component}'s {property} is {v}");
            let (win_kind, lose_kind) = if s.loser_kind {
                (low_kind.clone(), high_kind.clone())
            } else {
                (high_kind.clone(), high_kind.clone())
            };
            let (win_at, lose_at) = if s.loser_fresher {
                (now - 60 * DAY, now - DAY)
            } else {
                (now - 30 * DAY, now - 30 * DAY)
            };
            let winner = write(engine, win_kind, &title(win_value), &body, win_at)?;
            let loser = write(engine, lose_kind, &title(lose_value), &body, lose_at)?;
            // The loser is endorsed first, so on a shared rung the winner
            // holds the latest endorsement; on different rungs the order is
            // moot.
            endorse(engine, &loser, s.loser)?;
            endorse(engine, &winner, s.winner)?;
            written.push(winner.clone());
            written.push(loser.clone());

            let question = format!("What is the {component}'s {property}?");
            let hits = engine.search(&question, &[], limit)?;
            let rank = |id: &str| hits.iter().position(|h| h.id == id);
            match (rank(&winner), rank(&loser)) {
                (None, None) => {}
                (Some(w), Some(l)) => {
                    delivered += 1;
                    if w < l {
                        first += 1;
                    }
                }
                (Some(_), None) => {
                    delivered += 1;
                    first += 1;
                }
                (None, Some(_)) => delivered += 1,
            }
            // Twins of this subject leave before the next scenario's arrive,
            // but within a scenario every subject is distinct.
        }
    }
    for id in written {
        engine.delete_node(&id)?;
    }
    Ok(ScenarioScore {
        scenario: s.name.to_string(),
        cases,
        endorsed_first: first as f64 / delivered.max(1) as f64,
        missed: (cases - delivered) as f64 / cases.max(1) as f64,
    })
}

fn write(
    engine: &Engine,
    kind: NodeType,
    title: &str,
    body: &str,
    created_at: i64,
) -> anyhow::Result<String> {
    let node = engine.add_node(NewNode {
        node_type: kind,
        title: title.to_string(),
        body: Some(body.to_string()),
        created_at: Some(created_at),
        durability: Durability::Stable,
        source: Source::Agent,
        session_id: Some("authority".into()),
        status: None,
        code_refs: Vec::new(),
        tags: Vec::new(),
        version: None,
        props: None,
        fields: None,
    })?;
    Ok(node.id)
}

fn endorse(engine: &Engine, id: &str, e: Endorsement) -> anyhow::Result<()> {
    match e {
        Endorsement::None => {}
        Endorsement::Confirm => {
            engine.reconfirm(id)?;
        }
        Endorsement::Approve => {
            engine.approve(id)?;
        }
        Endorsement::Pin => {
            engine.set_trust_override(id, Some(1.0))?;
        }
    }
    Ok(())
}
