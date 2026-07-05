//! Trust tuning (PLAN §6A / §11). These are the concrete numbers the plan left
//! open; kept in one place so they're easy to revisit as we dogfood.
//!
//! Trust is **computed at read time** from three timestamps — nothing depends
//! on a background process having run:
//! - only `created_at`: starts at [`TRUST_UNSEEN_START`], linear to
//!   [`TRUST_FLOOR`] over [`PROVISIONAL_TRUST_WINDOW_SECS`]
//! - `last_seen` set (surfaced by search/brief): starts at
//!   [`TRUST_SEEN_START`], same window, clock restarts at `last_seen`
//! - `approved_at` set (user approval, or assistant approval on explicit user
//!   demand): starts at [`TRUST_APPROVED_START`], linear to
//!   [`TRUST_APPROVED_FLOOR`] over [`APPROVED_TRUST_WINDOW_SECS`]
//!
//! A node whose computed trust falls below [`STALE_TRUST`] is **stale**: still
//! searchable (buried by the trust multiplier), flagged to the assistant, and
//! surfaced in the pane's review queue for a human decision.

/// Starting trust for a node that has never been surfaced or approved.
pub const TRUST_UNSEEN_START: f64 = 0.5;
/// Starting trust once retrieval has surfaced the node (it proved findable).
pub const TRUST_SEEN_START: f64 = 0.6;
/// Where unapproved trust bottoms out.
pub const TRUST_FLOOR: f64 = 0.01;
/// Unapproved trust runs its start→floor course over half a year.
pub const PROVISIONAL_TRUST_WINDOW_SECS: i64 = 183 * 24 * 60 * 60;
/// Trust the moment a node is approved.
pub const TRUST_APPROVED_START: f64 = 1.0;
/// Where approved trust bottoms out — approval never fully expires.
pub const TRUST_APPROVED_FLOOR: f64 = 0.2;
/// Approved trust runs its course over a full year.
pub const APPROVED_TRUST_WINDOW_SECS: i64 = 365 * 24 * 60 * 60;
/// Below this computed trust a node is stale (needs review or re-approval).
pub const STALE_TRUST: f64 = 0.3;

/// Same-type cosine similarity at/above which `add_note` treats the new note
/// as a duplicate and returns the existing match instead of creating (PLAN §6A).
pub const DUPLICATE_SIMILARITY: f64 = 0.90;
/// Similarity at/above which a write warns about nearby conflicted or
/// superseded nodes (the pull-based form of PLAN §7 conflict surfacing).
pub const WARN_SIMILARITY: f64 = 0.70;
/// Character budget for the session-start brief (~3k tokens).
pub const DEFAULT_BRIEF_CHARS: usize = 12000;
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

/// The computed trust of a node at `now` (see module docs for the model).
pub fn trust(created_at: i64, last_seen: Option<i64>, approved_at: Option<i64>, now: i64) -> f64 {
    if let Some(approved) = approved_at {
        return ramp(
            TRUST_APPROVED_START,
            TRUST_APPROVED_FLOOR,
            APPROVED_TRUST_WINDOW_SECS,
            now - approved,
        );
    }
    if let Some(seen) = last_seen {
        return ramp(
            TRUST_SEEN_START,
            TRUST_FLOOR,
            PROVISIONAL_TRUST_WINDOW_SECS,
            now - seen,
        );
    }
    ramp(
        TRUST_UNSEEN_START,
        TRUST_FLOOR,
        PROVISIONAL_TRUST_WINDOW_SECS,
        now - created_at,
    )
}

pub fn is_stale(trust: f64) -> bool {
    trust < STALE_TRUST
}
