#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid {kind}: {value:?}")]
    Parse { kind: &'static str, value: String },
    /// The operation would override a user pin — pinned knowledge is
    /// user-managed; the assistant must surface it instead of acting.
    #[error("pinned: {0}")]
    Pinned(String),
    #[error("embedding: {0}")]
    Embedding(String),
}

pub type Result<T> = std::result::Result<T, Error>;
