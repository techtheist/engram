//! Contradiction shapes — can the logic layer tell a contradiction from a
//! trap, with and without the graph around the pair? (0.9.9)
//!
//! The shapes are KnowledgeDrift v2's fourteen (github.com/techtheist/
//! knowledgedrift, `src/world.rs`), re-implemented here over this harness's
//! own corpus generator — the bench repository is not touched and does not
//! know about this module. Tiers as there: tier 1 is what similarity
//! catches, tier 2 is what a sentence-pair judge catches, tier 3 needs more
//! than the pair.
//!
//! Two changes of substance against the bench:
//!
//! * **`transitive` is bridged by an EDGE, not a note.** KnowledgeDrift plants
//!   a bridge note ("X mirrors every setting of G"); engram's own graphs say
//!   such things with edges, so here the truth about X is linked `builds-on`
//!   to the flipped claim about G. The pair's text is exactly the
//!   `coreference` negative's — two subjects, two values — so a judge can
//!   only tell them apart by reading the edge.
//! * **`bridge_collider`** (new, negative): the same edge to an unrelated
//!   fact about G. Context must not turn every linked pair into an alarm.
//!
//! Three input arms per pair (0.9.9 plan, with the user):
//!
//! * `title` — the two bare titles (what the shipped title gate reads);
//! * `full` — each title + the first sentence of its body, plus up to 7
//!   related notes per side (1-hop, then 2-hop through a neighbour) as cut
//!   titles with their edge verb;
//! * `links` — the bare titles plus 1-hop related notes only: context
//!   without the transitive hop and without bodies.
//!
//! A Laya judge reads the context as extra state keys (`premise_links`,
//! `hypothesis_links`) under the XNLI question; a three-label NLI judge reads
//! it appended to each side's text. Every pair is judged in both directions
//! and the stronger contradiction counts, as `judge_pair` does in the engine.

use std::collections::HashMap;
use std::time::Instant;

use serde::Serialize;

use crate::generate::{Corpus, Fact, Kind};

// ------------------------------------------------------------ shape builders

fn decision_parts(f: &Fact) -> Option<(String, u64, String)> {
    let rest = f.predicate.strip_prefix("uses a ")?;
    let (param, value_unit) = rest.split_once(" of ")?;
    let (value, unit) = value_unit.split_once(' ')?;
    Some((param.to_string(), value.parse().ok()?, unit.to_string()))
}

fn altered(value: u64, salt: usize) -> u64 {
    value + 1 + (salt % 40) as u64
}

fn convert(unit: &str) -> Option<(&'static str, u64)> {
    match unit {
        "seconds" => Some(("milliseconds", 1000)),
        "minutes" => Some(("seconds", 60)),
        "hours" => Some(("minutes", 60)),
        "days" => Some(("hours", 24)),
        "megabytes" => Some(("kilobytes", 1024)),
        _ => None,
    }
}

fn flip_b(subject: &str, f: &Fact, salt: usize) -> String {
    match f.kind {
        Kind::Decision => match decision_parts(f) {
            Some((_, v, unit)) => {
                format!("{subject} is configured with {} {unit}", altered(v, salt))
            }
            None => format!(
                "{subject} is configured with something other than {}",
                f.answer
            ),
        },
        Kind::Caution => format!("{subject} never {}, whatever happens", f.predicate),
        Kind::Principle => format!("{subject} may freely {}", f.answer),
        Kind::Problem => format!("{subject} no longer {}", f.predicate),
        Kind::Insight => format!(
            "It is not true that {subject} {} because {}",
            f.predicate, f.answer
        ),
    }
}

fn flip_clause(f: &Fact, salt: usize) -> String {
    match f.kind {
        Kind::Decision => match decision_parts(f) {
            Some((_, v, unit)) => format!("it is configured with {} {unit}", altered(v, salt)),
            None => format!("it is configured with something other than {}", f.answer),
        },
        Kind::Caution => format!("it never {}, whatever happens", f.predicate),
        Kind::Principle => format!("it may freely {}", f.answer),
        Kind::Problem => format!("it no longer {}", f.predicate),
        Kind::Insight => format!("it {} for a reason other than {}", f.predicate, f.answer),
    }
}

fn flip_c(f: &Fact, salt: usize) -> String {
    let s = &f.subject;
    match f.kind {
        Kind::Decision => match decision_parts(f) {
            Some((param, v, unit)) => format!(
                "Operators pinned the {s} to {} {unit} for its {param}, and the dashboard agrees",
                altered(v, salt)
            ),
            None => format!("Operators moved the {s} off {} some time ago", f.answer),
        },
        Kind::Caution => format!(
            "Nobody has ever seen the {s} {}; it has been stable in every environment",
            f.predicate
        ),
        Kind::Principle => format!(
            "Doing {} inside the {s} is fine and routinely done",
            f.answer
        ),
        Kind::Problem => format!(
            "The {s} has stopped showing that it {}; the issue is resolved",
            f.predicate
        ),
        Kind::Insight => format!(
            "The explanation for the {s} {} was traced elsewhere; {} plays no part",
            f.predicate, f.answer
        ),
    }
}

fn synonym(f: &Fact) -> Option<String> {
    let t = &f.title;
    let swapped = match f.kind {
        Kind::Decision => t
            .contains(" uses a ")
            .then(|| t.replacen(" uses a ", " keeps a ", 1)),
        Kind::Caution => t
            .contains(" when ")
            .then(|| t.replacen(" when ", " whenever ", 1)),
        Kind::Principle => t
            .contains(" must never ")
            .then(|| t.replacen(" must never ", " should never ", 1)),
        Kind::Problem => Some(format!("Confirmed: {t}")),
        Kind::Insight => t
            .contains(" because ")
            .then(|| t.replacen(" because ", " since ", 1)),
    }?;
    (swapped != *t).then_some(swapped)
}

fn agree(f: &Fact) -> String {
    let s = &f.subject;
    match f.kind {
        Kind::Decision => format!("The agreed setting for the {s} is {}.", f.answer),
        Kind::Caution => format!("In production, {s} {} without warning.", f.predicate),
        Kind::Principle => format!("It is forbidden for the {s} to {}.", f.answer),
        Kind::Problem => format!("There is an open issue where the {s} {}.", f.predicate),
        Kind::Insight => format!("The reason the {s} {} is that {}.", f.predicate, f.answer),
    }
}

/// The rotation: KnowledgeDrift v2's fourteen shapes, `bridge_collider`, and
/// the three chain shapes (0.9.9) where the truth `builds-on` an upstream
/// note about G and the planted note speaks about G: a flip of G's value
/// (`transitive_chain`, positive — the truth inherited that value), an
/// unrelated fact about G (`chain_collider`) or G restated (`chain_paraphrase`).
pub const SHAPES: [(&str, u8, bool); 18] = [
    ("value", 1, true),
    ("reworded", 1, true),
    ("negation", 2, true),
    ("paraphrase", 2, false),
    ("synonym", 2, false),
    ("unit", 2, true),
    ("clause", 2, true),
    ("coreference", 2, false),
    ("transitive", 3, true),
    ("quantifier", 2, true),
    ("collider", 2, false),
    ("compound", 3, true),
    ("unit_agree", 2, false),
    ("historical", 3, false),
    ("bridge_collider", 3, false),
    ("transitive_chain", 3, true),
    ("chain_collider", 3, false),
    ("chain_paraphrase", 3, false),
];

/// One note as the judge may see it.
#[derive(Debug, Clone, Serialize)]
pub struct NoteText {
    pub title: String,
    pub body: String,
}

/// One planted pair: the truth (premise side) against the planted note.
#[derive(Debug, Clone, Serialize)]
pub struct Case {
    pub shape: &'static str,
    pub tier: u8,
    pub positive: bool,
    pub truth: NoteText,
    pub planted: NoteText,
    /// Cut-title context lines per side, 1-hop and 2-hop.
    pub truth_links1: Vec<String>,
    pub truth_links2: Vec<String>,
    pub planted_links1: Vec<String>,
    pub planted_links2: Vec<String>,
    /// Titles the truth depends on (`builds-on`, `needs`, `because` out of
    /// it), the pair partner excluded — what the propagation rule walks.
    pub truth_deps: Vec<String>,
}

fn verb_out(verb: &str) -> &'static str {
    match verb {
        "about" => "is about",
        "because" => "holds because",
        "answers" => "answers",
        "builds-on" => "builds on",
        "replaces" => "replaces",
        "conflicts-with" => "conflicts with",
        "needs" => "needs",
        _ => "relates to",
    }
}

fn verb_in(verb: &str) -> &'static str {
    match verb {
        "about" => "is the topic of",
        "because" => "is the reason for",
        "answers" => "is answered by",
        "builds-on" => "is built on by",
        "replaces" => "is replaced by",
        "conflicts-with" => "conflicts with",
        "needs" => "is needed by",
        _ => "relates to",
    }
}

/// A related note's title as context: its first clause, at most 12 words.
pub fn cut_title(t: &str) -> String {
    let c = engram_core::title_clause(t.trim());
    let words: Vec<&str> = c.split_whitespace().collect();
    if words.len() <= 12 {
        c.trim_end_matches('.').to_string()
    } else {
        words[..12].join(" ") + "…"
    }
}

/// The first sentence of a body, at most 160 chars.
fn body_cut(b: &str) -> String {
    let first = b.trim().replace('\n', " ");
    let first = first.split(". ").next().unwrap_or("").trim().to_string();
    if first.chars().count() <= 160 {
        first
    } else {
        first.chars().take(160).collect::<String>() + "…"
    }
}

/// Stands for the other side of the pair inside a context line.
const PARTNER: &str = "\u{1}PARTNER";

/// An adjacency list over titles: node → (edge phrase, neighbour).
#[derive(Default)]
struct Graph {
    titles: HashMap<String, String>,
    adj: HashMap<String, Vec<(String, String)>>,
}

impl Graph {
    fn add_node(&mut self, key: &str, title: &str) {
        self.titles.insert(key.to_string(), title.to_string());
    }
    fn add_edge(&mut self, from: &str, verb: &str, to: &str) {
        self.adj
            .entry(from.to_string())
            .or_default()
            .push((verb_out(verb).to_string(), to.to_string()));
        self.adj
            .entry(to.to_string())
            .or_default()
            .push((verb_in(verb).to_string(), from.to_string()));
    }
    /// A neighbour as context text. The pair partner is never quoted — its
    /// title is already the other side of the pair, and quoting it put the
    /// hypothesis inside the premise (read as entailment) — it becomes the
    /// [`PARTNER`] placeholder, resolved to "the premise"/"the hypothesis"
    /// once the judging direction is known.
    fn name(&self, key: &str, partner: &str) -> String {
        if key == partner {
            PARTNER.to_string()
        } else {
            cut_title(&self.titles[key])
        }
    }
    /// Titles `key` depends on: its outgoing builds-on / needs / because.
    fn deps(&self, key: &str, partner: &str) -> Vec<String> {
        let dep = [
            verb_out("builds-on"),
            verb_out("needs"),
            verb_out("because"),
        ];
        self.adj
            .get(key)
            .map(|v| {
                v.iter()
                    .filter(|(p, k)| dep.contains(&p.as_str()) && k != partner)
                    .map(|(_, k)| self.titles[k].clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Drop every edge between `a` and `b`, both directions.
    fn remove_edges(&mut self, a: &str, b: &str) {
        if let Some(v) = self.adj.get_mut(a) {
            v.retain(|(_, k)| k != b);
        }
        if let Some(v) = self.adj.get_mut(b) {
            v.retain(|(_, k)| k != a);
        }
    }
    /// 1-hop lines, then 2-hop lines (through a neighbour, never back).
    fn links(&self, key: &str, partner: &str) -> (Vec<String>, Vec<String>) {
        let none = Vec::new();
        let hop1 = self.adj.get(key).unwrap_or(&none);
        let l1 = hop1
            .iter()
            .map(|(p, k)| format!("{p}: {}", self.name(k, partner)))
            .collect();
        let mut l2 = Vec::new();
        for (p1, k1) in hop1 {
            for (p2, k2) in self.adj.get(k1).unwrap_or(&none) {
                if k2 == key {
                    continue;
                }
                l2.push(format!(
                    "{p1} {} which {p2}: {}",
                    self.name(k1, partner),
                    self.name(k2, partner)
                ));
            }
        }
        (l1, l2)
    }
}

/// Plant one case per tested fact: the least-planted shape that applies to
/// the fact's kind (ties broken by rotation), so narrow shapes like
/// `unit_agree` — a Decision with a convertible unit — are not starved.
pub fn plant(corpus: &Corpus) -> Vec<Case> {
    let mut g = Graph::default();
    for f in &corpus.facts {
        g.add_node(&f.key, &f.title);
    }
    for e in &corpus.edges {
        g.add_edge(&e.from, e.verb, &e.to);
    }
    let component = |f: &Fact| {
        f.subject
            .rsplit_once(' ')
            .map(|(_, c)| c.to_string())
            .unwrap_or_default()
    };
    let tested: Vec<&Fact> = corpus.facts.iter().filter(|f| f.tested).collect();
    let mut cases = Vec::new();
    let mut planted = [0usize; SHAPES.len()];
    for (n, f) in tested.iter().enumerate() {
        let comp = component(f);
        let other_subject = corpus
            .facts
            .iter()
            .find(|o| o.key != f.key && o.kind != f.kind && component(o) == comp)
            .map(|o| o.subject.clone());
        let other_predicate = corpus
            .facts
            .iter()
            .find(|o| o.key != f.key && o.kind != f.kind && component(o) != comp)
            .map(|o| o.predicate.clone());
        let salt = n + 17;
        let mut order: Vec<usize> = (0..SHAPES.len()).map(|s| (n + s) % SHAPES.len()).collect();
        order.sort_by_key(|i| planted[*i]);
        for idx in order {
            let (shape, tier, positive) = SHAPES[idx];
            let Some((title, body, bridge)) = make(
                shape,
                f,
                salt,
                other_subject.as_deref(),
                other_predicate.as_deref(),
            ) else {
                continue;
            };
            let pkey = format!("planted-{n}");
            let gkey = format!("upstream-{n}");
            g.add_node(&pkey, &title);
            match &bridge {
                Bridge::None => {}
                Bridge::Direct => g.add_edge(&f.key, "builds-on", &pkey),
                Bridge::Upstream(upstream) => {
                    g.add_node(&gkey, upstream);
                    g.add_edge(&f.key, "builds-on", &gkey);
                }
            }
            let (t1, t2) = g.links(&f.key, &pkey);
            let (p1, p2) = g.links(&pkey, &f.key);
            let truth_deps = g.deps(&f.key, &pkey);
            // Keep this case's scaffolding out of every other case's context.
            g.remove_edges(&f.key, &pkey);
            g.remove_edges(&f.key, &gkey);
            cases.push(Case {
                shape,
                tier,
                positive,
                truth: NoteText {
                    title: f.title.clone(),
                    body: f.body.clone(),
                },
                planted: NoteText { title, body },
                truth_links1: t1,
                truth_links2: t2,
                planted_links1: p1,
                planted_links2: p2,
                truth_deps,
            });
            planted[idx] += 1;
            break;
        }
    }
    cases
}

/// How a case's planted note hangs off the truth.
enum Bridge {
    /// Not linked (every shape the pair alone decides).
    None,
    /// The truth `builds-on` the planted note itself.
    Direct,
    /// The truth `builds-on` an upstream note (this title) about another
    /// subject; the planted note speaks about that subject, unlinked.
    Upstream(String),
}

/// (title, body, bridge) for one shape.
fn make(
    shape: &str,
    f: &Fact,
    salt: usize,
    other_subject: Option<&str>,
    other_predicate: Option<&str>,
) -> Option<(String, String, Bridge)> {
    let s = &f.subject;
    let parts = decision_parts(f);
    let t = |title: String, body: &str| Some((title, body.to_string(), Bridge::None));
    // The upstream note for the chain shapes: the truth's own claim, said of
    // G — "X builds on G, and G uses 7" is why X uses 7.
    let upstream = |g: &str| f.title.replacen(s.as_str(), g, 1);
    match shape {
        "value" => t(flip_b(s, f, salt), "Recorded while re-reading the runbook."),
        "negation" => t(
            format!("It is not the case that the {s} {}.", f.predicate),
            "Checked against the current deployment.",
        ),
        "paraphrase" => t(agree(f), "Restated for the onboarding notes."),
        "reworded" => t(
            flip_c(f, salt),
            "Written up fresh after the last operations review.",
        ),
        "clause" => t(
            format!(
                "Contrary to the runbook, it is wrong that the {s} {}; the earlier note was mistaken",
                f.predicate
            ),
            "Corrected after the incident that the runbook version caused.",
        ),
        "synonym" => t(
            synonym(f)?,
            "Restated word for word from the original note.",
        ),
        "unit" => {
            let (param, v, unit) = parts?;
            let (unit2, factor) = convert(&unit)?;
            t(
                format!(
                    "{s} is set to a {param} of {} {unit2}",
                    altered(v, salt) * factor
                ),
                "Converted from the ops dashboard's units.",
            )
        }
        "unit_agree" => {
            let (param, v, unit) = parts?;
            let (unit2, factor) = convert(&unit)?;
            t(
                format!("{s} keeps its {param} at {} {unit2}", v * factor),
                "Same setting, read off the dashboard in its own units.",
            )
        }
        "coreference" => t(
            flip_b(other_subject?, f, salt),
            "Noted while comparing the two.",
        ),
        "collider" => t(
            format!("{s} {}", other_predicate?),
            "Observed on the same component, unrelated to the setting above.",
        ),
        "transitive" => Some((
            flip_b(other_subject?, f, salt),
            "Captured from the sibling's runbook.".into(),
            Bridge::Direct,
        )),
        "bridge_collider" => Some((
            format!("{} {}", other_subject?, other_predicate?),
            "Observed on the sibling, unrelated to the setting above.".into(),
            Bridge::Direct,
        )),
        "transitive_chain" => {
            let g = other_subject?;
            Some((
                flip_b(g, f, salt),
                "Captured from the sibling's runbook.".into(),
                Bridge::Upstream(upstream(g)),
            ))
        }
        "chain_collider" => {
            let g = other_subject?;
            Some((
                format!("{g} {}", other_predicate?),
                "Observed on the sibling, unrelated to its settings.".into(),
                Bridge::Upstream(upstream(g)),
            ))
        }
        "chain_paraphrase" => {
            let g = other_subject?;
            let restated = synonym(f).map_or_else(
                || agree(f).replacen(s.as_str(), g, 1),
                |t| t.replacen(s.as_str(), g, 1),
            );
            Some((
                restated,
                "Restated from the sibling's runbook.".into(),
                Bridge::Upstream(upstream(g)),
            ))
        }
        "quantifier" => {
            let text = match f.kind {
                Kind::Decision => {
                    let (param, v, unit) = parts?;
                    format!(
                        "Every deployment of the {s} runs with a {param} of {} {unit}, without exception",
                        altered(v, salt)
                    )
                }
                Kind::Principle => {
                    format!("{s} is allowed to {} whenever it is convenient", f.answer)
                }
                Kind::Caution | Kind::Problem => format!(
                    "In every environment tried so far the {s} runs clean: {} has not happened once",
                    f.predicate
                ),
                Kind::Insight => format!(
                    "{s} {} for reasons that have nothing to do with {}",
                    f.predicate, f.answer
                ),
            };
            t(text, "Surveyed across every environment.")
        }
        "compound" => t(
            format!(
                "{s} {}; separately, operators report that {}",
                f.predicate,
                flip_clause(f, salt)
            ),
            "Two observations from one incident review.",
        ),
        "historical" => t(
            format!(
                "Until the 3.1 rollout, {}; the rollout changed that, and the current note stands",
                flip_b(s, f, salt)
            ),
            "History, kept so nobody re-litigates the rollout.",
        ),
        _ => None,
    }
}

// ------------------------------------------------------------------ judging

/// What one side of a pair shows the judge under an arm.
#[derive(Debug, Clone)]
pub struct Side {
    pub text: String,
    pub links: Vec<String>,
}

pub const ARMS: [&str; 3] = ["title", "full", "links"];
const MAX_LINKS: usize = 7;

/// `other` names the opposite side's role ("the premise"/"the hypothesis").
fn side(note: &NoteText, l1: &[String], l2: &[String], arm: &str, other: &str) -> Side {
    let mut s = side_raw(note, l1, l2, arm);
    for l in &mut s.links {
        *l = l.replace(PARTNER, other);
    }
    s
}

fn side_raw(note: &NoteText, l1: &[String], l2: &[String], arm: &str) -> Side {
    match arm {
        "title" => Side {
            text: note.title.clone(),
            links: Vec::new(),
        },
        "full" => {
            let b = body_cut(&note.body);
            let text = if b.is_empty() {
                note.title.clone()
            } else {
                format!("{}. {b}", note.title.trim_end_matches('.'))
            };
            Side {
                text,
                links: l1.iter().chain(l2).take(MAX_LINKS).cloned().collect(),
            }
        }
        _ => Side {
            text: note.title.clone(),
            links: l1.iter().take(MAX_LINKS).cloned().collect(),
        },
    }
}

/// A judge: P(contradiction) for (premise side, hypothesis side) rows.
pub trait Judge {
    fn contradiction(&self, rows: &[(Side, Side)]) -> anyhow::Result<Vec<f32>>;
}

/// Any three-label NLI: context is appended to each side's text.
pub struct NliJudge(pub Box<dyn engram_core::Nli>);

fn flatten(s: &Side) -> String {
    if s.links.is_empty() {
        s.text.clone()
    } else {
        format!("{} (Related: {}.)", s.text, s.links.join("; "))
    }
}

impl Judge for NliJudge {
    fn contradiction(&self, rows: &[(Side, Side)]) -> anyhow::Result<Vec<f32>> {
        let pairs: Vec<(String, String)> =
            rows.iter().map(|(a, b)| (flatten(a), flatten(b))).collect();
        Ok(self
            .0
            .judge(&pairs)?
            .into_iter()
            .map(|j| j.contradiction)
            .collect())
    }
}

/// Laya: context as extra state keys under the XNLI question.
#[cfg(feature = "fastembed")]
pub struct LayaJudge {
    pub model: engram_core::laya::LayaModel,
    pub question: engram_core::laya::Question,
}

#[cfg(feature = "fastembed")]
impl Judge for LayaJudge {
    fn contradiction(&self, rows: &[(Side, Side)]) -> anyhow::Result<Vec<f32>> {
        use engram_core::laya::{StateValue, state_json};
        let states: Vec<(String, &engram_core::laya::Question)> = rows
            .iter()
            .map(|(a, b)| {
                let mut fields = vec![
                    ("premise", StateValue::Text(a.text.clone())),
                    ("hypothesis", StateValue::Text(b.text.clone())),
                ];
                if !a.links.is_empty() {
                    fields.push(("premise_links", StateValue::List(a.links.clone())));
                }
                if !b.links.is_empty() {
                    fields.push(("hypothesis_links", StateValue::List(b.links.clone())));
                }
                (state_json(&fields), &self.question)
            })
            .collect();
        Ok(self
            .model
            .decide(&states)?
            .into_iter()
            .map(|p| p[2])
            .collect())
    }
}

/// Load the judge `ENGRAM_NLI_DIR` (or the default model dir) holds.
#[cfg(feature = "fastembed")]
pub fn load_judge() -> anyhow::Result<(Box<dyn Judge>, String)> {
    let dir = engram_core::nli::nli_model_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let name = dir
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    if engram_core::laya::is_laya_dir(&dir) {
        Ok((
            Box::new(LayaJudge {
                model: engram_core::laya::LayaModel::from_dir(&dir)?,
                question: engram_core::laya::nli_question(),
            }),
            name,
        ))
    } else {
        Ok((
            Box::new(NliJudge(Box::new(engram_core::FastNli::from_dir(&dir)?))),
            name,
        ))
    }
}

// ------------------------------------------------------------------ scoring

#[derive(Debug, Clone, Serialize)]
pub struct ShapeRow {
    pub shape: &'static str,
    pub tier: u8,
    pub positive: bool,
    pub n: usize,
    pub mean: f64,
    /// Share on the right side of the gate: ≥ gate for positives, < gate
    /// for negatives.
    pub pass_50: f64,
    pub pass_80: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArmReport {
    pub arm: &'static str,
    pub shapes: Vec<ShapeRow>,
    pub recall_50: f64,
    pub false_alarm_50: f64,
    pub recall_80: f64,
    pub false_alarm_80: f64,
    /// Separation of positives from negatives over every case.
    pub auroc: f64,
    /// Tier-3 positives (transitive, compound) recall at 0.5.
    pub tier3_recall_50: f64,
    pub ms_per_call: f64,
    pub mean_state_chars: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShapesReport {
    pub model: String,
    pub cases: usize,
    pub seed: u64,
    pub arms: Vec<ArmReport>,
    /// Every case's score per arm, for re-analysis.
    pub scores: Vec<CaseScore>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseScore {
    pub shape: &'static str,
    pub positive: bool,
    pub truth: String,
    pub planted: String,
    pub title: f32,
    pub full: f32,
    pub links: f32,
    /// Whether the shipped subject guard admits the pair at all.
    pub admitted: bool,
    /// The propagation rule's score: the strongest contradiction between
    /// the planted note and an admitted upstream the truth depends on.
    pub inherited: f32,
}

fn auroc(pos: &[f32], neg: &[f32]) -> f64 {
    if pos.is_empty() || neg.is_empty() {
        return f64::NAN;
    }
    let mut wins = 0.0;
    for p in pos {
        for n in neg {
            wins += if p > n {
                1.0
            } else if p == n {
                0.5
            } else {
                0.0
            };
        }
    }
    wins / (pos.len() * neg.len()) as f64
}

fn arm_report(
    arm: &'static str,
    cases: &[Case],
    scores: &[f32],
    ms_per_call: f64,
    mean_state_chars: f64,
) -> ArmReport {
    let mut shapes = Vec::new();
    for (shape, tier, positive) in SHAPES {
        let s: Vec<f32> = cases
            .iter()
            .zip(scores)
            .filter(|(c, _)| c.shape == shape)
            .map(|(_, s)| *s)
            .collect();
        if s.is_empty() {
            continue;
        }
        let pass =
            |g: f32| s.iter().filter(|x| (**x >= g) == positive).count() as f64 / s.len() as f64;
        shapes.push(ShapeRow {
            shape,
            tier,
            positive,
            n: s.len(),
            mean: s.iter().map(|x| f64::from(*x)).sum::<f64>() / s.len() as f64,
            pass_50: pass(0.5),
            pass_80: pass(0.8),
        });
    }
    let pick = |f: &dyn Fn(&Case) -> bool| -> Vec<f32> {
        cases
            .iter()
            .zip(scores)
            .filter(|(c, _)| f(c))
            .map(|(_, s)| *s)
            .collect()
    };
    let pos = pick(&|c| c.positive);
    let neg = pick(&|c| !c.positive);
    let t3 = pick(&|c| c.positive && c.tier == 3);
    let share =
        |v: &[f32], g: f32| v.iter().filter(|x| **x >= g).count() as f64 / v.len().max(1) as f64;
    ArmReport {
        arm,
        shapes,
        recall_50: share(&pos, 0.5),
        false_alarm_50: share(&neg, 0.5),
        recall_80: share(&pos, 0.8),
        false_alarm_80: share(&neg, 0.8),
        auroc: auroc(&pos, &neg),
        tier3_recall_50: share(&t3, 0.5),
        ms_per_call,
        mean_state_chars,
    }
}

/// Judge `rows` both ways round and keep the stronger contradiction per
/// pair (what `judge_pair` does in the engine); returns (scores, ms/call).
fn judge_both(judge: &dyn Judge, rows: Vec<(Side, Side)>) -> anyhow::Result<(Vec<f32>, f64)> {
    if rows.is_empty() {
        return Ok((Vec::new(), 0.0));
    }
    let mut both = Vec::with_capacity(rows.len() * 2);
    for (a, b) in rows {
        both.push((a.clone(), b.clone()));
        both.push((b, a));
    }
    let t0 = Instant::now();
    let raw = judge.contradiction(&both)?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0 / both.len() as f64;
    Ok((raw.chunks(2).map(|c| c[0].max(c[1])).collect(), ms))
}

fn bare(title: &str) -> Side {
    Side {
        text: title.to_string(),
        links: Vec::new(),
    }
}

pub fn run(
    cases: &[Case],
    judge: &dyn Judge,
    model: String,
    seed: u64,
) -> anyhow::Result<ShapesReport> {
    let mut per_arm: Vec<Vec<f32>> = Vec::new();
    let mut arms = Vec::new();
    for arm in ARMS {
        let mut rows = Vec::with_capacity(cases.len() * 2);
        for c in cases {
            // Each direction resolves the partner placeholder for its roles.
            let ta = |other| side(&c.truth, &c.truth_links1, &c.truth_links2, arm, other);
            let pb = |other| side(&c.planted, &c.planted_links1, &c.planted_links2, arm, other);
            rows.push((ta("the hypothesis"), pb("the premise")));
            rows.push((pb("the hypothesis"), ta("the premise")));
        }
        let chars: usize = rows
            .iter()
            .map(|(a, b)| flatten(a).len() + flatten(b).len())
            .sum();
        let t0 = Instant::now();
        let raw = judge.contradiction(&rows)?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0 / rows.len() as f64;
        let scores: Vec<f32> = raw.chunks(2).map(|c| c[0].max(c[1])).collect();
        arms.push(arm_report(
            arm,
            cases,
            &scores,
            ms,
            chars as f64 / rows.len() as f64,
        ));
        per_arm.push(scores);
    }
    let (title, full) = (&per_arm[0], &per_arm[1]);

    // The shipped admission rule: the judge only speaks on a pair whose
    // titles plausibly name one subject (`title_pair_admission`, the
    // sub-floor path's guard). A refused pair scores 0.
    let admitted: Vec<bool> = cases
        .iter()
        .map(|c| engram_core::title_pair_admission(&c.truth.title, &c.planted.title).is_some())
        .collect();
    let guard = |v: &[f32]| -> Vec<f32> {
        v.iter()
            .zip(&admitted)
            .map(|(s, ok)| if *ok { *s } else { 0.0 })
            .collect()
    };
    let guarded = guard(title);
    arms.push(arm_report("guarded", cases, &guarded, 0.0, 0.0));
    arms.push(arm_report("guarded_full", cases, &guard(full), 0.0, 0.0));
    let mean: Vec<f32> = title.iter().zip(full).map(|(a, b)| (a + b) / 2.0).collect();
    arms.push(arm_report("mix_mean", cases, &mean, 0.0, 0.0));
    arms.push(arm_report("guarded_mix", cases, &guard(&mean), 0.0, 0.0));

    // Propagation (0.9.9): a planted note B that contradicts an upstream U
    // the truth depends on (builds-on / needs / because), about U's own
    // subject, contradicts the truth by inheritance. The judge reads only
    // the admitted same-subject pair (U, B); the edge carries the rest.
    let mut prop_rows = Vec::new();
    let mut prop_owner = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        for u in &c.truth_deps {
            if engram_core::title_pair_admission(u, &c.planted.title).is_some() {
                prop_rows.push((bare(u), bare(&c.planted.title)));
                prop_owner.push(i);
            }
        }
    }
    let (prop_scores, prop_ms) = judge_both(judge, prop_rows)?;
    let mut inherited = vec![0.0f32; cases.len()];
    for (i, s) in prop_owner.iter().zip(&prop_scores) {
        inherited[*i] = inherited[*i].max(*s);
    }
    let chain: Vec<f32> = guarded
        .iter()
        .zip(&inherited)
        .map(|(g, p)| g.max(*p))
        .collect();
    arms.push(arm_report("chain", cases, &chain, prop_ms, 0.0));

    let scores = cases
        .iter()
        .enumerate()
        .map(|(i, c)| CaseScore {
            shape: c.shape,
            positive: c.positive,
            truth: c.truth.title.clone(),
            planted: c.planted.title.clone(),
            title: per_arm[0][i],
            full: per_arm[1][i],
            links: per_arm[2][i],
            admitted: admitted[i],
            inherited: inherited[i],
        })
        .collect();
    Ok(ShapesReport {
        model,
        cases: cases.len(),
        seed,
        arms,
        scores,
    })
}

pub fn print(r: &ShapesReport) {
    println!(
        "engram-eval — contradiction shapes   model: {}   cases: {}   seed: {}",
        r.model, r.cases, r.seed
    );
    for a in &r.arms {
        println!(
            "\n[{}]  recall@.5 {:.2}  FA@.5 {:.2}  recall@.8 {:.2}  FA@.8 {:.2}  AUROC {:.3}  tier-3 recall@.5 {:.2}   {:.1} ms/call  {:.0} chars/row",
            a.arm,
            a.recall_50,
            a.false_alarm_50,
            a.recall_80,
            a.false_alarm_80,
            a.auroc,
            a.tier3_recall_50,
            a.ms_per_call,
            a.mean_state_chars
        );
        for s in &a.shapes {
            println!(
                "  t{} {} {:16} n={:3}  mean {:.2}  pass@.5 {:.2}  pass@.8 {:.2}",
                s.tier,
                if s.positive { "+" } else { "-" },
                s.shape,
                s.n,
                s.mean,
                s.pass_50,
                s.pass_80
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_is_planted_and_transitive_mirrors_coreference_text() {
        let corpus = crate::generate::corpus(300, 150, 1);
        let cases = plant(&corpus);
        for (shape, _, _) in SHAPES {
            assert!(
                cases.iter().any(|c| c.shape == shape),
                "shape {shape} never planted"
            );
        }
        let tr = cases.iter().find(|c| c.shape == "transitive").unwrap();
        // The bridge is the only thing the judge can use.
        assert!(
            tr.truth_links1
                .iter()
                .any(|l| *l == format!("builds on: {PARTNER}"))
        );
        assert!(
            tr.planted_links1
                .iter()
                .any(|l| *l == format!("is built on by: {PARTNER}"))
        );
        // The partner's title never leaks into its own pair's context.
        assert!(
            !tr.truth_links1
                .iter()
                .chain(&tr.truth_links2)
                .any(|l| l.contains(&tr.planted.title))
        );
        let un = cases.iter().find(|c| c.shape == "value").unwrap();
        assert!(un.planted_links1.is_empty());
        // Chain shapes: the truth depends on an upstream about G, and the
        // subject guard admits (upstream, planted) — the pair the
        // propagation rule judges — while refusing (truth, planted).
        for c in cases.iter().filter(|c| c.shape == "transitive_chain") {
            // The truth's own corpus dependencies ride along; the upstream
            // is the one that restates the truth's claim about another subject.
            let up = c
                .truth_deps
                .iter()
                .find(|u| u.ends_with(c.truth.title.split_once(' ').unwrap().1))
                .expect("the upstream is among the dependencies");
            assert!(
                engram_core::title_pair_admission(up, &c.planted.title).is_some(),
                "{up} / {}",
                c.planted.title
            );
        }
    }
}
