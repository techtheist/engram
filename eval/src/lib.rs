//! Engram's offline evaluation harness.
//!
//! Why it exists: every threshold in this project — the conflict-similarity
//! floor, the sweep's confidence gate, the rerank trust weight, the brief's
//! quotas — was set by dogfooding intuition. There has never been a way to
//! tell whether a change to any of them helped or hurt. This is that way.
//!
//! Why it is synthetic: facts about invented subjects cannot be answered from
//! pretraining, from having read this repository, or from knowing how the tool
//! works. That immunity is the point. Evidence gathered by watching a
//! well-informed agent use a tool it understands measures the agent, not the
//! tool, and no amount of it is worth one clean number.
//!
//! Two halves, deliberately not sharing results:
//!
//! * **offline** (`run`) — no model in the loop at all. Retrieval quality,
//!   token cost, NLI accuracy, and the false-positive rate on questions whose
//!   answer was never written. Free, deterministic, fit for CI.
//! * **online** (`online`) — the contract for the half that needs a live
//!   model. It carries no API client on purpose; grading is a substring check
//!   against a generated answer, so it never needs a judge either.

pub mod arms;
pub mod generate;
pub mod metrics;
pub mod nli_eval;
pub mod online;
pub mod profile;
pub mod rng;
pub mod run;
pub mod variants;
