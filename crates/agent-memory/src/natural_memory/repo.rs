use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};

use crate::error::AgentMemoryError;

use super::NaturalMemory;

/// Simple sqlx CRUD repository for natural memories (no event sourcing).
#[derive(Debug, Clone)]
pub struct NaturalMemoryRepo {
    pool: SqlitePool,
}

impl NaturalMemoryRepo {
    pub fn new(pool: &SqlitePool) -> Self {
        Self { pool: pool.clone() }
    }

    /// Insert a new natural memory.
    pub async fn insert(&self, memory: &NaturalMemory) -> Result<(), AgentMemoryError> {
        let tags_json = serde_json::to_string(&memory.tags)?;
        sqlx::query(
            "INSERT OR REPLACE INTO natural_memories
                (id, title, content, tags, project, source_task, source_type,
                 file_path, created_at, updated_at, last_accessed, access_count, pinned)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&memory.id)
        .bind(&memory.title)
        .bind(&memory.content)
        .bind(&tags_json)
        .bind(&memory.project)
        .bind(&memory.source_task)
        .bind(&memory.source_type)
        .bind(&memory.file_path)
        .bind(memory.created_at.to_rfc3339())
        .bind(memory.updated_at.to_rfc3339())
        .bind(memory.last_accessed.map(|dt| dt.to_rfc3339()))
        .bind(memory.access_count)
        .bind(memory.pinned as i32)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Find a memory by exact ID.
    pub async fn find_by_id(&self, id: &str) -> Result<NaturalMemory, AgentMemoryError> {
        let row = sqlx::query(
            "SELECT id, title, content, tags, project, source_task, source_type,
                    file_path, created_at, updated_at, last_accessed, access_count, pinned
             FROM natural_memories WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AgentMemoryError::NotFound(format!("natural memory '{id}'")))?;
        Ok(row_to_natural_memory(&row))
    }

    /// Find memories by ID prefix.
    pub async fn find_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<NaturalMemory>, AgentMemoryError> {
        let pattern = format!("{prefix}%");
        let rows = sqlx::query(
            "SELECT id, title, content, tags, project, source_task, source_type,
                    file_path, created_at, updated_at, last_accessed, access_count, pinned
             FROM natural_memories WHERE id LIKE ?",
        )
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_natural_memory).collect())
    }

    /// Resolve an exact ID or unique prefix to a single memory.
    pub async fn resolve_id(&self, id_or_prefix: &str) -> Result<NaturalMemory, AgentMemoryError> {
        match self.find_by_id(id_or_prefix).await {
            Ok(m) => return Ok(m),
            Err(AgentMemoryError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        let matches = self.find_by_prefix(id_or_prefix).await?;
        match matches.len() {
            0 => Err(AgentMemoryError::NotFound(format!(
                "natural memory '{id_or_prefix}'"
            ))),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => Err(AgentMemoryError::AmbiguousPrefix(
                id_or_prefix.to_string(),
                n,
            )),
        }
    }

    /// Delete a memory by exact ID.
    pub async fn delete(&self, id: &str) -> Result<(), AgentMemoryError> {
        sqlx::query("DELETE FROM natural_memories WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// List memories with optional project filter, ordered by created_at DESC.
    pub async fn list(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<NaturalMemory>, AgentMemoryError> {
        let rows = if let Some(proj) = project {
            sqlx::query(
                "SELECT id, title, content, tags, project, source_task, source_type,
                        file_path, created_at, updated_at, last_accessed, access_count, pinned
                 FROM natural_memories WHERE project = ?
                 ORDER BY created_at DESC LIMIT ?",
            )
            .bind(proj)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, title, content, tags, project, source_task, source_type,
                        file_path, created_at, updated_at, last_accessed, access_count, pinned
                 FROM natural_memories ORDER BY created_at DESC LIMIT ?",
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows.iter().map(row_to_natural_memory).collect())
    }

    /// Get all memory IDs.
    pub async fn all_ids(&self) -> Result<Vec<String>, AgentMemoryError> {
        let rows = sqlx::query("SELECT id FROM natural_memories")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get("id")).collect())
    }

    /// Count total memories.
    pub async fn count(&self) -> Result<u64, AgentMemoryError> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM natural_memories")
            .fetch_one(&self.pool)
            .await?;
        let count: i64 = row.get("cnt");
        Ok(count as u64)
    }

    /// Delete all natural memories (for full reindex).
    pub async fn clear_all(&self) -> Result<(), AgentMemoryError> {
        sqlx::query("DELETE FROM natural_memories")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record access for a batch of memory IDs (reset last_accessed, increment access_count).
    pub async fn record_access(&self, ids: &[String]) -> Result<(), AgentMemoryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "UPDATE natural_memories SET last_accessed = ?1, access_count = access_count + 1
             WHERE id IN ({})",
            placeholders.join(", ")
        );
        let mut query = sqlx::query(&sql).bind(&now);
        for id in ids {
            query = query.bind(id);
        }
        query.execute(&self.pool).await?;
        Ok(())
    }

    /// Set the pinned status for a memory.
    pub async fn set_pinned(&self, id: &str, pinned: bool) -> Result<(), AgentMemoryError> {
        let result = sqlx::query("UPDATE natural_memories SET pinned = ? WHERE id = ?")
            .bind(pinned as i32)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(AgentMemoryError::NotFound(format!("natural memory '{id}'")));
        }
        Ok(())
    }

    /// Fetch memories by a list of IDs.
    pub async fn get_by_ids(&self, ids: &[String]) -> Result<Vec<NaturalMemory>, AgentMemoryError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT id, title, content, tags, project, source_task, source_type,
                    file_path, created_at, updated_at, last_accessed, access_count, pinned
             FROM natural_memories WHERE id IN ({})",
            placeholders.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for id in ids {
            query = query.bind(id);
        }
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows.iter().map(row_to_natural_memory).collect())
    }
}

fn row_to_natural_memory(row: &sqlx::sqlite::SqliteRow) -> NaturalMemory {
    let tags_json: String = row.get("tags");
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let created_str: String = row.get("created_at");
    let updated_str: String = row.get("updated_at");
    let last_accessed_str: Option<String> = row.get("last_accessed");
    let pinned_int: i32 = row.get("pinned");

    NaturalMemory {
        id: row.get("id"),
        title: row.get("title"),
        content: row.get("content"),
        tags,
        project: row.get("project"),
        source_task: row.get("source_task"),
        source_type: row.get("source_type"),
        file_path: row.get("file_path"),
        created_at: parse_dt(&created_str),
        updated_at: parse_dt(&updated_str),
        last_accessed: last_accessed_str.as_deref().map(parse_dt),
        access_count: row.get("access_count"),
        pinned: pinned_int != 0,
    }
}

fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_default()
}
