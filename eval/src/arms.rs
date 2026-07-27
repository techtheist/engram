//! The three arms under comparison.
//!
//! * `whole-file` — the honest baseline nobody admits to: put the memory file
//!   in the context and let the model read it. Perfect recall, no ranking, and
//!   it pays the full token bill on every single session.
//! * `grep` — keyword search over that same file. Cheap, and exactly as good
//!   as the query's vocabulary overlap.
//! * `engram` — `Engine::search`, the thing being measured.
//!
//! Every arm reports what it delivered AND what it cost, because cost is the
//! axis the whole design argument turns on.

use std::collections::HashMap;

use engram_core::{Embedder, Engine, NewNode, SqliteStore};

use crate::generate::{Corpus, Fact, Kind};

const STOPWORDS: [&str; 46] = [
    "the", "a", "an", "is", "are", "was", "were", "it", "its", "in", "on", "of", "for", "to",
    "and", "or", "that", "this", "what", "which", "who", "how", "did", "does", "do", "we", "our",
    "us", "you", "be", "been", "with", "from", "by", "at", "as", "if", "not", "no", "yes", "about",
    "into", "than", "then", "so", "up",
];

/// Rough token estimate. Deliberately crude and identical across arms — the
/// comparison is a ratio, and no tokenizer dependency survives contact with
/// CI build times.
pub fn tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

/// What a hit costs to show when there is no FTS snippet to quote — the same
/// 160 characters `store::excerpt` gives a keyword-less match, so the bench in
/// `variants.rs` is billed on the shipped arm's terms rather than its own.
pub fn excerpt_of(node: &engram_core::Node) -> String {
    node.body
        .as_deref()
        .unwrap_or(&node.title)
        .chars()
        .take(160)
        .collect()
}

pub enum Delivery {
    /// Ordered candidate fact keys, best first.
    Ranked(Vec<String>),
    /// The arm does no ranking: it puts a fixed body of text in context and
    /// the model reads all of it. The payload is the keys that text contains,
    /// so a fact inside it counts as delivered and one outside it counts as
    /// missing — position within the dump is not a rank and is not scored.
    ///
    /// `whole-file` carries every key. `curated-file` carries as many as fit
    /// its budget, which is the entire difference between them.
    Dump(Vec<String>),
}

pub struct Retrieval {
    pub delivery: Delivery,
    pub tokens: usize,
    /// The text the arm actually put in front of the caller, in delivery
    /// order — one entry per delivered record.
    ///
    /// This exists because `tokens` says what an arm *cost* and `delivery`
    /// says *which* facts it found, but neither says what text was shown. The
    /// online half needs exactly that, and reconstructing it from the corpus
    /// is how it previously went wrong: every ranked arm was handed full note
    /// bodies regardless of what it had actually delivered, which erased the
    /// difference between an arm that returns snippets and one that returns
    /// whole records — the difference being measured. Each arm now renders
    /// what it charged for, so the two cannot drift.
    pub rendered: Vec<String>,
    /// Confidence of the best hit, when the arm has one. Used to ask whether
    /// any threshold separates answerable from unanswerable questions.
    pub top_score: Option<f64>,
    /// Facts reachable in one hop from a returned hit, paired with the rank of
    /// the hit that carried them. This is the graph layer: a fact that never
    /// ranks can still reach the caller because something adjacent did.
    pub neighbors: Vec<(String, usize)>,
}

impl Retrieval {
    pub fn ranked(
        delivery: Delivery,
        rendered: Vec<String>,
        tokens: usize,
        top_score: Option<f64>,
    ) -> Self {
        Self {
            delivery,
            rendered,
            tokens,
            top_score,
            neighbors: Vec::new(),
        }
    }
}

pub trait Arm {
    fn name(&self) -> &'static str;
    fn retrieve(&self, query: &str, limit: usize) -> Retrieval;
    /// The tokens this arm costs every session before a single question is
    /// asked (an always-injected file; Engram's brief).
    fn standing_cost(&self) -> usize {
        0
    }
}

// ---------------------------------------------------------------- whole file

pub struct WholeFileArm {
    file: String,
    keys: Vec<String>,
    cost: usize,
}

impl WholeFileArm {
    pub fn new(corpus: &Corpus) -> Self {
        let file = corpus.flat_file();
        Self {
            cost: tokens(&file),
            keys: corpus.facts.iter().map(|f| f.key.clone()).collect(),
            file,
        }
    }
}

impl Arm for WholeFileArm {
    fn name(&self) -> &'static str {
        "whole-file"
    }

    fn retrieve(&self, _query: &str, _limit: usize) -> Retrieval {
        Retrieval::ranked(
            Delivery::Dump(self.keys.clone()),
            vec![self.file.clone()],
            self.cost,
            None,
        )
    }

    fn standing_cost(&self) -> usize {
        self.cost
    }
}

// ------------------------------------------------------------- curated file

/// The baseline a working developer actually has: a hand-maintained
/// `CLAUDE.md` that is pruned to stay readable.
///
/// This exists because `whole-file` is the wrong strawman. It dumps every note
/// ever written, and nobody does that — the honest comparison named by a
/// reviewer of this project is "a well-maintained memory file plus the prompt
/// *check my memory files and see if we can shorten them*". A curated file is
/// smaller, better organised, and always in context, and beating it is a much
/// harder claim than beating a dump.
///
/// The curation rule has to be **blind to the questions**, or the arm gets
/// oracle knowledge no human has. So it ranks by a stated human heuristic —
/// durable types first (a principle earns its line in the file; a resolved
/// problem does not), newest first within a type — and fills to a token
/// budget, exactly as somebody pruning a file to keep it readable would.
///
/// What it cannot do is hold everything: at a realistic budget the file
/// carries a small fraction of the graph, and every question about the rest is
/// simply unanswerable from it. That limit is the measurement, not a handicap.
pub struct CuratedFileArm {
    file: String,
    keys: Vec<String>,
    cost: usize,
    budget: usize,
}

/// Deterministic, order-destroying hash of a fact key.
///
/// FNV alone is NOT order-destroying for keys this short and this structured.
/// On `f0000`..`f1499` its high bits stay dominated by the second character,
/// so sorting by it sorts by `f0` before `f1` — which put every tested fact
/// on one side of the cut and silently reproduced the exact bias the hash was
/// added to remove. The splitmix64 finaliser supplies the avalanche FNV does
/// not, and the test below runs on a corpus large enough for the difference
/// to show. Seeded constants only: the harness reproduces from `--seed`.
fn scramble(key: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

impl CuratedFileArm {
    /// `budget` is in tokens — a real `CLAUDE.md` is a few thousand.
    pub fn new(corpus: &Corpus, budget: usize) -> Self {
        // A maintainer's ordering, and one that never consults the questions.
        let rank = |f: &Fact| match f.kind {
            Kind::Principle => 0,
            Kind::Decision => 1,
            Kind::Caution => 2,
            Kind::Insight => 3,
            Kind::Problem => 4,
        };
        // Within a type the tie-break must be BLIND to whether a fact is ever
        // asked about, and key order is not: the corpus writes every tested
        // fact before every distractor, so sorting by key — either direction —
        // fills the file with one population and scores an artifact. The first
        // version of this arm took the highest keys and scored exactly 0.000
        // recall, having selected 37 distractors.
        //
        // A hash is the honest model anyway: beyond type, which notes a human
        // keeps is judgment this harness cannot represent, and pretending
        // otherwise would hand the arm oracle knowledge or its opposite.
        let mut order: Vec<&Fact> = corpus.facts.iter().collect();
        order.sort_by_key(|f| (rank(f), scramble(&f.key)));

        let mut file = String::new();
        let mut keys = Vec::new();
        let mut used = 0;
        for f in order {
            // Curated entries are trimmed, not pasted whole — that is what
            // makes the file readable and is why it holds more than its byte
            // count suggests.
            let body: String = f.body.chars().take(200).collect();
            let entry = format!("## {}\n{}\n\n", f.title, body);
            let cost = tokens(&entry);
            if used + cost > budget {
                continue;
            }
            used += cost;
            file.push_str(&entry);
            keys.push(f.key.clone());
        }
        Self {
            cost: used,
            file,
            keys,
            budget,
        }
    }

    /// Facts that fit — the ceiling on anything this arm can ever answer.
    pub fn held(&self) -> usize {
        self.keys.len()
    }

    pub fn budget(&self) -> usize {
        self.budget
    }
}

impl Arm for CuratedFileArm {
    fn name(&self) -> &'static str {
        "curated-file"
    }

    fn retrieve(&self, _query: &str, _limit: usize) -> Retrieval {
        Retrieval::ranked(
            Delivery::Dump(self.keys.clone()),
            vec![self.file.clone()],
            self.cost,
            None,
        )
    }

    fn standing_cost(&self) -> usize {
        self.cost
    }
}

// --------------------------------------------------------------------- grep

pub struct GrepArm {
    records: Vec<(String, String)>,
}

impl GrepArm {
    pub fn new(corpus: &Corpus) -> Self {
        Self {
            records: corpus.records(),
        }
    }
}

fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect();
    out.sort();
    out.dedup();
    out
}

impl Arm for GrepArm {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn retrieve(&self, query: &str, limit: usize) -> Retrieval {
        let terms = terms(query);
        let mut scored: Vec<(usize, usize, &String, &String)> = self
            .records
            .iter()
            .enumerate()
            .filter_map(|(i, (key, text))| {
                let hay = text.to_lowercase();
                let hits = terms.iter().filter(|t| hay.contains(*t)).count();
                (hits > 0).then_some((hits, i, key, text))
            })
            .collect();
        // Most matched terms first; original order breaks ties, which is what
        // a human scrolling a file would get.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(limit);

        let top_score = scored
            .first()
            .map(|(hits, ..)| *hits as f64 / terms.len().max(1) as f64);
        let cost = scored.iter().map(|(_, _, _, text)| tokens(text)).sum();

        Retrieval::ranked(
            Delivery::Ranked(scored.iter().map(|(_, _, k, _)| (*k).clone()).collect()),
            // grep shows whole records — that is what makes it expensive, and
            // the online context must reflect it.
            scored
                .iter()
                .map(|(_, _, _, text)| (*text).clone())
                .collect(),
            cost,
            top_score,
        )
    }
}

// ------------------------------------------------------------------- chance

/// Returns `limit` facts picked by hashing the query. Retrieves nothing, and
/// exists to put a floor under the table: any arm scoring near this one is not
/// finding anything, it is being carried by the odds. Without it a number like
/// "0.22 recall" reads as weak retrieval when it may be no retrieval at all.
pub struct ChanceArm {
    keys: Vec<String>,
    records: Vec<(String, String)>,
}

impl ChanceArm {
    pub fn new(corpus: &Corpus) -> Self {
        let records = corpus.records();
        Self {
            keys: records.iter().map(|(k, _)| k.clone()).collect(),
            records,
        }
    }
}

impl Arm for ChanceArm {
    fn name(&self) -> &'static str {
        "chance"
    }

    fn retrieve(&self, query: &str, limit: usize) -> Retrieval {
        let seed = query
            .bytes()
            .fold(0u64, |a, b| a.wrapping_mul(0x0100_0000_01b3) ^ u64::from(b));
        let mut rng = crate::rng::Rng::new(seed);
        let mut picked: Vec<String> = Vec::new();
        while picked.len() < limit.min(self.keys.len()) {
            let k = &self.keys[rng.below(self.keys.len())];
            if !picked.contains(k) {
                picked.push(k.clone());
            }
        }
        let shown: Vec<String> = picked
            .iter()
            .filter_map(|k| self.records.iter().find(|(key, _)| key == k))
            .map(|(_, text)| text.clone())
            .collect();
        let cost = shown.iter().map(|t| tokens(t)).sum();
        // Constant confidence: it has no idea, and the separation column
        // should show exactly that.
        Retrieval::ranked(Delivery::Ranked(picked), shown, cost, Some(0.5))
    }
}

// ---------------------------------------------------------------------- rag

/// What a conventional RAG stack does: embed the query, take the nearest
/// vectors, stop. No keyword channel, no trust or type priors, no reranker,
/// no graph. It shares the engram arm's store so the corpus is only embedded
/// once — the difference between the two arms is purely retrieval strategy.
pub struct RagArm<'a> {
    store: &'a dyn engram_core::Store,
    embedder: Box<dyn Embedder>,
    by_id: HashMap<String, String>,
}

impl<'a> RagArm<'a> {
    pub fn new(engram: &'a EngramArm, embedder: Box<dyn Embedder>) -> Self {
        Self {
            store: engram.engine().store(),
            embedder,
            by_id: engram.by_id.clone(),
        }
    }
}

impl Arm for RagArm<'_> {
    fn name(&self) -> &'static str {
        "rag"
    }

    fn retrieve(&self, query: &str, limit: usize) -> Retrieval {
        let Ok(qv) = self.embedder.embed_one(query) else {
            return Retrieval::ranked(Delivery::Ranked(Vec::new()), Vec::new(), 0, None);
        };
        let hits = self.store.search_vec(&qv, limit).unwrap_or_default();
        let mut keys = Vec::new();
        let mut rendered = Vec::new();
        let mut cost = 0;
        let mut top = None;
        for (id, distance) in &hits {
            if top.is_none() {
                top = Some((1.0 - distance).clamp(0.0, 1.0));
            }
            let Some(key) = self.by_id.get(id) else {
                continue;
            };
            if let Ok(Some(node)) = self.store.get_node(id) {
                let body = node.body.as_deref().unwrap_or("");
                cost += tokens(&node.title) + tokens(body);
                // A conventional vector stack hands the model whole chunks.
                rendered.push(format!("## {}\n{}", node.title, body));
            }
            keys.push(key.clone());
        }
        Retrieval::ranked(Delivery::Ranked(keys), rendered, cost, top)
    }
}

// ------------------------------------------------------------------- engram

pub struct EngramArm {
    engine: Engine,
    by_id: HashMap<String, String>,
    brief_cost: usize,
    /// Report the graph the arm actually built, so a run can never quietly
    /// measure an edgeless corpus again.
    pub edges_written: usize,
}

impl EngramArm {
    /// `rerank` must mirror what `serve`/`mcp` actually load, or the arm
    /// measures a stack the product does not ship. The reranker IS the
    /// precision layer — leaving it out understates search and, worse, hides
    /// regressions in the layer that exists to fix ordering.
    pub fn build(
        corpus: &Corpus,
        embedder: Box<dyn Embedder>,
        rerank: Option<Box<dyn engram_core::Reranker>>,
    ) -> anyhow::Result<Self> {
        let store = SqliteStore::open_in_memory()?;
        let mut engine = Engine::new(store, embedder);
        if let Some(r) = rerank {
            engine.set_reranker(r);
        }
        let mut by_id = HashMap::new();

        let mut ids: HashMap<String, String> = HashMap::new();
        for f in &corpus.facts {
            let node = engine.add_node(new_node(f))?;
            ids.insert(f.key.clone(), node.id.clone());
            by_id.insert(node.id, f.key.clone());
        }

        let mut edges_written = 0;
        for e in &corpus.edges {
            let (Some(from_id), Some(to_id)) = (ids.get(&e.from), ids.get(&e.to)) else {
                continue;
            };
            engine.add_edge(engram_core::NewEdge {
                edge_type: engram_core::EdgeType::parse(e.verb)?,
                from_id: from_id.clone(),
                to_id: to_id.clone(),
                source: engram_core::Source::Claude,
                note: None,
                confidence: None,
                strength: None,
                status: None,
            })?;
            edges_written += 1;
        }

        let brief_cost = tokens(&engine.brief(engine.brief_chars(None))?);
        Ok(Self {
            engine,
            by_id,
            brief_cost,
            edges_written,
        })
    }

    /// Mutable access, for the one caller that has to install a component
    /// after the corpus is written: the contradiction bench needs the NLI
    /// model, and nothing else in the harness does.
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// node id -> corpus fact key, so a bench arm reading the same store can
    /// score its hits against the same ground truth.
    pub fn keys(&self) -> &HashMap<String, String> {
        &self.by_id
    }

    /// Re-point any search policy value without rebuilding.
    ///
    /// Sweeping is only affordable because of this: embedding the corpus is
    /// the expensive part, and the tuning lives in per-graph config that
    /// `search_hybrid` reads at query time, so one built engine can answer for
    /// every point on the grid.
    pub fn tune(
        &self,
        f: impl FnOnce(&mut engram_core::config::PolicyConfig),
    ) -> anyhow::Result<()> {
        let mut cfg = self.engine.graph_config();
        f(&mut cfg.policy);
        self.engine.set_graph_config(&cfg)?;
        Ok(())
    }

    /// Flatten every type's `rank_prior` to zero.
    ///
    /// The prior is a multiplicative tilt applied after relevance: a Principle
    /// carries +0.05 and a Problem carries nothing, so a Principle distractor
    /// outranks an equally relevant Problem by construction. It is the last
    /// structural difference between this arm and the pure-vector one, and the
    /// only way to find out whether it is buying ranking quality or quietly
    /// spending recall is to switch it off and look.
    pub fn flatten_type_priors(&self) -> anyhow::Result<()> {
        let mut cfg = self.engine.graph_config();
        for t in cfg.ontology.types.iter_mut() {
            t.roles.rank_prior = 0.0;
        }
        self.engine.set_graph_config(&cfg)?;
        Ok(())
    }
}

fn new_node(f: &Fact) -> NewNode {
    NewNode {
        node_type: f.kind.node_type(),
        title: f.title.clone(),
        body: Some(f.body.clone()),
        created_at: None,
        durability: engram_core::Durability::Stable,
        source: engram_core::Source::Claude,
        session_id: Some("eval".to_string()),
        status: f.kind.status(),
        code_refs: f.code_refs.clone(),
        tags: vec![],
        version: None,
    }
}

impl Arm for EngramArm {
    fn name(&self) -> &'static str {
        "engram"
    }

    fn retrieve(&self, query: &str, limit: usize) -> Retrieval {
        let hits = self.engine.search(query, &[], limit).unwrap_or_default();
        let cost = hits
            .iter()
            .map(|h| tokens(&h.title) + tokens(&h.snippet))
            .sum();
        // A title and the matched snippet — not the whole note. This is the
        // arm's entire cost advantage, so the online context has to be it.
        let rendered: Vec<String> = hits
            .iter()
            .map(|h| {
                let snippet = h.snippet.replace(['\u{e000}', '\u{e001}'], "");
                format!("## {}\n{}", h.title, snippet)
            })
            .collect();
        let mut neighbors = Vec::new();
        for (i, h) in hits.iter().enumerate() {
            for n in &h.neighbors {
                if let Some(key) = self.by_id.get(&n.id) {
                    neighbors.push((key.clone(), i + 1));
                }
            }
        }
        Retrieval {
            top_score: hits.first().map(|h| h.score),
            delivery: Delivery::Ranked(
                hits.iter()
                    .filter_map(|h| self.by_id.get(&h.id).cloned())
                    .collect(),
            ),
            rendered,
            tokens: cost,
            neighbors,
        }
    }

    fn standing_cost(&self) -> usize {
        self.brief_cost
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{Phrasing, corpus};
    use engram_core::FakeEmbedder;

    #[test]
    fn terms_drop_function_words() {
        let t = terms("what retry budget did we settle on for the Kelnor lease broker?");
        assert!(t.contains(&"retry".to_string()));
        assert!(t.contains(&"kelnor".to_string()));
        assert!(!t.contains(&"what".to_string()));
        assert!(!t.contains(&"the".to_string()));
    }

    #[test]
    fn the_curated_file_does_not_select_by_tested_status() {
        // The bug this guards: the corpus writes every tested fact before
        // every distractor, so ANY key-ordered tie-break fills the file with
        // one population. The first version sorted by descending key, held 37
        // distractors out of 37, and scored a clean 0.000 — which looked like
        // a devastating result for hand-maintained files and was actually a
        // sorting mistake.
        // Big enough that keys cross the f0999/f1000 boundary — the earlier
        // 180-fact version of this test could not see the prefix bias at all
        // and passed against a hash that selected distractors exclusively.
        let c = corpus(400, 800, 5);
        let arm = CuratedFileArm::new(&c, 4000);
        assert!(arm.held() > 10, "budget too small to say anything");
        let tested_in_file = arm
            .keys
            .iter()
            .filter(|k| c.fact(k).is_some_and(|f| f.tested))
            .count();
        let share = tested_in_file as f64 / arm.held() as f64;
        let corpus_share = c.tested() as f64 / c.facts.len() as f64;
        assert!(
            (share - corpus_share).abs() < 0.2,
            "curated file is {share:.2} tested vs {corpus_share:.2} in the corpus — \
             the selection is correlated with what gets asked about"
        );
    }

    #[test]
    fn whole_file_always_delivers_everything_at_full_price() {
        let c = corpus(20, 40, 1);
        let arm = WholeFileArm::new(&c);
        let r = arm.retrieve("anything at all", 5);
        assert!(matches!(r.delivery, Delivery::Dump(_)));
        assert_eq!(r.tokens, arm.standing_cost());
        assert!(r.tokens > 200, "20 facts should not be free");
    }

    #[test]
    fn grep_finds_lexical_questions() {
        let c = corpus(30, 60, 2);
        let arm = GrepArm::new(&c);
        let f = &c.facts[0];
        let q = f
            .questions
            .iter()
            .find(|q| q.phrasing == Phrasing::Lexical)
            .unwrap();
        match arm.retrieve(&q.text, 10).delivery {
            Delivery::Ranked(keys) => assert_eq!(keys.first(), Some(&f.key)),
            Delivery::Dump(_) => panic!("grep ranks"),
        }
    }

    #[test]
    fn grep_returns_nothing_for_a_subject_it_never_saw() {
        let c = corpus(30, 60, 3);
        let arm = GrepArm::new(&c);
        match arm.retrieve("Zzyzxqua flux capacitor", 10).delivery {
            Delivery::Ranked(keys) => assert!(keys.is_empty()),
            Delivery::Dump(_) => panic!("grep ranks"),
        }
    }

    #[test]
    fn engram_retrieves_a_fact_by_its_own_title() {
        // Robust on any embedder: this is the lexical path, and if an exact
        // title cannot be found the harness itself is broken.
        let c = corpus(40, 80, 4);
        let arm = EngramArm::build(&c, Box::new(FakeEmbedder::default()), None).unwrap();
        let f = &c.facts[7];
        match arm.retrieve(&f.title, 5).delivery {
            Delivery::Ranked(keys) => {
                assert_eq!(keys.first(), Some(&f.key), "exact title must rank first")
            }
            Delivery::Dump(_) => panic!("engram ranks"),
        }
        assert!(arm.standing_cost() > 0, "the brief is not free either");
    }
}
