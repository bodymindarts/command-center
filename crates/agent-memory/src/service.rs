use std::collections::HashMap;

use chrono::Utc;
use uuid::Uuid;

use crate::config::Config;
use crate::embed::Embedder;
use crate::error::AgentMemoryError;
use crate::index;
use crate::markdown::MarkdownStore;
use crate::memory::{Memory, NewMemory, SearchResult};
use crate::store::Store;

/// High-level orchestrator for memory operations.
///
/// Coordinates the SQLite store, fastembed embedder, and markdown file I/O.
pub struct MemoryService {
    store: Store,
    embedder: Embedder,
    markdown: MarkdownStore,
}

impl std::fmt::Debug for MemoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryService").finish_non_exhaustive()
    }
}

impl MemoryService {
    /// Create a new MemoryService from config.
    #[tracing::instrument(name = "agent_memory.service.new", skip_all)]
    pub fn new(config: &Config) -> Result<Self, AgentMemoryError> {
        let store = Store::open(&config.db_path)?;
        store.migrate()?;

        let embedder = Embedder::new()?;
        if embedder.is_available() {
            tracing::info!("Embedder loaded — vector search enabled");
        } else {
            tracing::warn!("Embedder unavailable — falling back to keyword-only search");
        }

        let markdown = MarkdownStore::new(config.memories_dir.clone());

        Ok(Self {
            store,
            embedder,
            markdown,
        })
    }

    /// Store a new memory.
    #[tracing::instrument(name = "agent_memory.service.store", skip_all, fields(title = %new.title))]
    pub fn store(&self, new: NewMemory) -> Result<Memory, AgentMemoryError> {
        let now = Utc::now();
        let id = Uuid::now_v7().to_string();

        let memory = Memory {
            id: id.clone(),
            title: new.title,
            content: new.content,
            tags: new.tags,
            project: new.project,
            source_task: new.source_task,
            source_type: new.source_type,
            file_path: String::new(),
            created_at: now,
            updated_at: now,
        };

        // Write markdown file
        let file_path = self.markdown.write(&memory)?;
        let memory = Memory {
            file_path: file_path.to_string_lossy().to_string(),
            ..memory
        };

        // Insert into store
        self.store.insert_memory(&memory)?;

        // Generate embedding if available
        if self.embedder.is_available() {
            let text = format!("{}\n\n{}", memory.title, memory.content);
            match self.embedder.embed_document(&text) {
                Ok(embedding) => {
                    self.store.upsert_embedding(&id, &embedding)?;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to generate embedding");
                }
            }
        }

        tracing::info!(id = %memory.id, "Memory stored");
        Ok(memory)
    }

    /// Hybrid search: FTS + vector with Reciprocal Rank Fusion.
    #[tracing::instrument(name = "agent_memory.service.search", skip_all, fields(query = %query))]
    pub fn search(
        &self,
        query: &str,
        project: Option<&str>,
        tags: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<SearchResult>, AgentMemoryError> {
        let fetch_limit = limit * 3;

        // 1. FTS keyword search
        let fts_results = self.store.search_fts(query, fetch_limit)?;

        // 2. Vector search (if available)
        let vec_results = if self.embedder.is_available() && self.store.has_embeddings()? {
            match self.embedder.embed_query(query) {
                Ok(query_embedding) => self.store.search_vector(&query_embedding, fetch_limit)?,
                Err(e) => {
                    tracing::warn!(error = %e, "Vector search failed, using FTS only");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // 3. Reciprocal Rank Fusion
        let k = 60.0_f64;
        let mut scores: HashMap<String, f64> = HashMap::new();

        for (rank, result) in fts_results.iter().enumerate() {
            *scores.entry(result.id.clone()).or_default() += 1.0 / (k + rank as f64 + 1.0);
        }
        for (rank, result) in vec_results.iter().enumerate() {
            *scores.entry(result.id.clone()).or_default() += 1.0 / (k + rank as f64 + 1.0);
        }

        // Sort by RRF score descending
        let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Fetch full memories for top results
        let top_ids: Vec<String> = ranked.iter().map(|(id, _)| id.clone()).collect();
        let memories = self.store.get_by_ids(&top_ids)?;

        // Build a map for O(1) lookup
        let memory_map: HashMap<String, Memory> =
            memories.into_iter().map(|m| (m.id.clone(), m)).collect();

        // Filter by project and tags, then take top N
        let mut results = Vec::new();
        for (id, score) in &ranked {
            if let Some(memory) = memory_map.get(id) {
                // Project filter
                if let Some(proj) = project
                    && memory.project.as_deref() != Some(proj)
                {
                    continue;
                }
                // Tags filter
                if let Some(tag_filter) = tags
                    && !tag_filter.iter().all(|t| memory.tags.contains(t))
                {
                    continue;
                }
                results.push(SearchResult {
                    memory: memory.clone(),
                    score: *score,
                });
                if results.len() >= limit {
                    break;
                }
            }
        }

        Ok(results)
    }

    /// List memories with optional filters.
    #[tracing::instrument(name = "agent_memory.service.list", skip_all)]
    pub fn list(
        &self,
        project: Option<&str>,
        tags: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<Memory>, AgentMemoryError> {
        self.store.list(project, tags, limit)
    }

    /// Get a memory by ID or prefix.
    #[tracing::instrument(name = "agent_memory.service.get", skip_all, fields(id = %id_or_prefix))]
    pub fn get(&self, id_or_prefix: &str) -> Result<Memory, AgentMemoryError> {
        self.store.resolve_id(id_or_prefix)
    }

    /// Delete a memory by ID or prefix.
    #[tracing::instrument(name = "agent_memory.service.delete", skip_all, fields(id = %id_or_prefix))]
    pub fn delete(&self, id_or_prefix: &str) -> Result<(), AgentMemoryError> {
        let memory = self.store.resolve_id(id_or_prefix)?;
        self.markdown.delete(&memory.file_path)?;
        self.store.delete(&memory.id)?;
        tracing::info!(id = %memory.id, "Memory deleted");
        Ok(())
    }

    /// Rebuild the store from markdown files.
    #[tracing::instrument(name = "agent_memory.service.reindex", skip_all)]
    pub fn reindex(&self) -> Result<usize, AgentMemoryError> {
        index::reindex(&self.store, &self.markdown, &self.embedder)
    }

    /// Get database stats.
    pub fn stats(&self) -> Result<Stats, AgentMemoryError> {
        Ok(Stats {
            memory_count: self.store.count()?,
            has_embeddings: self.store.has_embeddings()?,
            embedder_available: self.embedder.is_available(),
        })
    }
}

/// Database statistics.
#[derive(Debug)]
pub struct Stats {
    pub memory_count: u64,
    pub has_embeddings: bool,
    pub embedder_available: bool,
}
