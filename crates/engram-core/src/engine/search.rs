//! Hybrid search: retrieval, time windows, reranking and delivery order.

use super::*;

impl Engine {
    /// Hybrid retrieval: embed the query, fuse keyword + vector hits, run the
    /// precision layer when present (over-fetch candidates, cross-encode them
    /// against the query, re-order), then attach each hit's 1-hop neighbors
    /// (conflicts/supersessions first) so contradictions surface passively
    /// with the match.
    pub fn search(&self, query: &str, types: &[NodeType], limit: usize) -> Result<Vec<SearchHit>> {
        self.search_filtered(query, types, limit, &SearchFilter::default())
    }

    /// [`Engine::search`] with a time window and an explicit ordering.
    ///
    /// The window prunes candidates BEFORE the reranker and before both
    /// calibrated cuts, so the delivery floor, the knee trim and the verdict
    /// all describe the scoped set — a "strong" verdict means the best answer
    /// *inside the window* cleared the line, not that something outside it
    /// did.
    ///
    /// The ordering is applied LAST, after every cut. Re-sorting a delivered
    /// set cannot change what cleared the line, so a chronological read still
    /// carries the same verdict its relevance-ordered twin would.
    pub fn search_filtered(
        &self,
        query: &str,
        types: &[NodeType],
        limit: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<SearchHit>> {
        for t in types {
            self.check_node_type(t)?;
        }
        self.check_clock(filter)?;
        let qv = self.embedder.embed_one(query)?;
        let fetch = match &self.reranker {
            Some(_) => (limit * 3).clamp(12, 50),
            None => limit,
        };
        let mut hits = self.store.search_hybrid(
            query,
            Some(&qv),
            types,
            fetch,
            filter.window,
            &filter.clock,
        )?;
        if let Some(reranker) = &self.reranker
            && !hits.is_empty()
        {
            // Even a single hit goes through: the delivery floor and the
            // search verdict both read the cross-encoder's calibrated scale,
            // so an unreranked one-hit result would be judged on the wrong
            // ruler.
            self.rerank(reranker.as_ref(), query, &mut hits);
            // Calibrated delivery, trim face: hits under the floor are tail
            // by score even when the vote ranked them mid-list — measured
            // free of recall cost (policy::DELIVERY_FLOOR), and every token
            // they would have carried is attention the answer keeps.
            let floor = self.store.config().policy.delivery_floor;
            hits.retain(|h| h.score >= floor);
        }
        // Session-diverse delivery: when the floor-surviving pool is deeper
        // than the limit, a session already holding a delivered slot pays a
        // rank demotion for each further one, so restatements from one
        // session stop crowding out complementary evidence from others. With
        // the knob at 0 this IS `truncate(limit)`.
        session_diverse_select(
            &mut hits,
            limit,
            self.store.config().policy.session_diversity_demote,
            |h| h.session_id.as_deref(),
        );
        // Calibrated delivery, knee face: cut at the largest relative drop
        // in the delivered score curve — the cliff between the relevance
        // head and the noise tail (policy::KNEE_MIN_CLIFF). Measured free of
        // recall cost at every size, and unlike the fixed floor the cliff
        // sharpens as the graph grows. Reranker-gated like the floor: the
        // fused hybrid score is a different scale.
        if self.reranker.is_some()
            && let Some(cliff) = self.store.config().policy.knee_cliff
        {
            let mut curve: Vec<f64> = hits.iter().map(|h| h.score).collect();
            curve.sort_by(|a, b| b.total_cmp(a));
            if let Some(k) = knee_floor(&curve, cliff) {
                hits.retain(|h| h.score >= k);
            }
        }
        if let Some(band) = self.store.config().policy.twin_trust_order {
            self.order_twins(&mut hits, band)?;
        }
        for hit in &mut hits {
            hit.neighbors = self.store.neighbors(&hit.id, NEIGHBOR_CAP)?;
        }
        // Role, not type name: a custom ontology can call its tombstone
        // anything, and a reader skimming for X must not take "Removed: X"
        // for a memory of X — the flag rides on every surface.
        let cfg = self.store.config();
        for hit in &mut hits {
            hit.tombstone = cfg
                .type_def(hit.node_type.as_str())
                .is_some_and(|t| t.roles.tombstone);
            hit.standing = Some(self.standing_of(&hit.id, hit.tombstone, hit.stale)?);
        }
        order_hits(&mut hits, filter.order);
        // Observability stamp on what was actually returned — never the
        // over-fetched candidates the reranker discarded. (Trust doesn't
        // read this either way; see policy.)
        let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        self.store.touch(&ids)?;
        Ok(hits)
    }

    /// The half-open window a named working version was current for, read from
    /// the audit journal's `version_switched` rows (`set_current_version`
    /// journals every switch under entity_id "version").
    ///
    /// The window OPENS when the version was switched to and CLOSES when it
    /// was switched away from — the still-current version has no end. A
    /// version that was set more than once (a cycle reopened, a bump reverted)
    /// spans from its first switch-in to the last switch-away, which is the
    /// reading that keeps "during 0.8.4" meaning the same thing as the
    /// release's story.
    ///
    /// `None` when the journal never mentions the version: the caller must
    /// say so rather than silently searching all of time.
    pub fn version_window(&self, version: &str) -> Result<Option<crate::timespec::TimeWindow>> {
        let version = version.trim();
        if version.is_empty() {
            return Ok(None);
        }
        // The journal is paged newest-first; version switches are rare enough
        // that one deep page holds a project's whole history.
        let page = self.store.audit_page(None, Some("version"), 4096)?;
        let (mut after, mut before) = (None::<i64>, None::<i64>);
        for row in &page.entries {
            if row.action != "version_switched" {
                continue;
            }
            // The row's title is "<previous> → <next>".
            let Some((prev, next)) = row.title.as_deref().and_then(|t| t.split_once('→')) else {
                continue;
            };
            let (prev, next) = (prev.trim(), next.trim());
            if next == version {
                after = Some(after.map_or(row.ts, |a: i64| a.min(row.ts)));
            }
            if prev == version {
                before = Some(before.map_or(row.ts, |b: i64| b.max(row.ts)));
            }
        }
        // Switched away from but never switched to: the graph was already on
        // that version before journaling began. Everything up to the switch
        // away is the honest window.
        if after.is_none() && before.is_none() {
            return Ok(None);
        }
        Ok(Some(crate::timespec::TimeWindow { after, before }))
    }

    /// Resolve the temporal arguments a caller passed into one filter,
    /// anchored at the current clock. `during_version` contributes the
    /// release's window; explicit `after`/`before` INTERSECT it rather than
    /// replacing it, so "during 0.8.4, after the 10th" narrows as it reads.
    pub fn time_filter(
        &self,
        after: Option<&str>,
        before: Option<&str>,
        during_version: Option<&str>,
        order: Option<&str>,
    ) -> Result<SearchFilter> {
        self.time_filter_clocked(after, before, during_version, order, None)
    }

    /// [`Engine::time_filter`] with the bitemporal clock selector:
    /// `date_field` re-aims the window at a date-kind custom field, or a
    /// `from..to` pair with interval-overlap semantics. Validated here so an
    /// unreadable selector is an ERROR, never a dropped filter.
    pub fn time_filter_clocked(
        &self,
        after: Option<&str>,
        before: Option<&str>,
        during_version: Option<&str>,
        order: Option<&str>,
        date_field: Option<&str>,
    ) -> Result<SearchFilter> {
        let mut window = crate::timespec::window(after, before, crate::store::now())?;
        if let Some(v) = during_version.map(str::trim).filter(|v| !v.is_empty()) {
            let Some(release) = self.version_window(v)? else {
                return Err(crate::Error::Config(format!(
                    "no recorded switch to or from version {v:?} — `set_version` \
                     journals each switch, so only versions this graph was \
                     actually worked under can be searched by name"
                )));
            };
            window = window.intersect(release);
            if window.is_empty() {
                return Err(crate::Error::Config(format!(
                    "the window is empty: the given after/before falls outside \
                     the time version {v:?} was current"
                )));
            }
        }
        let order = match order.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => crate::timespec::SearchOrder::parse(s).ok_or_else(|| {
                crate::Error::Config(format!(
                    "order {s:?} must be relevance, chronological or recent"
                ))
            })?,
            None => crate::timespec::SearchOrder::default(),
        };
        let clock = match date_field.map(str::trim).filter(|s| !s.is_empty()) {
            Some(sel) => crate::timespec::TimeClock::parse_selector(sel).ok_or_else(|| {
                crate::Error::Config(format!(
                    "date_field {sel:?} must be a field name or a from..to pair"
                ))
            })?,
            None => crate::timespec::TimeClock::CreatedAt,
        };
        Ok(SearchFilter {
            window,
            order,
            clock,
        })
    }

    /// Calibrated delivery, verdict face: how the assistant should hold a
    /// search result. `"none"` — the graph is silent, say so instead of
    /// inventing. `"weak"` — delivered, but nothing cleared the calibrated
    /// confidence line (`policy::WEAK_EVIDENCE_TOP`): as likely topical
    /// adjacency as an answer, verify before relying. `"strong"` — the top
    /// hit cleared the line. Returns `None` for a non-empty result when no
    /// reranker is loaded: the fused hybrid score is a different scale, and
    /// a verdict from an uncalibrated ruler would be noise wearing a badge.
    /// A label rather than a harder floor by measurement: hard abstention
    /// costs oblique recall (see `policy::DELIVERY_FLOOR`), a label costs
    /// nothing and keeps the assistant aligned.
    pub fn search_confidence(&self, hits: &[SearchHit]) -> Option<&'static str> {
        if hits.is_empty() {
            return Some("none");
        }
        self.reranker.as_ref()?;
        let top = hits.iter().map(|h| h.score).fold(f64::MIN, f64::max);
        Some(if top < self.store.config().policy.weak_evidence_top {
            "weak"
        } else {
            "strong"
        })
    }

    /// Re-score candidates with the cross-encoder: relevance comes from the
    /// reranker logit (sigmoid-squashed), trust modulates it the same way it
    /// modulates the hybrid blend — relevance dominates, trust breaks ties
    ///. A reranker failure keeps hybrid order: precision is an
    /// upgrade, never a dependency.
    ///
    /// With `rerank_vote_k` set (the default), the cross-encoder does not get
    /// the final word: its ordering is combined with the retrieval ordering by
    /// a reciprocal-rank vote, so a hit two independent channels ranked highly
    /// cannot be buried by one confident cross-encoder mistake.
    ///
    /// The *reported* score stays the cross-encoder's calibrated relevance
    /// either way. The vote decides order only — reciprocal-rank sums encode
    /// rank, not confidence, and `score` is surfaced to assistants over MCP and
    /// to the pane, where a number that no longer means "how good is this
    /// match" would quietly mislead both.
    pub(super) fn rerank(&self, reranker: &dyn Reranker, query: &str, hits: &mut [SearchHit]) {
        // `rerank_full_note` scores `title + whole body` instead of `title +
        // keyword-window snippet`: notes fit the cross-encoder's window whole,
        // and on an oblique query the evidence sentence often shares no
        // keyword with the query — it never enters the window at all. The
        // snippet stays the delivered text either way; this changes only what
        // the judge reads.
        let full_note = self.config().policy.rerank_full_note;
        let docs: Vec<String> = hits
            .iter()
            .map(|h| {
                if full_note
                    && let Ok(Some(node)) = self.store.get_node(&h.id)
                    && let Some(body) = node.body.as_deref().filter(|b| !b.is_empty())
                {
                    return format!("{}\n{}", h.title, body);
                }
                let snippet = h.snippet.replace(
                    [crate::store::SNIPPET_OPEN, crate::store::SNIPPET_CLOSE],
                    "",
                );
                format!("{}\n{}", h.title, snippet)
            })
            .collect();
        let Ok(scores) = reranker.rank(query, &docs) else {
            return;
        };
        if scores.len() != hits.len() {
            return;
        }
        let config = self.config();
        let trust_weight = config.policy.rerank_trust_weight;

        // `hits` arrives in retrieval order, so a hit's index IS its incoming
        // rank — captured before anything re-scores or re-sorts.
        let retrieval_rank: Vec<usize> = (1..=hits.len()).collect();
        for (hit, logit) in hits.iter_mut().zip(&scores) {
            let relevance = 1.0 / (1.0 + f64::from(-*logit).exp());
            hit.score = relevance * (1.0 + trust_weight * hit.trust);
        }

        let Some(k) = config.policy.rerank_vote_k else {
            hits.sort_by(|a, b| b.score.total_cmp(&a.score));
            return;
        };

        let mut by_rerank: Vec<usize> = (0..hits.len()).collect();
        by_rerank.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
        let mut vote = vec![0.0_f64; hits.len()];
        for (rank, idx) in by_rerank.into_iter().enumerate() {
            vote[idx] = 1.0 / (k + retrieval_rank[idx] as f64) + 1.0 / (k + (rank + 1) as f64);
        }
        let mut order: Vec<usize> = (0..hits.len()).collect();
        order.sort_by(|a, b| vote[*b].total_cmp(&vote[*a]).then(a.cmp(b)));
        let reordered: Vec<SearchHit> = order.into_iter().map(|i| hits[i].clone()).collect();
        hits.clone_from_slice(&reordered);
    }
}

/// The temporal arguments of one search, resolved. Default is the
/// whole timeline in relevance order — which is exactly what every caller
/// before this existed asked for, so [`Engine::search`] stays a thin wrapper
/// and no existing behavior moves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilter {
    pub window: crate::timespec::TimeWindow,
    pub order: crate::timespec::SearchOrder,
    /// Which clock the window reads: `created_at` by default, or a
    /// date-kind custom field / `from..to` field span — bitemporal search
    /// for graphs that declare an event clock (historic imports).
    pub clock: crate::timespec::TimeClock,
}

impl SearchFilter {
    /// Does this filter change anything? Used to keep the unfiltered path
    /// identical rather than merely equivalent. A non-default clock always
    /// travels with a window (validated at search time), so the window test
    /// covers it.
    pub fn is_default(&self) -> bool {
        self.window.is_open() && !self.order.is_temporal()
    }
}

/// Re-order a delivered result set. Ties break by id, which is time-sortable,
/// so two notes captured in the same second still come out in a stable and
/// truthful order rather than whatever the hash iteration produced.
pub(super) fn order_hits(hits: &mut [SearchHit], order: crate::timespec::SearchOrder) {
    use crate::timespec::SearchOrder;
    match order {
        SearchOrder::Relevance => {}
        SearchOrder::Chronological => {
            hits.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        }
        SearchOrder::Recent => {
            hits.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        }
    }
}

/// Cosine similarity, computed with magnitudes rather than assuming unit
/// vectors — the embedder is swappable (models.json), and a normalization
/// assumption that holds for today's default would fail silently on the next.
pub(super) fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Session-diverse delivery cut: pick `limit` hits from a ranked
/// candidate list, demoting each additional hit from an already-picked
/// session by `demote` RANK positions — a greedy MMR keyed on rank rather
/// than score, because rank is the one scale that transfers between graphs
/// (the 0.8.1 register lesson). With `demote` 0 the pick is byte-identical
/// to `truncate(limit)`; hits without a session are never demoted and never
/// count against one. Selection and order only — scores are untouched, so
/// the floor, the knee and the verdict all still read the cross-encoder's
/// calibrated scale.
pub(crate) fn session_diverse_select<T>(
    hits: &mut Vec<T>,
    limit: usize,
    demote: f64,
    session_of: impl Fn(&T) -> Option<&str>,
) {
    if hits.len() <= limit || demote <= 0.0 {
        hits.truncate(limit);
        return;
    }
    let n = hits.len();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut used = vec![false; n];
    let mut picked: Vec<usize> = Vec::with_capacity(limit);
    while picked.len() < limit {
        let mut best: Option<(f64, usize)> = None;
        for (i, taken) in used.iter().enumerate() {
            if *taken {
                continue;
            }
            let repeats = session_of(&hits[i])
                .and_then(|s| counts.get(s).copied())
                .unwrap_or(0);
            let effective = i as f64 + demote * repeats as f64;
            // Strict `<` keeps ties on the earlier (better-ranked) index.
            if best.is_none_or(|(b, _)| effective < b) {
                best = Some((effective, i));
            }
        }
        let Some((_, i)) = best else { break };
        used[i] = true;
        if let Some(s) = session_of(&hits[i]) {
            *counts.entry(s.to_string()).or_default() += 1;
        }
        picked.push(i);
    }
    let mut keep = std::mem::take(hits);
    let mut by_index: Vec<Option<T>> = keep.drain(..).map(Some).collect();
    *hits = picked
        .into_iter()
        .filter_map(|i| by_index[i].take())
        .collect();
}
