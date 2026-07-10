mod engine;
mod error;
pub mod id;
pub mod policy;
pub mod rag;
pub mod redact;
mod schema;
mod store;
mod types;

pub use engine::{AuditOrigin, ChangeEvent, Engine, Listener};
pub use error::{Error, Result};
#[cfg(feature = "fastembed")]
pub use rag::FastEmbedder;
pub use rag::{Embedder, FakeEmbedder};
pub use store::{Store, normalize_tags, now};
pub use types::*;

#[cfg(test)]
mod tests;
