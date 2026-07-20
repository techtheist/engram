pub mod cortex;
pub mod digest;
mod engine;
mod error;
pub mod harness;
mod hub;
pub mod id;
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

pub use engine::{AuditOrigin, ChangeEvent, EMBED_COMPOSITION, Engine, Listener};
pub use error::{Error, Result};
pub use hub::{EngineFactory, Hub, ListenerFactory, ProjectHandle};
#[cfg(feature = "fastembed")]
pub use nli::FastNli;
pub use nli::{FakeNli, Nli, NliJudgment, SymmetricJudgment};
pub use rag::{Embedder, FakeEmbedder, Reranker};
#[cfg(feature = "fastembed")]
pub use rag::{FastEmbedder, FastReranker};
pub use store::{
    SNIPPET_CLOSE, SNIPPET_OPEN, Store, normalize_tags, now, open_store, resolve_db_path,
};
pub use store_sqlite::SqliteStore;
pub use store_tepin::{TepinStore, is_tepin_path};
pub use types::*;

#[cfg(test)]
mod tests;
