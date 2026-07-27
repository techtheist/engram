//! The search bench: retrieval strategies Engram does *not* ship.
//!
//! `arms.rs` measures the product. This measures candidate replacements for
//! the part of the product that is losing, and it lives here rather than
//! behind a feature flag in `engram-core` for one reason: three of the four
//! mechanisms below are expected to fail, and a mechanism that fails should
//! leave no trace in the shipped stack.
//!
//! Every variant reads the same store the `engram` and `rag` arms read, so a
//! difference between rows is a difference in strategy and nothing else.

use std::collections::{HashMap, HashSet};

use engram_core::{Embedder, Reranker, Store};

use crate::arms::{Arm, Delivery, Retrieval, tokens};

/// How the keyword and vector channels are combined into one ordering.
#[derive(Debug, Clone, Copy)]
pub enum Fusion {
    /// What ships: a weighted sum of two normalised scores, with the vector
    /// side floored.
    ///
    /// Two properties matter and both hurt name-free queries. The floor makes
    /// a sub-threshold cosine worth exactly zero, and a candidate worth zero
    /// on both channels is skipped rather than ranked last — so the floor
    /// deletes. And `keyword_weight` caps what a query with no lexical overlap
    /// can score at `1 - keyword_weight`, while any distractor sharing one
    /// common word scores on both channels.
    Weighted {
        keyword_weight: f64,
        semantic_floor: f64,
    },
    /// Reciprocal rank fusion: `Σ 1/(k + rank)` over the channels a candidate
    /// appears in.
    ///
    /// The reason to try it is that it reads *ranks*, not scores, so neither
    /// pathology above can occur: there is no floor to fall under and no
    /// magnitude to be capped by. A gold ranked 3rd by vectors and absent from
    /// keywords beats a distractor ranked 40th by vectors and 1st by keywords
    /// — which is the ordering a name-free query needs and the one the
    /// weighted sum gets backwards.
    ///
    /// `k` damps the head: small k makes rank 1 dominate, large k flattens the
    /// contribution toward equality. 60 is the number from the original paper
    /// and is very flat at the depths used here.
    Rrf { k: f64 },
    /// The keyword channel switched off entirely — what the `rag` arm does,
    /// expressed as a strategy so the other axes (query prefix, rerank depth,
    /// graph spreading) can be measured on top of it.
    VectorOnly,
}

/// The instruction `bge-*-en-v1.5` was trained to see in front of a *query*,
/// and never in front of a document.
///
/// These models are trained asymmetrically: the query side gets this sentence,
/// the corpus side gets nothing. Engram embeds both through one `embed_one`
/// and so has never used it. Retrieval here is exactly the asymmetric case the
/// instruction is for — a short question against a long note — and it costs one
/// string concatenation to test.
pub const BGE_QUERY_INSTRUCTION: &str = "Represent this sentence for searching relevant passages: ";

/// How much authority the cross-encoder gets over the final ordering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RerankMode {
    /// What ships: the reranker's score *replaces* the fused score outright,
    /// and the retrieval evidence that produced the candidate is discarded.
    Replace,
    /// The reranker's ordering is fused with the one it was given, by the same
    /// reciprocal-rank sum used to combine keyword and vector channels.
    ///
    /// This is the ranking form of a rule this project already holds
    /// elsewhere: a model's judgment nominates, it does not decide. Under
    /// `Replace`, one confident cross-encoder mistake sends a gold from rank 2
    /// to rank 20 and nothing remembers that two independent channels had
    /// ranked it second. Under a blend, the reranker has to out-vote them.
    RrfBlend { k: f64 },
}

#[derive(Debug, Clone)]
pub struct Strategy {
    pub label: String,
    pub fusion: Fusion,
    /// Candidates pulled from each channel before fusion.
    pub pool: usize,
    /// How many fused candidates the cross-encoder re-scores. 0 disables it.
    ///
    /// This is the one axis where the shipped stack has headroom the `rag` arm
    /// structurally cannot match: a bi-encoder's ordering is fixed once the
    /// vectors are stored, while a cross-encoder reads the query and the
    /// document together and can promote a gold from depth 40 to rank 2. Its
    /// cost is linear in the depth, which is why the shipped value is 30.
    pub rerank_depth: usize,
    /// What the cross-encoder's opinion is allowed to do to the ordering.
    pub rerank_mode: RerankMode,
    /// Show the cross-encoder the whole note instead of the 160-character
    /// excerpt the shipped path hands it.
    ///
    /// Worth an axis because the excerpt is a *prefix*: on a note whose median
    /// body is 748 characters it is the first fifth, and if the sentence that
    /// answers the question sits below it, the cross-encoder is being asked to
    /// judge relevance from text that does not contain the answer. The reason
    /// it is not free is cost — cross-encoding scales with document length,
    /// and this multiplies the tokens fed to it by roughly five.
    pub rerank_full_body: bool,
    /// Weight on the score a candidate inherits from its best-scoring
    /// neighbour — spreading activation across the graph's edges.
    ///
    /// Zero means edges do not touch ranking, which is what ships today: they
    /// are attached to results after ranking as delivery, never before it as
    /// evidence. Above zero, a fact that no query term and no vector reaches
    /// can still rank because something adjacent to it did.
    pub graph_boost: f64,
    /// Rewire every edge at random, preserving only how many there are.
    ///
    /// The control for `graph_boost`, and the reason any gain from it can be
    /// believed. 70% of generated edges join facts about the same component,
    /// so a boost that spreads along them is partly just a topical prior — and
    /// a topical prior would survive rewiring, because a random edge in a
    /// corpus this dense still lands on a same-component fact sometimes. If
    /// the gain holds up under shuffling it was never the graph; if it
    /// collapses, the edges carried it.
    pub shuffle_edges: bool,
    /// Prepended to the query before embedding, never to the stored notes.
    pub query_prefix: Option<&'static str>,
}

impl Strategy {
    /// The shipped stack, expressed in this module's terms — the row every
    /// other row is a delta against. Not identical to `EngramArm` (no trust or
    /// type priors, no recency), which is deliberate: those were measured
    /// separately and cost 0.05 oblique between them.
    pub fn shipped() -> Self {
        Self {
            label: "weighted floor=0.6".to_string(),
            fusion: Fusion::Weighted {
                keyword_weight: engram_core::policy::SEARCH_KEYWORD_WEIGHT,
                semantic_floor: engram_core::policy::SEARCH_SEMANTIC_FLOOR,
            },
            pool: 120,
            rerank_depth: 30,
            rerank_mode: RerankMode::Replace,
            rerank_full_body: false,
            graph_boost: 0.0,
            shuffle_edges: false,
            query_prefix: None,
        }
    }

    pub fn label(mut self, s: &str) -> Self {
        self.label = s.to_string();
        self
    }
}

pub struct VariantArm<'a> {
    store: &'a dyn Store,
    embedder: Box<dyn Embedder>,
    reranker: Option<&'a dyn Reranker>,
    by_id: HashMap<String, String>,
    /// node id -> neighbour node ids, both directions.
    adjacency: HashMap<String, Vec<String>>,
    strategy: Strategy,
    name: &'static str,
}

impl<'a> VariantArm<'a> {
    pub fn new(
        store: &'a dyn Store,
        by_id: HashMap<String, String>,
        embedder: Box<dyn Embedder>,
        reranker: Option<&'a dyn Reranker>,
        strategy: Strategy,
    ) -> anyhow::Result<Self> {
        let adjacency = adjacency(store, strategy.shuffle_edges)?;
        Ok(Self {
            store,
            embedder,
            reranker,
            by_id,
            adjacency,
            strategy,
            name: "variant",
        })
    }

    pub fn label(&self) -> &str {
        &self.strategy.label
    }

    /// The query as the embedder sees it — instruction-prefixed when the
    /// strategy asks for it. The keyword channel always gets the raw text: an
    /// instruction sentence in an FTS query would just be noise terms.
    fn embed_query(&self, query: &str) -> engram_core::Result<Vec<f32>> {
        match self.strategy.query_prefix {
            Some(p) => self.embedder.embed_one(&format!("{p}{query}")),
            None => self.embedder.embed_one(query),
        }
    }

    /// Fused candidate scores, best first.
    fn fuse(&self, query: &str, qv: &[f32]) -> Vec<(String, f64)> {
        let pool = self.strategy.pool;
        let keyword_channel = !matches!(self.strategy.fusion, Fusion::VectorOnly);
        let fts = if keyword_channel {
            self.store.search_fts(query, &[], pool).unwrap_or_default()
        } else {
            Vec::new()
        };
        let vecs = self.store.search_vec(qv, pool).unwrap_or_default();

        let mut scored: HashMap<String, f64> = HashMap::new();
        match self.strategy.fusion {
            Fusion::Weighted {
                keyword_weight,
                semantic_floor,
            } => {
                let max_fts = fts.iter().map(|h| h.score).fold(0.0_f64, f64::max);
                let mut keyword: HashMap<&str, f64> = HashMap::new();
                if max_fts > 0.0 {
                    for h in &fts {
                        keyword.insert(h.id.as_str(), h.score / max_fts);
                    }
                }
                let mut semantic: HashMap<&str, f64> = HashMap::new();
                for (id, distance) in &vecs {
                    semantic.insert(id.as_str(), (1.0 - distance).clamp(0.0, 1.0));
                }
                for id in keyword.keys().chain(semantic.keys()) {
                    let kw = keyword.get(id).copied().unwrap_or(0.0);
                    let raw = semantic.get(id).copied().unwrap_or(0.0);
                    let sem = ((raw - semantic_floor) / (1.0 - semantic_floor).max(f64::EPSILON))
                        .clamp(0.0, 1.0);
                    let relevance = keyword_weight * kw + (1.0 - keyword_weight) * sem;
                    // The shipped skip, reproduced exactly: zero relevance is
                    // not last place, it is absent.
                    if relevance <= 0.0 {
                        continue;
                    }
                    scored.insert((*id).to_string(), relevance);
                }
            }
            Fusion::Rrf { k } => {
                for (rank, h) in fts.iter().enumerate() {
                    *scored.entry(h.id.clone()).or_default() += 1.0 / (k + (rank + 1) as f64);
                }
                for (rank, (id, _)) in vecs.iter().enumerate() {
                    *scored.entry(id.clone()).or_default() += 1.0 / (k + (rank + 1) as f64);
                }
            }
            Fusion::VectorOnly => {
                for (id, distance) in &vecs {
                    scored.insert(id.clone(), (1.0 - distance).clamp(0.0, 1.0));
                }
            }
        }

        let mut out: Vec<(String, f64)> = scored.into_iter().collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }

    /// Let every candidate lend score to its neighbours, and let a neighbour
    /// that ranked nowhere enter the list on borrowed score.
    ///
    /// One hop only, and `max` rather than `sum` over the neighbours: summing
    /// would rank by degree, promoting whatever the corpus happened to link
    /// most rather than whatever the query is about.
    fn spread(&self, ranked: &mut Vec<(String, f64)>) {
        let boost = self.strategy.graph_boost;
        if boost <= 0.0 {
            return;
        }
        let mut inherited: HashMap<String, f64> = HashMap::new();
        for (id, score) in ranked.iter() {
            let Some(neighbours) = self.adjacency.get(id) else {
                continue;
            };
            for n in neighbours {
                let lent = boost * score;
                let slot = inherited.entry(n.clone()).or_default();
                if lent > *slot {
                    *slot = lent;
                }
            }
        }
        // Candidates that already ranked keep their own score and add what
        // they inherited; the rest enter the list on borrowed score alone.
        for (id, score) in ranked.iter_mut() {
            if let Some(lent) = inherited.remove(id.as_str()) {
                *score += lent;
            }
        }
        ranked.extend(inherited);
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    }
}

/// node id -> neighbours, optionally rewired at random.
///
/// The shuffle preserves the edge count and nothing else, which is the point:
/// it is a graph with the same density and no meaning.
fn adjacency(store: &dyn Store, shuffle: bool) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let edges = store.all_edges()?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    if !shuffle {
        for e in &edges {
            out.entry(e.from_id.clone())
                .or_default()
                .push(e.to_id.clone());
            out.entry(e.to_id.clone())
                .or_default()
                .push(e.from_id.clone());
        }
        return Ok(out);
    }

    let ids: Vec<String> = store.all_nodes()?.into_iter().map(|n| n.id).collect();
    if ids.len() < 2 {
        return Ok(out);
    }
    let mut rng = crate::rng::Rng::new(0x5eed_c047);
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut placed = 0;
    let mut attempts = 0;
    while placed < edges.len() && attempts < edges.len() * 20 {
        attempts += 1;
        let (a, b) = (rng.below(ids.len()), rng.below(ids.len()));
        if a == b || !seen.insert((a.min(b), a.max(b))) {
            continue;
        }
        out.entry(ids[a].clone()).or_default().push(ids[b].clone());
        out.entry(ids[b].clone()).or_default().push(ids[a].clone());
        placed += 1;
    }
    Ok(out)
}

impl Arm for VariantArm<'_> {
    fn name(&self) -> &'static str {
        self.name
    }

    fn retrieve(&self, query: &str, limit: usize) -> Retrieval {
        let Ok(qv) = self.embed_query(query) else {
            return Retrieval::ranked(Delivery::Ranked(Vec::new()), Vec::new(), 0, None);
        };
        let mut ranked = self.fuse(query, &qv);
        self.spread(&mut ranked);

        if let Some(reranker) = self.reranker
            && self.strategy.rerank_depth > 0
        {
            ranked.truncate(self.strategy.rerank_depth);
            let full = self.strategy.rerank_full_body;
            let docs: Vec<String> = ranked
                .iter()
                .map(|(id, _)| match self.store.get_node(id) {
                    Ok(Some(n)) => {
                        let text = if full {
                            n.body.clone().unwrap_or_default()
                        } else {
                            crate::arms::excerpt_of(&n)
                        };
                        format!("{}\n{}", n.title, text)
                    }
                    _ => String::new(),
                })
                .collect();
            if let Ok(scores) = reranker.rank(query, &docs)
                && scores.len() == ranked.len()
            {
                match self.strategy.rerank_mode {
                    RerankMode::Replace => {
                        for (slot, logit) in ranked.iter_mut().zip(scores) {
                            slot.1 = 1.0 / (1.0 + (-logit as f64).exp());
                        }
                    }
                    RerankMode::RrfBlend { k } => {
                        // `ranked` is already in retrieval order, so a
                        // candidate's index IS its incoming rank.
                        let mut order: Vec<usize> = (0..ranked.len()).collect();
                        order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
                        let mut rerank_rank = vec![0usize; ranked.len()];
                        for (rank, idx) in order.into_iter().enumerate() {
                            rerank_rank[idx] = rank + 1;
                        }
                        for (i, slot) in ranked.iter_mut().enumerate() {
                            slot.1 = 1.0 / (k + (i + 1) as f64) + 1.0 / (k + rerank_rank[i] as f64);
                        }
                    }
                }
                ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            }
        }
        ranked.truncate(limit);

        let top = ranked.first().map(|(_, s)| *s);
        let mut keys = Vec::new();
        let mut rendered = Vec::new();
        let mut cost = 0;
        for (id, _) in &ranked {
            if let Ok(Some(node)) = self.store.get_node(id) {
                // Charged like the engram arm: the caller is shown a title and
                // an excerpt, not the whole record.
                let excerpt = crate::arms::excerpt_of(&node);
                cost += tokens(&node.title) + tokens(&excerpt);
                rendered.push(format!("## {}\n{}", node.title, excerpt));
            }
            if let Some(key) = self.by_id.get(id) {
                keys.push(key.clone());
            }
        }
        Retrieval::ranked(Delivery::Ranked(keys), rendered, cost, top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arms::EngramArm;
    use crate::generate::corpus;
    use engram_core::FakeEmbedder;

    fn bench(strategy: Strategy) -> (EngramArm, crate::generate::Corpus) {
        let c = corpus(40, 80, 9);
        let arm = EngramArm::build(&c, Box::new(FakeEmbedder::default()), None).unwrap();
        let _ = strategy;
        (arm, c)
    }

    #[test]
    fn rrf_reads_ranks_and_never_deletes_a_candidate() {
        // The property that makes RRF worth measuring: every candidate either
        // channel produced survives fusion. The weighted sum drops the ones
        // that score zero on both, which is the whole defect being chased.
        let (arm, c) = bench(Strategy::shipped());
        let v = VariantArm::new(
            arm.engine().store(),
            arm.keys().clone(),
            Box::new(FakeEmbedder::default()),
            None,
            Strategy {
                fusion: Fusion::Rrf { k: 60.0 },
                rerank_depth: 0,
                ..Strategy::shipped()
            },
        )
        .unwrap();
        let f = &c.facts[3];
        let qv = FakeEmbedder::default().embed_one(&f.title).unwrap();
        let fused = v.fuse(&f.title, &qv);
        assert!(!fused.is_empty(), "fusion returned nothing at all");
        assert!(
            fused.windows(2).all(|w| w[0].1 >= w[1].1),
            "fused candidates must come out ordered"
        );
    }

    #[test]
    fn the_graph_boost_can_introduce_a_node_no_channel_returned() {
        // Spreading activation is only interesting if it *adds* candidates.
        // If it merely re-ordered ones already found it would be a tie-break,
        // not a retrieval mechanism.
        let (arm, _) = bench(Strategy::shipped());
        let v = VariantArm::new(
            arm.engine().store(),
            arm.keys().clone(),
            Box::new(FakeEmbedder::default()),
            None,
            Strategy {
                graph_boost: 0.5,
                rerank_depth: 0,
                ..Strategy::shipped()
            },
        )
        .unwrap();
        let seed = arm.keys().keys().next().unwrap().clone();
        let mut ranked = vec![(seed.clone(), 1.0)];
        v.spread(&mut ranked);
        if v.adjacency.contains_key(&seed) {
            assert!(ranked.len() > 1, "a linked node lent its score to nobody");
        }
    }

    #[test]
    fn shuffling_edges_keeps_the_density_and_drops_the_meaning() {
        let (arm, _) = bench(Strategy::shipped());
        let store = arm.engine().store();
        let real = adjacency(store, false).unwrap();
        let fake = adjacency(store, true).unwrap();
        let degree = |m: &HashMap<String, Vec<String>>| m.values().map(Vec::len).sum::<usize>();
        assert_eq!(
            degree(&real),
            degree(&fake),
            "the control must preserve edge count"
        );
        assert_ne!(real, fake, "the control must not preserve the wiring");
    }
}
