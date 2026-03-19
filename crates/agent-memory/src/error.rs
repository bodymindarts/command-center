/// Errors for the agent-memory crate.
#[derive(Debug, thiserror::Error)]
pub enum AgentMemoryError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("ambiguous prefix '{0}' matches {1} memories")]
    AmbiguousPrefix(String, usize),

    #[error("{0}")]
    Other(String),
}
