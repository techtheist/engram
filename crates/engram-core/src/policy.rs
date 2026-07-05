//! Trust & decay tuning (PLAN §6A / §11). These are the concrete numbers the
//! plan left open; kept in one place so they're easy to revisit as we dogfood.

/// Confidence a Claude-created node starts at — provisional until reconfirmed.
pub const PROVISIONAL_CONFIDENCE: f64 = 0.5;
/// Confidence a user-created node starts at — trusted from the start.
pub const USER_CONFIDENCE: f64 = 1.0;
/// At or above this, a node is "trusted" and no longer decays.
pub const TRUSTED_THRESHOLD: f64 = 0.7;
/// How much each reconfirmation (an `update_node` revisit) raises confidence.
pub const RECONFIRM_BUMP: f64 = 0.15;
/// Ceiling for Claude-reconfirmed confidence — only the user reaches 1.0.
pub const CLAUDE_CONFIDENCE_CAP: f64 = 0.9;
/// Default time-to-live before a stale provisional episodic node is archived.
pub const DEFAULT_DECAY_TTL_SECS: i64 = 14 * 24 * 60 * 60; // 14 days
/// Volatile nodes decay at ttl / this divisor (7 days at the default TTL) —
/// volatile is the most perishable durability class.
pub const VOLATILE_TTL_DIVISOR: i64 = 2;
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
