mod repo;

pub use repo::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A stored memory — observation, decision, research finding, or knowledge.
///
/// Memories decay over time unless pinned or persistent.
/// Persistent memories are exempt from decay (replaces the old Report type).
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
    pub last_accessed: Option<DateTime<Utc>>,
    pub access_count: i64,
    pub pinned: bool,
    /// Persistent memories are exempt from decay (like former reports).
    pub persistent: bool,
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
    /// When true, the memory is exempt from decay.
    pub persistent: bool,
}
