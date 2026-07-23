pub mod config;
pub mod cortex;
pub mod digest;
mod engine;
mod error;
pub mod harness;
mod hub;
pub mod id;
mod migrate;
pub mod nli;
pub mod policy;
pub mod rag;
pub mod redact;
pub mod registry;
mod schema;
mod store;
mod store_sqlite;
mod store_tepin;
mod types;

pub use config::GraphConfig;
#[cfg(test)]
pub(crate) use engine::claim_texts as engine_claims_for_tests;
pub use engine::{AuditOrigin, ChangeEvent, EMBED_COMPOSITION, Engine, Listener};
pub use error::{Error, Result};
pub use hub::{ConflictAlert, ConflictFeed, EngineFactory, Hub, ListenerFactory, ProjectHandle};
pub use migrate::{MigrationSummary, migrate_to_tepin};
#[cfg(feature = "fastembed")]
pub use nli::FastNli;
pub use nli::{FakeNli, Nli, NliJudgment, SymmetricJudgment};
pub use rag::{Embedder, FakeEmbedder, Reranker};
#[cfg(feature = "fastembed")]
pub use rag::{FastEmbedder, FastReranker};
pub use store::{
    SNIPPET_CLOSE, SNIPPET_OPEN, Store, normalize_tags, now, open_store, parse_day, resolve_db_path,
};
pub use store_sqlite::SqliteStore;
pub use store_tepin::{TepinStore, is_tepin_path};
pub use types::*;

#[cfg(test)]
mod tests;

/// One brief-style line for a node (`- Title [Type id …] — excerpt`) — the
/// shared record shape the HTTP layer reuses for hook injections.
pub fn brief_line(n: &Node) -> String {
    engine::node_line(n, engine::EXCERPT_CHARS)
}
