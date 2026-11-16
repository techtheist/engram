//! Trust and retrieval tuning: the shipped defaults, in one place. The live
//! numbers come from each graph's `PolicyConfig`; every value here cites the
//! measurement that set it (receipts under `eval/results/`).
//!
//! Trust is computed at read time from timestamps, never by a sweep:
//! - **Time doesn't validate.** `stable` notes hold flat until a live
//!   `conflicts-with` stamps `demoted_at`, then ramp down; withdrawing the
//!   edge withdraws the demotion. Drift never demotes (it depends on the
//!   checkout). Episodic/volatile notes ramp with age; open Problems and
//!   Intents never decay.
//! - **Exposure doesn't validate.** Retrieval stamps `last_seen` only. Trust
//!   anchors on `confirmed_at` (an update or "Confirm still true") or
//!   `approved_at`, so a popular wrong note can't keep itself alive.
//!
//! Starting values: created only → [`TRUST_UNSEEN_START`]; confirmed →
//! [`TRUST_CONFIRMED_START`]; approved → [`TRUST_APPROVED_START`], ramping to
//! [`TRUST_APPROVED_FLOOR`]. A pin (`trust_override`) wins outright and is never
//! decayed or archived. Below [`STALE_TRUST`] a node is stale: still searchable,
//! flagged, and queued for human review.

use crate::config::PolicyConfig;
use crate::types::{Durability, NodeStatus};

/// Starting trust for a node that was never confirmed or approved.
pub const TRUST_UNSEEN_START: f64 = 0.5;
/// Starting trust after a deliberate confirm (never after retrieval).
pub const TRUST_CONFIRMED_START: f64 = 0.6;
/// Where unapproved trust bottoms out.
pub const TRUST_FLOOR: f64 = 0.01;
/// Unapproved episodic trust runs start → floor over half a year.
pub const PROVISIONAL_TRUST_WINDOW_SECS: i64 = 183 * 24 * 60 * 60;
/// Volatile notes rot fast: one month from start to floor.
pub const VOLATILE_TRUST_WINDOW_SECS: i64 = 30 * 24 * 60 * 60;
/// Trust the moment a node is approved.
pub const TRUST_APPROVED_START: f64 = 1.0;
/// Approved trust never fully expires.
pub const TRUST_APPROVED_FLOOR: f64 = 0.2;
/// Approved (non-stable) trust runs its course over a year.
pub const APPROVED_TRUST_WINDOW_SECS: i64 = 365 * 24 * 60 * 60;
/// Below this computed trust a node is stale.
pub const STALE_TRUST: f64 = 0.3;

/// Same-type cosine at/above which `add_note` returns the existing match.
pub const DUPLICATE_SIMILARITY: f64 = 0.90;
/// Cosine at/above which two unlinked notes become a suspected conflict. Fitted
/// on the dogfood graph's judged suspects: 26 of 27 dismissals sat ≤ 0.883, the
/// one real conflict at 0.916. Widen only once the positive class grows.
pub const CONFLICT_SUSPECT_SIMILARITY: f64 = 0.88;
/// Second way into the suspect queue: below the similarity floor, a pair is
/// still nominated when NLI reads the two titles as contradicting at this
/// confidence and they name the same subject. No similarity channel separates
/// contradiction from restatement (AUROC 0.59); title NLI does. With the strict
/// subject guard and [`CONFLICT_NLI_OVERLAP`], it kept 8/15 planted positives
/// and let 1 of the dogfood graph's 56 false alarms through. `None` disables.
pub const CONFLICT_NLI_GATE: f64 = 0.80;
/// Bridged contradictions: a pair about two subjects that a bridge note ties
/// queues when NLI reads "bridge. y ⟹ x" as a contradiction at this confidence.
/// 0 false alarms on the dogfood graph (eval Finding 00ge9s5nwc1z). `None`
/// disables.
pub const CONFLICT_BRIDGE_GATE: f64 = 0.80;
/// Bridged pairs one write may judge, closest first.
pub const BRIDGE_CHECK_CAP: usize = 24;
/// Content words the two titles must share beyond their common subject before
/// title NLI may nominate them: a contradiction restates a claim, while two
/// facts about one subject share only the subject (the shape that flooded the
/// queue). Words compare on their first five characters.
pub const CONFLICT_NLI_OVERLAP: usize = 1;
/// Title-NLI candidates must still be this similar: every NLI call costs about
/// 16 ms, and planted positives sat at 0.64+.
pub const CONFLICT_NLI_FLOOR: f64 = 0.50;
/// Auto-tune (per-graph conflict-floor calibration) engages only past this many
/// notes; smaller graphs keep the benchmark defaults.
pub const AUTO_TUNE_MIN_NOTES: i64 = 200;
/// ...and with at least this many judged suspect pairs,
pub const AUTO_TUNE_MIN_JUDGED: usize = 20;
/// ...with this many on each side: one-sided history can't place a floor.
pub const AUTO_TUNE_MIN_EACH: usize = 3;
/// The fitted conflict floor never drops below this (topical-cluster noise).
pub const AUTO_TUNE_FLOOR_MIN: f64 = 0.85;
/// Fitted moves smaller than this are ignored.
pub const AUTO_TUNE_MIN_DELTA: f64 = 0.005;
/// Each auto-tune move goes this fraction of the way to the fresh fit, so one
/// noisy fit can't teleport a dial; consistent fits still converge in a few runs.
pub const AUTO_TUNE_DAMPING: f64 = 0.5;
/// Reranked hits under this score are not delivered. Top of the measured free
/// zone: recall@5 and oblique recall unchanged at 100/500/1500 notes, ~20%
/// fewer hits delivered. Not abstention (that would cost 38% of real answers at
/// 500 notes); abstention is a label, see [`WEAK_EVIDENCE_TOP`].
pub const DELIVERY_FLOOR: f64 = 0.22;
/// A delivery whose best hit scores under this is labeled weak evidence
/// (best separation threshold 0.84–0.87 across 500–1500 notes).
pub const WEAK_EVIDENCE_TOP: f64 = 0.85;
/// Knee cut: after the floor, cut the delivered list at its largest relative
/// score drop if that drop is at least this fraction (arXiv:2606.11907,
/// simplified). Recall within 0.01 of the floor alone; focus 0.20 → 0.66 and
/// 35–50% fewer tokens, and the cliff sharpens as the graph grows.
pub const KNEE_MIN_CLIFF: f64 = 0.25;
/// Session-diverse delivery: each extra hit from an already-represented
/// session drops this many ranks before the cut, so other sessions' evidence
/// gets the slots. Rank-based because score thresholds don't transfer between
/// graphs. At 2, top-5 session coverage rose 0.63 → 0.79 with recall unchanged
/// (`eval --sessions`). `0` disables.
pub const SESSION_DIVERSITY_DEMOTE: f64 = 2.0;
/// The weak line is fitted per graph as this quantile of the top scores that
/// phantom probes (questions about invented subjects) reach. q90 flags 84–95%
/// of never-written questions at every measured size.
pub const WEAK_LINE_QUANTILE: f64 = 0.90;
/// Phantom probes minted per weak-line fit.
pub const WEAK_LINE_PROBES: usize = 24;
/// The weak-line fit engages past this many notes. Lower than
/// [`AUTO_TUNE_MIN_NOTES`] because small graphs are the ones the fixed 0.85
/// over-flags.
pub const WEAK_LINE_MIN_NOTES: i64 = 50;
/// The fitted weak line stays at least this far above `delivery_floor` (or it
/// could never fire). A margin, not a quality bar: the score scale depends on
/// how the graph's notes are written.
pub const WEAK_LINE_ABOVE_FLOOR: f64 = 0.03;
/// Fitted weak lines never exceed this; above it the label fires on everything.
pub const WEAK_LINE_MAX: f64 = 0.95;
/// Days a node must stay below [`STALE_TRUST`] before decay archives it.
pub const DECAY_TTL_DAYS: i64 = 14;
/// Similarity at/above which a write warns about nearby conflicted or
/// superseded nodes.
pub const WARN_SIMILARITY: f64 = 0.70;
/// Character budget for the session-start brief (~4k tokens).
pub const DEFAULT_BRIEF_CHARS: usize = 16000;
/// The keyword channel's share of a hybrid score; the rest is vector
/// similarity. Name-free queries score zero on keywords, so a high weight
/// sinks them: 0.15 with the reranker vote is +0.17 oblique recall at no cost
/// to named queries. Not zero, because keywords also make the FTS snippet.
pub const SEARCH_KEYWORD_WEIGHT: f64 = 0.15;

/// Cosine below which a vector hit carries no signal (bge-small packs unrelated
/// pairs into about [0.5, 0.7]); the semantic part rescales from here to 1.0.
pub const SEARCH_SEMANTIC_FLOOR: f64 = 0.6;
/// Hybrid hits under this are dropped, so an unrelated query returns nothing.
pub const SEARCH_MIN_SCORE: f64 = 0.1;
/// Hits under this fraction of the best hit are dropped (the FTS OR-tail).
pub const SEARCH_RELATIVE_CUT: f64 = 0.25;
/// Newness bonus ceiling: breaks near-ties toward current knowledge.
pub const SEARCH_RECENCY_BOOST: f64 = 0.15;
/// Age at which the newness bonus has halved.
pub const SEARCH_RECENCY_HALF_LIFE_SECS: i64 = 30 * 24 * 60 * 60;
/// Candidate-pool multiplier for time-windowed searches (neither channel is
/// date-ordered). 2 is the cheapest depth that is never worse; depth beyond it
/// is dead weight (`eval --window`, three seeds).
pub const SEARCH_WINDOW_OVERFETCH: usize = 2;
/// How much trust modulates a reranked score: relevance dominates, trust breaks
/// near-ties.
pub const RERANK_TRUST_WEIGHT: f64 = 0.15;
/// Cosine at/above which two hits are twins and the pinned or more trusted one
/// goes first: the endorsed twin ranked first 0.575 → 1.00 with no recall cost
/// (`eval --authority`, 2026-09-27). 0.95 misses twins.
pub const TWIN_TRUST_ORDER: Option<f64> = Some(0.90);
/// Damping `k` of the reciprocal-rank vote between retrieval order and the
/// cross-encoder, so one confident reranker mistake must out-vote two channels.
/// Worth +0.065 on name-free queries, but only at a low
/// [`SEARCH_KEYWORD_WEIGHT`]; change the two together. `None` lets the
/// cross-encoder decide alone.
pub const RERANK_VOTE_K: Option<f64> = Some(10.0);
/// Contradiction confidence before `check_claim` reports a claim as
/// contradicted; below it the verdict is `silent` (probabilities still ride
/// along). Picked over five seeds (`eval/CONTRADICTIONS.md`) as the last gate
/// before catch comes apart:
///
/// ```text
///   gate   catch (min)  spread   false alarms   agreeing claims
///                                                called conflicts
///   0.70      95 (94)        1     44            2-6%
///   0.80      92 (90)        4     38            1-2%   <- shipped
///   0.90      85 (79)       11     27            0-2%
/// ```
pub const CLAIM_CONTRADICTION_MIN_CONFIDENCE: f64 = 0.80;

/// NLI confidence before an audit sweep queues a pair. MNLI models presuppose
/// co-reference, so unrelated same-shaped titles read as confident
/// contradictions; the sweep therefore also keeps
/// [`CONFLICT_SUSPECT_SIMILARITY`] as its floor. Calibrate on judged suspects
/// before loosening either.
pub const NLI_SWEEP_MIN_CONFIDENCE: f32 = 0.8;
/// Score multiplier on hits from other projects, so local canon wins ties.
pub const CROSS_PROJECT_PRIOR: f64 = 0.85;
/// Same-type cosine at/above which a Principle/Caution recurring in another
/// project becomes a promotion nomination for the home graph.
pub const PROMOTION_SIMILARITY: f64 = 0.85;
/// Brief characters reserved for the home-graph section, so user-level canon
/// never starves the project's own sections.
pub const HOME_BRIEF_RESERVE: usize = 1500;

/// Everything the trust computation reads off a node. `last_seen` is
/// deliberately absent: retrieval is observability, not evidence.
#[derive(Debug, Clone, Copy)]
pub struct TrustInputs {
    pub created_at: i64,
    pub confirmed_at: Option<i64>,
    pub approved_at: Option<i64>,
    pub demoted_at: Option<i64>,
    pub trust_override: Option<f64>,
    pub durability: Durability,
    pub status: Option<NodeStatus>,
}

/// Linear ramp from `start` at age 0 to `floor` at `window`, clamped.
fn ramp(start: f64, floor: f64, window: i64, age: i64) -> f64 {
    if age <= 0 {
        return start;
    }
    if age >= window {
        return floor;
    }
    start - (start - floor) * (age as f64 / window as f64)
}

fn provisional_window(durability: Durability, p: &PolicyConfig) -> i64 {
    match durability {
        Durability::Volatile => p.volatile_window_days * 86_400,
        // Stable only ramps after a demotion; it borrows the episodic window.
        Durability::Stable | Durability::Episodic => p.episodic_window_days * 86_400,
    }
}

/// The computed trust of a node at `now` under the graph's policy (model in
/// the module docs).
pub fn trust(n: &TrustInputs, now: i64, p: &PolicyConfig) -> f64 {
    if let Some(o) = n.trust_override {
        return o.clamp(0.0, 1.0);
    }
    let (start, anchor, floor, window) = match (n.approved_at, n.confirmed_at) {
        (Some(a), _) => (
            p.trust_approved,
            a,
            p.trust_approved_floor,
            p.approved_window_days * 86_400,
        ),
        (None, Some(c)) => (
            p.trust_confirmed,
            c,
            p.trust_floor,
            provisional_window(n.durability, p),
        ),
        (None, None) => (
            p.trust_created,
            n.created_at,
            p.trust_floor,
            provisional_window(n.durability, p),
        ),
    };
    // Live worklist: an open Problem/Intent is never buried by age.
    if n.status == Some(NodeStatus::Open) {
        return start;
    }
    match (n.durability, n.demoted_at) {
        // Stable knowledge doesn't rot with time — only evidence moves it.
        (Durability::Stable, None) => start,
        // Demoted: the ramp runs from the evidence event, not the anchor.
        (Durability::Stable, Some(d)) => ramp(start, floor, window, now - d.max(anchor)),
        _ => ramp(start, floor, window, now - anchor),
    }
}

pub fn is_stale(trust: f64, p: &PolicyConfig) -> bool {
    trust < p.stale_trust
}

/// When computed trust crosses the stale threshold (the decay TTL's clock).
/// `None` for nodes that never cross on their own: approved, pinned, stable,
/// and open worklist.
pub fn stale_since(n: &TrustInputs, p: &PolicyConfig) -> Option<i64> {
    if n.approved_at.is_some()
        || n.trust_override.is_some()
        || n.status == Some(NodeStatus::Open)
        || n.durability == Durability::Stable
    {
        return None;
    }
    // A tuned floor at/above the stale threshold means trust never crosses
    // it — such nodes bottom out but are never stale, so they never decay.
    if p.trust_floor >= p.stale_trust {
        return None;
    }
    let (start, anchor) = match n.confirmed_at {
        Some(c) => (p.trust_confirmed, c),
        None => (p.trust_created, n.created_at),
    };
    let window = provisional_window(n.durability, p);
    let fraction = ((start - p.stale_trust) / (start - p.trust_floor)).clamp(0.0, 1.0);
    Some(anchor + (window as f64 * fraction) as i64)
}
