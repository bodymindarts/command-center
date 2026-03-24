use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::embed::DIMENSIONS;
use crate::error::AgentMemoryError;

/// FTS keyword search result before merging.
#[derive(Debug)]
pub struct FtsResult {
    pub id: String,
    pub rank: f64,
}

/// Vector similarity search result before merging.
#[derive(Debug)]
pub struct VecResult {
    pub id: String,
    pub distance: f64,
}

/// Manages the search projections (FTS5 + sqlite-vec) and pool lifecycle.
pub struct SearchStore {
    pool: SqlitePool,
}

impl std::fmt::Debug for SearchStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchStore").finish_non_exhaustive()
    }
}

impl SearchStore {
    /// Open or create the database, loading sqlite-vec extension and running migrations.
    pub async fn open(db_path: &Path) -> Result<Self, AgentMemoryError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Register sqlite-vec as auto-extension BEFORE creating the pool.
        unsafe {
            libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut libsqlite3_sys::sqlite3,
                    *mut *mut i8,
                    *const libsqlite3_sys::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .map_err(|e| AgentMemoryError::Other(format!("failed to open memory db: {e}")))?;

        // Run embedded sqlx migrations.
        sqlx::migrate!().run(&pool).await?;

        // Create vec0 virtual table at runtime (dimension is configurable).
        Self::ensure_vec_table(&pool).await?;

        Ok(Self { pool })
    }

    /// Get the underlying pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Create the sqlite-vec virtual table if it doesn't exist.
    async fn ensure_vec_table(pool: &SqlitePool) -> Result<(), AgentMemoryError> {
        let sql = format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(
                memory_id TEXT PRIMARY KEY,
                embedding float[{DIMENSIONS}] distance_metric=cosine
            )"
        );
        sqlx::query(&sql).execute(pool).await?;
        Ok(())
    }

    // ── FTS5 projection management ──────────────────────────────────

    /// Insert or update an FTS5 entry for a memory.
    pub async fn upsert_fts(
        &self,
        memory_id: &str,
        title: &str,
        content: &str,
        tags: &str,
    ) -> Result<(), AgentMemoryError> {
        // Delete existing entry first (FTS5 standalone tables don't support UPDATE).
        sqlx::query("DELETE FROM memory_fts WHERE memory_id = ?")
            .bind(memory_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "INSERT INTO memory_fts(memory_id, memory_type, title, content, tags)
             VALUES (?, 'memory', ?, ?, ?)",
        )
        .bind(memory_id)
        .bind(title)
        .bind(content)
        .bind(tags)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete an FTS5 entry.
    pub async fn delete_fts(&self, memory_id: &str) -> Result<(), AgentMemoryError> {
        sqlx::query("DELETE FROM memory_fts WHERE memory_id = ?")
            .bind(memory_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Full-text keyword search, returning ranked results.
    pub async fn search_fts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FtsResult>, AgentMemoryError> {
        let escaped_query = escape_fts5_query(query);
        if escaped_query.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            "SELECT memory_id, rank
             FROM memory_fts
             WHERE memory_fts MATCH ?
             ORDER BY rank
             LIMIT ?",
        )
        .bind(&escaped_query)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| FtsResult {
                id: row.get("memory_id"),
                rank: row.get("rank"),
            })
            .collect())
    }

    // ── Vector projection management ────────────────────────────────

    /// Store a vector embedding for a memory.
    pub async fn upsert_embedding(
        &self,
        memory_id: &str,
        embedding: &[f32],
    ) -> Result<(), AgentMemoryError> {
        // Delete existing first (vec0 doesn't support UPDATE).
        sqlx::query("DELETE FROM memory_vectors WHERE memory_id = ?")
            .bind(memory_id)
            .execute(&self.pool)
            .await?;
        sqlx::query("INSERT INTO memory_vectors(memory_id, embedding) VALUES (?, ?)")
            .bind(memory_id)
            .bind(embedding_to_bytes(embedding))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Delete a vector embedding.
    pub async fn delete_embedding(&self, memory_id: &str) -> Result<(), AgentMemoryError> {
        sqlx::query("DELETE FROM memory_vectors WHERE memory_id = ?")
            .bind(memory_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Vector similarity search (KNN), returning ranked results.
    pub async fn search_vector(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<VecResult>, AgentMemoryError> {
        let rows = sqlx::query(
            "SELECT memory_id, distance
             FROM memory_vectors
             WHERE embedding MATCH ? AND k = ?",
        )
        .bind(embedding_to_bytes(query_embedding))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| VecResult {
                id: row.get("memory_id"),
                distance: row.get("distance"),
            })
            .collect())
    }

    /// Check whether any embeddings exist.
    pub async fn has_embeddings(&self) -> Result<bool, AgentMemoryError> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM memory_vectors")
            .fetch_one(&self.pool)
            .await?;
        let count: i64 = row.get("cnt");
        Ok(count > 0)
    }

    /// Clear all search projection data (for reindex).
    pub async fn clear_projections(&self) -> Result<(), AgentMemoryError> {
        sqlx::query("DELETE FROM memory_vectors")
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM memory_fts")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Convert f32 embedding slice to bytes for sqlite-vec.
fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Escape a user query for safe use in FTS5 MATCH expressions.
///
/// FTS5 query syntax interprets `word:term` as a column filter (e.g.,
/// `agent:value` → search column "agent" for "value"). If the column doesn't
/// exist, SQLite returns "no such column". By wrapping each token in double
/// quotes we force FTS5 to treat them as literal search terms.
fn escape_fts5_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_fts5_plain_terms() {
        assert_eq!(escape_fts5_query("hello world"), "\"hello\" \"world\"");
    }

    #[test]
    fn escape_fts5_column_like_term() {
        // "agent" alone could be fine, but "agent:value" would fail without escaping
        assert_eq!(escape_fts5_query("agent"), "\"agent\"");
        assert_eq!(
            escape_fts5_query("style-agent:value"),
            "\"style-agent:value\""
        );
    }

    #[test]
    fn escape_fts5_strips_quotes() {
        assert_eq!(escape_fts5_query("hello \"world\""), "\"hello\" \"world\"");
    }

    #[test]
    fn escape_fts5_empty_query() {
        assert_eq!(escape_fts5_query(""), "");
        assert_eq!(escape_fts5_query("   "), "");
    }
}
