#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("tepindb: {0}")]
    Tepin(#[from] tepindb::TepinError),
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
    #[error("io: {0}")]
    Io(String),
    /// A project selector the hub can't serve — unknown name/id, a write
    /// addressed to `all`, or multi-project access outside the daemon.
    /// Client error (400 / invalid_params), with the fix in the message.
    #[error("{0}")]
    Project(String),
}

pub type Result<T> = std::result::Result<T, Error>;
