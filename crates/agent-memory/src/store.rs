use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use sqlite_vec::sqlite3_vec_init;
use zerocopy::AsBytes;

use crate::error::AgentMemoryError;
use crate::memory::Memory;

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

/// SQLite-backed store with FTS5 full-text search and sqlite-vec vector search.
pub struct Store {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Open or create the database, loading sqlite-vec extension.
    pub fn open(db_path: &Path) -> Result<Self, AgentMemoryError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut i8,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite3_vec_init as *const ()
            )));
        }

        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, AgentMemoryError> {
        self.conn
            .lock()
            .map_err(|e| AgentMemoryError::Other(e.to_string()))
    }

    /// Run migrations to create/update the schema.
    pub fn migrate(&self) -> Result<(), AgentMemoryError> {
        let conn = self.lock_conn()?;
        let sql = include_str!("../migrations/001_initial.sql");
        conn.execute_batch(sql)?;
        Ok(())
    }

    /// Insert or replace a memory.
    pub fn insert_memory(&self, memory: &Memory) -> Result<(), AgentMemoryError> {
        let conn = self.lock_conn()?;
        let tags_json = serde_json::to_string(&memory.tags)?;
        conn.execute(
            "INSERT OR REPLACE INTO memories
                (id, title, content, tags, project, source_task, source_type,
                 file_path, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                memory.id,
                memory.title,
                memory.content,
                tags_json,
                memory.project,
                memory.source_task,
                memory.source_type,
                memory.file_path,
                memory.created_at.to_rfc3339(),
                memory.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Store a vector embedding for a memory.
    pub fn upsert_embedding(
        &self,
        memory_id: &str,
        embedding: &[f32],
    ) -> Result<(), AgentMemoryError> {
        let conn = self.lock_conn()?;
        // Delete existing first (vec0 doesn't support UPDATE)
        conn.execute(
            "DELETE FROM memory_vectors WHERE memory_id = ?1",
            [memory_id],
        )?;
        conn.execute(
            "INSERT INTO memory_vectors(memory_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![memory_id, embedding.as_bytes()],
        )?;
        Ok(())
    }

    /// Full-text keyword search, returning ranked results.
    pub fn search_fts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FtsResult>, AgentMemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, fts.rank
             FROM memories_fts fts
             JOIN memories m ON m.rowid = fts.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY fts.rank
             LIMIT ?2",
        )?;
        let results = stmt
            .query_map(rusqlite::params![query, limit as i64], |row| {
                Ok(FtsResult {
                    id: row.get(0)?,
                    rank: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }

    /// Vector similarity search (KNN), returning ranked results.
    pub fn search_vector(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<VecResult>, AgentMemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT memory_id, distance
             FROM memory_vectors
             WHERE embedding MATCH ?1 AND k = ?2",
        )?;
        let results = stmt
            .query_map(
                rusqlite::params![query_embedding.as_bytes(), limit as i64],
                |row| {
                    Ok(VecResult {
                        id: row.get(0)?,
                        distance: row.get(1)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }

    /// Get a memory by exact ID.
    pub fn find_by_id(&self, id: &str) -> Result<Memory, AgentMemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, content, tags, project, source_task, source_type,
                    file_path, created_at, updated_at
             FROM memories WHERE id = ?1",
        )?;
        stmt.query_row([id], |row| Ok(row_to_memory(row)))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    AgentMemoryError::NotFound(format!("memory '{id}'"))
                }
                other => AgentMemoryError::Sqlite(other),
            })
    }

    /// Get memories whose ID starts with the given prefix.
    pub fn find_by_prefix(&self, prefix: &str) -> Result<Vec<Memory>, AgentMemoryError> {
        let conn = self.lock_conn()?;
        let pattern = format!("{prefix}%");
        let mut stmt = conn.prepare(
            "SELECT id, title, content, tags, project, source_task, source_type,
                    file_path, created_at, updated_at
             FROM memories WHERE id LIKE ?1",
        )?;
        let results = stmt
            .query_map([&pattern], |row| Ok(row_to_memory(row)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }

    /// Resolve an exact ID or unique prefix to a single memory.
    pub fn resolve_id(&self, id_or_prefix: &str) -> Result<Memory, AgentMemoryError> {
        match self.find_by_id(id_or_prefix) {
            Ok(m) => return Ok(m),
            Err(AgentMemoryError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        let matches = self.find_by_prefix(id_or_prefix)?;
        match matches.len() {
            0 => Err(AgentMemoryError::NotFound(format!(
                "memory '{id_or_prefix}'"
            ))),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => Err(AgentMemoryError::AmbiguousPrefix(
                id_or_prefix.to_string(),
                n,
            )),
        }
    }

    /// Delete a memory by exact ID.
    pub fn delete(&self, id: &str) -> Result<(), AgentMemoryError> {
        let conn = self.lock_conn()?;
        conn.execute("DELETE FROM memory_vectors WHERE memory_id = ?1", [id])?;
        conn.execute("DELETE FROM memories WHERE id = ?1", [id])?;
        Ok(())
    }

    /// List memories with optional project/tag filters, ordered by created_at DESC.
    pub fn list(
        &self,
        project: Option<&str>,
        tags: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<Memory>, AgentMemoryError> {
        let conn = self.lock_conn()?;

        let mut sql = String::from(
            "SELECT id, title, content, tags, project, source_task, source_type,
                    file_path, created_at, updated_at
             FROM memories WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(proj) = project {
            params.push(Box::new(proj.to_string()));
            sql.push_str(&format!(" AND project = ?{}", params.len()));
        }

        if let Some(tag_list) = tags {
            for tag in tag_list {
                params.push(Box::new(format!("%\"{tag}\"%")));
                sql.push_str(&format!(" AND tags LIKE ?{}", params.len()));
            }
        }

        sql.push_str(" ORDER BY created_at DESC");
        params.push(Box::new(limit as i64));
        sql.push_str(&format!(" LIMIT ?{}", params.len()));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| &**p).collect();
        let mut stmt = conn.prepare(&sql)?;
        let results = stmt
            .query_map(param_refs.as_slice(), |row| Ok(row_to_memory(row)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }

    /// Get all memory IDs in the store.
    pub fn all_ids(&self) -> Result<Vec<String>, AgentMemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare("SELECT id FROM memories")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    /// Count total memories.
    pub fn count(&self) -> Result<u64, AgentMemoryError> {
        let conn = self.lock_conn()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Check whether any embeddings exist.
    pub fn has_embeddings(&self) -> Result<bool, AgentMemoryError> {
        let conn = self.lock_conn()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_vectors", [], |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Delete all data (for full reindex).
    pub fn clear_all(&self) -> Result<(), AgentMemoryError> {
        let conn = self.lock_conn()?;
        conn.execute_batch(
            "DELETE FROM memory_vectors;
             DELETE FROM memories;",
        )?;
        Ok(())
    }

    /// Fetch memories by a list of IDs.
    pub fn get_by_ids(&self, ids: &[String]) -> Result<Vec<Memory>, AgentMemoryError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock_conn()?;
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT id, title, content, tags, project, source_task, source_type,
                    file_path, created_at, updated_at
             FROM memories WHERE id IN ({})",
            placeholders.join(", ")
        );
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let results = stmt
            .query_map(param_refs.as_slice(), |row| Ok(row_to_memory(row)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }
}

fn row_to_memory(row: &rusqlite::Row) -> Memory {
    let tags_json: String = row.get_unwrap(3);
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let created_str: String = row.get_unwrap(8);
    let updated_str: String = row.get_unwrap(9);

    Memory {
        id: row.get_unwrap(0),
        title: row.get_unwrap(1),
        content: row.get_unwrap(2),
        tags,
        project: row.get_unwrap(4),
        source_task: row.get_unwrap(5),
        source_type: row.get_unwrap(6),
        file_path: row.get_unwrap(7),
        created_at: chrono::DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_default(),
        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_default(),
    }
}
