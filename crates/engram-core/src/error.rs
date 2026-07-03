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
    #[error("embedding: {0}")]
    Embedding(String),
}

pub type Result<T> = std::result::Result<T, Error>;
