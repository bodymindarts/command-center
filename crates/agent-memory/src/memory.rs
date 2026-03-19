use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A stored memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub source_task: Option<String>,
    pub source_type: String,
    pub file_path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a new memory.
#[derive(Debug, Clone)]
pub struct NewMemory {
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub source_task: Option<String>,
    pub source_type: String,
}

/// A search result with relevance score.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub memory: Memory,
    pub score: f64,
}
