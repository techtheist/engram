//! The per-graph policy numbers; defaults come from [`crate::policy`].

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PolicyConfig {
    /// Trust anchors: created → confirmed → approved (0..=1, ordered).
    pub trust_created: f64,
    pub trust_confirmed: f64,
    pub trust_approved: f64,
    /// Where approved trust bottoms out — approval never fully expires.
    pub trust_approved_floor: f64,
    /// Where unapproved trust bottoms out.
    pub trust_floor: f64,
    /// Below this computed trust a node is stale (verify before relying).
    pub stale_trust: f64,
    /// Unapproved episodic trust runs start→floor over this many days.
    pub episodic_window_days: i64,
    /// Volatile notes rot fast: days from start to floor.
    pub volatile_window_days: i64,
    /// Approved (non-stable) trust runs its course over this many days.
    pub approved_window_days: i64,
    /// Days below stale before the decay pass archives a provisional node.
    pub decay_ttl_days: i64,
    /// Same-type cosine at/above which add_note returns the match instead.
    pub duplicate_similarity: f64,
    /// Cosine at/above which two unlinked nodes become a suspected conflict.
    pub conflict_suspect_similarity: f64,
    /// Title-NLI path into the suspect queue, below the similarity floor: a
    /// same-subject pair whose titles read as contradicting at this confidence.
    /// `null` disables. See [`crate::policy::CONFLICT_NLI_GATE`].
    #[serde(default = "default_conflict_nli_gate")]
    pub conflict_nli_gate: Option<f64>,
    /// Bridged path: facts about two subjects a bridge note ties ("X uses the same
    /// datastore as Y") queue when "bridge. y ⟹ x" reads as a contradiction at
    /// this confidence. `null` disables. See [`crate::policy::CONFLICT_BRIDGE_GATE`].
    #[serde(default = "default_conflict_bridge_gate")]
    pub conflict_bridge_gate: Option<f64>,
    /// Similarity at/above which a write warns about conflicted/superseded
    /// neighbors.
    pub warn_similarity: f64,
    /// Minimum NLI confidence before an audit sweep queues a pair.
    pub nli_sweep_min_confidence: f64,
    /// The keyword channel's share of relevance (1.0 = pure BM25, 0.0 = pure
    /// vectors). A name-free query scores zero on keywords, so its relevance caps
    /// at `1 - keyword_weight`. See [`crate::policy::SEARCH_KEYWORD_WEIGHT`].
    #[serde(default = "default_keyword_weight")]
    pub keyword_weight: f64,
    /// Cosine below which a vector match contributes nothing; the range above
    /// it is rescaled into 0..1.
    #[serde(default = "default_semantic_floor")]
    pub semantic_floor: f64,
    /// Absolute score below which a hit is dropped.
    #[serde(default = "default_search_min_score")]
    pub search_min_score: f64,
    /// Fraction of the top hit's score below which a hit is dropped.
    #[serde(default = "default_search_relative_cut")]
    pub search_relative_cut: f64,
    /// Candidate-pool multiplier for time-windowed searches; 1 disables. A knob
    /// because the depth depends on how a graph's dates cluster; the vector index
    /// clamps k (`VEC_MAX_K`). See [`crate::policy::SEARCH_WINDOW_OVERFETCH`].
    #[serde(default = "default_window_overfetch")]
    pub window_overfetch: usize,
    /// How much trust tilts the reranker's ordering.
    #[serde(default = "default_rerank_trust_weight")]
    pub rerank_trust_weight: f64,
    /// Minimum contradiction confidence before `check_claim` asserts a
    /// contradiction; below it the verdict is reported as silent. See
    /// [`crate::policy::CLAIM_CONTRADICTION_MIN_CONFIDENCE`].
    #[serde(default = "default_claim_contradiction_min_confidence")]
    pub claim_contradiction_min_confidence: f64,
    /// Damping `k` of the reciprocal-rank vote between retrieval and the
    /// cross-encoder; `None` lets the cross-encoder decide. Move it together with
    /// `keyword_weight`. See [`crate::policy::RERANK_VOTE_K`].
    #[serde(default = "default_rerank_vote_k")]
    pub rerank_vote_k: Option<f64>,
    /// Let a mature graph calibrate its own conflict-suspect floor from its
    /// judged history at session boundaries (see [`crate::Engine::auto_tune`]).
    /// Gated on graph size and judgment volume; smaller graphs always run
    /// the benchmark-calibrated defaults regardless of this flag.
    #[serde(default = "default_auto_tune")]
    pub auto_tune: bool,
    /// Calibrated-delivery trim: post-rerank hits under this score are not
    /// delivered — attention protection, measured free of recall cost. See
    /// [`crate::policy::DELIVERY_FLOOR`].
    #[serde(default = "default_delivery_floor")]
    pub delivery_floor: f64,
    /// Top-score under which the search verdict is "weak" — verify before
    /// relying. See [`crate::policy::WEAK_EVIDENCE_TOP`]. Auto-tune's
    /// weak-line dial recalibrates this per graph from phantom probes.
    #[serde(default = "default_weak_evidence_top")]
    pub weak_evidence_top: f64,
    /// Calibrated-delivery knee trim: cut the delivered list at the largest
    /// relative drop in its score curve when the drop is at least this
    /// fraction of the running score. `None` disables the knee — the fixed
    /// `delivery_floor` alone then trims. See [`crate::policy::KNEE_MIN_CLIFF`].
    #[serde(default = "default_knee_cliff")]
    pub knee_cliff: Option<f64>,
    /// Quantile of phantom-probe top scores that auto-tune's weak-line dial
    /// fits `weak_evidence_top` to. See [`crate::policy::WEAK_LINE_QUANTILE`].
    #[serde(default = "default_weak_line_quantile")]
    pub weak_line_quantile: f64,
    /// How many phantom probes the weak-line fit mints per calibration pass.
    /// See [`crate::policy::WEAK_LINE_PROBES`].
    #[serde(default = "default_weak_line_probes")]
    pub weak_line_probes: usize,
    /// Rerank on title plus full body instead of the keyword snippet (evidence in
    /// an oblique answer may share no keyword). Off until the eval bench proves it.
    #[serde(default)]
    pub rerank_full_note: bool,
    /// Session-diverse delivery: rank positions each additional same-session
    /// hit is demoted when the candidate pool is deeper than the delivery
    /// limit — selection and order only, never scores. `0` disables. See
    /// [`crate::policy::SESSION_DIVERSITY_DEMOTE`].
    #[serde(default = "default_session_diversity_demote")]
    pub session_diversity_demote: f64,
    /// Twin order: among delivered hits whose vectors sit at or
    /// above this cosine — near-identical notes — the endorsed one goes
    /// first (pinned, then higher trust), whatever the vote said. Only twins
    /// trade places; `None` disables. See [`crate::policy::TWIN_TRUST_ORDER`].
    #[serde(default = "default_twin_trust_order")]
    pub twin_trust_order: Option<f64>,
}

pub(super) fn default_twin_trust_order() -> Option<f64> {
    crate::policy::TWIN_TRUST_ORDER
}

pub(super) fn default_session_diversity_demote() -> f64 {
    crate::policy::SESSION_DIVERSITY_DEMOTE
}

pub(super) fn default_delivery_floor() -> f64 {
    crate::policy::DELIVERY_FLOOR
}

pub(super) fn default_weak_evidence_top() -> f64 {
    crate::policy::WEAK_EVIDENCE_TOP
}

pub(super) fn default_conflict_nli_gate() -> Option<f64> {
    Some(crate::policy::CONFLICT_NLI_GATE)
}

pub(super) fn default_conflict_bridge_gate() -> Option<f64> {
    Some(crate::policy::CONFLICT_BRIDGE_GATE)
}

pub(super) fn default_knee_cliff() -> Option<f64> {
    Some(crate::policy::KNEE_MIN_CLIFF)
}

pub(super) fn default_weak_line_quantile() -> f64 {
    crate::policy::WEAK_LINE_QUANTILE
}

pub(super) fn default_weak_line_probes() -> usize {
    crate::policy::WEAK_LINE_PROBES
}

pub(super) fn default_auto_tune() -> bool {
    true
}

pub(super) fn default_keyword_weight() -> f64 {
    crate::policy::SEARCH_KEYWORD_WEIGHT
}

pub(super) fn default_semantic_floor() -> f64 {
    crate::policy::SEARCH_SEMANTIC_FLOOR
}

pub(super) fn default_search_min_score() -> f64 {
    crate::policy::SEARCH_MIN_SCORE
}

pub(super) fn default_search_relative_cut() -> f64 {
    crate::policy::SEARCH_RELATIVE_CUT
}

pub(super) fn default_window_overfetch() -> usize {
    crate::policy::SEARCH_WINDOW_OVERFETCH
}

pub(super) fn default_rerank_trust_weight() -> f64 {
    crate::policy::RERANK_TRUST_WEIGHT
}

pub(super) fn default_claim_contradiction_min_confidence() -> f64 {
    crate::policy::CLAIM_CONTRADICTION_MIN_CONFIDENCE
}

pub(super) fn default_rerank_vote_k() -> Option<f64> {
    crate::policy::RERANK_VOTE_K
}

impl Default for PolicyConfig {
    fn default() -> Self {
        use crate::policy::*;
        Self {
            trust_created: TRUST_UNSEEN_START,
            trust_confirmed: TRUST_CONFIRMED_START,
            trust_approved: TRUST_APPROVED_START,
            trust_approved_floor: TRUST_APPROVED_FLOOR,
            trust_floor: TRUST_FLOOR,
            stale_trust: STALE_TRUST,
            episodic_window_days: PROVISIONAL_TRUST_WINDOW_SECS / 86_400,
            volatile_window_days: VOLATILE_TRUST_WINDOW_SECS / 86_400,
            approved_window_days: APPROVED_TRUST_WINDOW_SECS / 86_400,
            decay_ttl_days: DECAY_TTL_DAYS,
            duplicate_similarity: DUPLICATE_SIMILARITY,
            conflict_suspect_similarity: CONFLICT_SUSPECT_SIMILARITY,
            conflict_nli_gate: Some(CONFLICT_NLI_GATE),
            conflict_bridge_gate: Some(crate::policy::CONFLICT_BRIDGE_GATE),
            warn_similarity: WARN_SIMILARITY,
            nli_sweep_min_confidence: f64::from(NLI_SWEEP_MIN_CONFIDENCE),
            keyword_weight: SEARCH_KEYWORD_WEIGHT,
            semantic_floor: SEARCH_SEMANTIC_FLOOR,
            search_min_score: SEARCH_MIN_SCORE,
            search_relative_cut: SEARCH_RELATIVE_CUT,
            window_overfetch: SEARCH_WINDOW_OVERFETCH,
            rerank_trust_weight: RERANK_TRUST_WEIGHT,
            claim_contradiction_min_confidence: CLAIM_CONTRADICTION_MIN_CONFIDENCE,
            rerank_vote_k: RERANK_VOTE_K,
            auto_tune: true,
            delivery_floor: DELIVERY_FLOOR,
            weak_evidence_top: WEAK_EVIDENCE_TOP,
            knee_cliff: Some(KNEE_MIN_CLIFF),
            weak_line_quantile: WEAK_LINE_QUANTILE,
            weak_line_probes: WEAK_LINE_PROBES,
            rerank_full_note: false,
            session_diversity_demote: SESSION_DIVERSITY_DEMOTE,
            twin_trust_order: TWIN_TRUST_ORDER,
        }
    }
}

// ---------------------------------------------------------------------------
// brief — full composition control (mirrors the engine's section constants)
// ---------------------------------------------------------------------------
