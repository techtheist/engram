//! Trust tuning (PLAN §6A / §11). These are the concrete numbers the plan left
//! open; kept in one place so they're easy to revisit as we dogfood.
//!
//! Trust is **computed at read time** — nothing depends on a background
//! process having run. Two principles fix which signals may move it
//! (both learned from external feedback on v0.4.1):
//!
//! - **Time doesn't validate.** `stable` knowledge holds its trust flat until
//!   a live `conflicts-with` edge (the judged-evidence event) stamps
//!   `demoted_at`; only then does it ramp down, and withdrawing the evidence
//!   (edge resolved/dismissed/deleted) withdraws the demotion. Drift is
//!   surfaced for review but deliberately never demotes — it is environment-
//!   dependent (wrong cwd, feature branches) and a sticky stamp from a bad
//!   scan would mass-bury the graph. Episodic/volatile knowledge genuinely
//!   rots, so it keeps a pure time ramp. Open Problems/Intents are live
//!   worklist and never decay while open.
//! - **Exposure doesn't validate.** Retrieval (search hits, brief inclusion)
//!   stamps `last_seen` for observability only — it proves a note was
//!   *findable*, not that it is true. Trust anchors on `confirmed_at`, which
//!   only deliberate acts refresh: an update (edits are re-validation) or an
//!   approval. Otherwise a broad recurring query would keep an attractive but
//!   wrong note alive forever — retrieval certifying its own outputs.
//!
//! The anchor picks the starting value:
//! - only `created_at`: starts at [`TRUST_UNSEEN_START`]
//! - `confirmed_at` set (deliberate update / "Confirm still true"): starts at
//!   [`TRUST_CONFIRMED_START`], clock restarts at `confirmed_at`
//! - `approved_at` set (user approval, or assistant approval on explicit user
//!   demand): starts at [`TRUST_APPROVED_START`], ramps to
//!   [`TRUST_APPROVED_FLOOR`] over [`APPROVED_TRUST_WINDOW_SECS`]
//!
//! `trust_override` (the pane's pin) short-circuits everything: pin = 1.0,
//! arbitrary constant values are allowed. Pinned nodes never decay, never
//! auto-archive, and evidence events skip them — a human said "forever", so
//! only a human unsays it (contradictions still surface in review).
//!
//! A node whose computed trust falls below [`STALE_TRUST`] is **stale**: still
//! searchable (buried by the trust multiplier), flagged to the assistant, and
//! surfaced in the pane's review queue for a human decision.

use crate::types::{Durability, NodeStatus};

/// Starting trust for a node that was never confirmed or approved.
pub const TRUST_UNSEEN_START: f64 = 0.5;
/// Starting trust once a deliberate act confirmed the node (an update or an
/// explicit "Confirm still true" — NOT retrieval, which proves nothing).
pub const TRUST_CONFIRMED_START: f64 = 0.6;
/// Where unapproved trust bottoms out.
pub const TRUST_FLOOR: f64 = 0.01;
/// Unapproved episodic trust runs its start→floor course over half a year.
pub const PROVISIONAL_TRUST_WINDOW_SECS: i64 = 183 * 24 * 60 * 60;
/// Volatile notes rot fast: one month from start to floor.
pub const VOLATILE_TRUST_WINDOW_SECS: i64 = 30 * 24 * 60 * 60;
/// Trust the moment a node is approved.
pub const TRUST_APPROVED_START: f64 = 1.0;
/// Where approved trust bottoms out — approval never fully expires.
pub const TRUST_APPROVED_FLOOR: f64 = 0.2;
/// Approved (non-stable) trust runs its course over a full year.
pub const APPROVED_TRUST_WINDOW_SECS: i64 = 365 * 24 * 60 * 60;
/// Below this computed trust a node is stale (needs review or re-approval).
pub const STALE_TRUST: f64 = 0.3;

/// Same-type cosine similarity at/above which `add_note` treats the new note
/// as a duplicate and returns the existing match instead of creating (PLAN §6A).
pub const DUPLICATE_SIMILARITY: f64 = 0.90;
/// Cosine similarity at/above which two *unlinked* nodes become a suspected
/// conflict (PLAN §7 conflict scan): close enough to be about the same thing,
/// below the duplicate bar. Judgment stays with Claude or the user. Retuned on
/// the dogfood graph twice: 0.75 flagged every topical cluster (136 pairs / ~50
/// nodes), so it moved to 0.85; but same-genre notes share so much domain
/// vocabulary that bge-small parks topically-distinct pairs at 0.85–0.87, and
/// the judged-suspects history bore it out — 27 verdicts, 26 dismissed (all
/// ≤ 0.883) and the single confirmed conflict at 0.916, a 96% false-positive
/// rate with a clean gap above the noise. Raised to 0.88 (2026-07-13): drops
/// every observed false positive, keeps the real one. Positive class is thin
/// (n=1), so widen only after the judged-suspects corpus grows.
pub const CONFLICT_SUSPECT_SIMILARITY: f64 = 0.88;
/// How long a node must sit below [`STALE_TRUST`] before the decay pass
/// archives it (PLAN §6B: stale provisional episodic nodes decay out).
pub const DECAY_TTL_DAYS: i64 = 14;
/// Similarity at/above which a write warns about nearby conflicted or
/// superseded nodes (the pull-based form of PLAN §7 conflict surfacing).
pub const WARN_SIMILARITY: f64 = 0.70;
/// Character budget for the session-start brief (~4k tokens).
pub const DEFAULT_BRIEF_CHARS: usize = 16000;
/// Cosine similarity below which a vector hit carries no semantic signal.
/// bge-small compresses unrelated pairs into roughly [0.5, 0.7]; the semantic
/// component rescales from this floor to 1.0 and is zero underneath it.
pub const SEARCH_SEMANTIC_FLOOR: f64 = 0.6;
/// Hybrid hits scoring below this are dropped — an unrelated query should
/// return nothing, not the least-unrelated node.
pub const SEARCH_MIN_SCORE: f64 = 0.1;
/// Hits scoring below this fraction of the best hit are dropped — the weak
/// FTS OR-recall tail that rides in behind one genuinely relevant match.
pub const SEARCH_RELATIVE_CUT: f64 = 0.25;
/// Newness bonus ceiling in hybrid search: a just-created node scores up to
/// this fraction higher than an otherwise-identical old one. Multiplicative
/// and small — relevance still dominates; this only breaks near-ties in
/// favor of current knowledge.
pub const SEARCH_RECENCY_BOOST: f64 = 0.15;
/// Age at which the newness bonus has halved.
pub const SEARCH_RECENCY_HALF_LIFE_SECS: i64 = 30 * 24 * 60 * 60; // 30 days
/// How much trust modulates a reranked hit's score (mirrors the trust weight
/// inside the hybrid blend): relevance dominates, trust breaks near-ties.
pub const RERANK_TRUST_WEIGHT: f64 = 0.15;
/// Minimum NLI confidence before an audit sweep queues a pair. One of two
/// guards learned on the dogfood graph (2026-07-13): MNLI-class models
/// presuppose co-reference, so below the ~0.85 similarity band unrelated
/// same-shaped titles read as CONFIDENT contradictions (140 junk pairs even
/// at this gate, most at 0.99–1.00) — which is why the conflict sweep also
/// keeps [`CONFLICT_SUSPECT_SIMILARITY`] as its floor. A sweep that floods
/// the judge is worse than one that misses; calibrate on the judged-suspects
/// corpus (scripts/nli-eval.py) before loosening either guard.
pub const NLI_SWEEP_MIN_CONFIDENCE: f32 = 0.8;

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

fn provisional_window(durability: Durability) -> i64 {
    match durability {
        Durability::Volatile => VOLATILE_TRUST_WINDOW_SECS,
        // Stable only ramps after a demotion; it borrows the episodic window.
        Durability::Stable | Durability::Episodic => PROVISIONAL_TRUST_WINDOW_SECS,
    }
}

/// The computed trust of a node at `now` (see module docs for the model).
pub fn trust(n: &TrustInputs, now: i64) -> f64 {
    if let Some(o) = n.trust_override {
        return o.clamp(0.0, 1.0);
    }
    let (start, anchor, floor, window) = match (n.approved_at, n.confirmed_at) {
        (Some(a), _) => (
            TRUST_APPROVED_START,
            a,
            TRUST_APPROVED_FLOOR,
            APPROVED_TRUST_WINDOW_SECS,
        ),
        (None, Some(c)) => (
            TRUST_CONFIRMED_START,
            c,
            TRUST_FLOOR,
            provisional_window(n.durability),
        ),
        (None, None) => (
            TRUST_UNSEEN_START,
            n.created_at,
            TRUST_FLOOR,
            provisional_window(n.durability),
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

pub fn is_stale(trust: f64) -> bool {
    trust < STALE_TRUST
}

/// When a node's computed trust crosses [`STALE_TRUST`] — the clock the decay
/// pass measures its TTL against. `None` for nodes that never cross on their
/// own: approved or pinned (PLAN §6B — confirmed/trusted persist), stable
/// (decays only on evidence, and never auto-archives), and open worklist.
pub fn stale_since(n: &TrustInputs) -> Option<i64> {
    if n.approved_at.is_some()
        || n.trust_override.is_some()
        || n.status == Some(NodeStatus::Open)
        || n.durability == Durability::Stable
    {
        return None;
    }
    let (start, anchor) = match n.confirmed_at {
        Some(c) => (TRUST_CONFIRMED_START, c),
        None => (TRUST_UNSEEN_START, n.created_at),
    };
    let window = provisional_window(n.durability);
    let fraction = (start - STALE_TRUST) / (start - TRUST_FLOOR);
    Some(anchor + (window as f64 * fraction) as i64)
}
