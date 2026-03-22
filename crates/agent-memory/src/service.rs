use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::config::Config;
use crate::embed::Embedder;
use crate::error::AgentMemoryError;
use crate::index;
use crate::markdown::MarkdownStore;
use crate::memory::{Memory, MemoryRepo, NewMemory};
use crate::store::SearchStore;

/// A search result with relevance scoring and decay information.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub id: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub score: f64,
    /// Decay factor (1.0 = fully fresh, 0.0 = fully decayed).
    /// Persistent or pinned memories always have 1.0.
    pub decay_factor: f64,
    pub pinned: bool,
    pub persistent: bool,
}

/// Decay configuration stored in the service.
#[derive(Debug, Clone)]
struct DecayConfig {
    half_life_days: f64,
    min_strength: f64,
    enabled: bool,
}

/// Compute exponential decay factor based on time since last access.
fn decay_factor(last_accessed: DateTime<Utc>, half_life_days: f64) -> f64 {
    let lambda = (2.0_f64).ln() / half_life_days;
    let days_elapsed = Utc::now()
        .signed_duration_since(last_accessed)
        .num_seconds() as f64
        / 86400.0;
    (-lambda * days_elapsed).exp()
}

/// High-level orchestrator for memory operations.
///
/// Coordinates the SQLite store, fastembed embedder, and markdown file I/O.
pub struct MemoryService {
    search: SearchStore,
    memory_repo: MemoryRepo,
    embedder: Option<Embedder>,
    markdown: MarkdownStore,
    decay: DecayConfig,
}

impl std::fmt::Debug for MemoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryService").finish_non_exhaustive()
    }
}

impl MemoryService {
    /// Create a new MemoryService from config.
    #[tracing::instrument(name = "agent_memory.service.new", skip_all)]
    pub async fn new(config: &Config) -> Result<Self, AgentMemoryError> {
        let search = SearchStore::open(&config.db_path).await?;
        let pool = search.pool().clone();

        let memory_repo = MemoryRepo::new(&pool);

        let embedder = match Embedder::new() {
            Ok(e) if e.is_available() => {
                tracing::info!("Embedder loaded — vector search enabled");
                Some(e)
            }
            Ok(_) => {
                tracing::warn!("Embedder unavailable — falling back to keyword-only search");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load embedder — falling back to keyword-only search");
                None
            }
        };

        let markdown = MarkdownStore::new(config.memories_dir.clone());

        let decay = DecayConfig {
            half_life_days: config.decay_half_life_days,
            min_strength: config.decay_min_strength,
            enabled: config.decay_enabled,
        };

        Ok(Self {
            search,
            memory_repo,
            embedder,
            markdown,
            decay,
        })
    }

    // ── Store ───────────────────────────────────────────────────────

    /// Store a new memory.
    #[tracing::instrument(name = "agent_memory.service.store", skip_all, fields(title = %new.title))]
    pub async fn store(&self, new: NewMemory) -> Result<Memory, AgentMemoryError> {
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
            last_accessed: None,
            access_count: 0,
            pinned: false,
            persistent: new.persistent,
        };

        // Write markdown file.
        let file_path = self.markdown.write(&memory)?;
        let memory = Memory {
            file_path: file_path.to_string_lossy().to_string(),
            ..memory
        };

        // Insert into store.
        self.memory_repo.insert(&memory).await?;

        // Update FTS index.
        let tags_str = memory.tags.join(", ");
        self.search
            .upsert_fts(&id, &memory.title, &memory.content, &tags_str)
            .await?;

        // Generate embedding if available.
        if let Some(embedder) = &self.embedder {
            let text = format!("{}\n\n{}", memory.title, memory.content);
            match embedder.embed_document(&text) {
                Ok(embedding) => {
                    self.search.upsert_embedding(&id, &embedding).await?;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to generate embedding");
                }
            }
        }

        tracing::info!(id = %memory.id, persistent = memory.persistent, "Memory stored");
        Ok(memory)
    }

    // ── List ────────────────────────────────────────────────────────

    /// List memories with optional filters.
    #[tracing::instrument(name = "agent_memory.service.list", skip_all)]
    pub async fn list(
        &self,
        project: Option<&str>,
        persistent: Option<bool>,
        limit: usize,
    ) -> Result<Vec<Memory>, AgentMemoryError> {
        self.memory_repo.list(project, persistent, limit).await
    }

    // ── Pin / Unpin ─────────────────────────────────────────────────

    /// Pin a memory (exempt from decay).
    #[tracing::instrument(name = "agent_memory.service.pin", skip_all, fields(id = %id_or_prefix))]
    pub async fn pin(&self, id_or_prefix: &str) -> Result<Memory, AgentMemoryError> {
        let memory = self.memory_repo.resolve_id(id_or_prefix).await?;
        self.memory_repo.set_pinned(&memory.id, true).await?;
        tracing::info!(id = %memory.id, "Memory pinned");
        self.memory_repo.find_by_id(&memory.id).await
    }

    /// Unpin a memory (subject to decay again).
    #[tracing::instrument(name = "agent_memory.service.unpin", skip_all, fields(id = %id_or_prefix))]
    pub async fn unpin(&self, id_or_prefix: &str) -> Result<Memory, AgentMemoryError> {
        let memory = self.memory_repo.resolve_id(id_or_prefix).await?;
        self.memory_repo.set_pinned(&memory.id, false).await?;
        tracing::info!(id = %memory.id, "Memory unpinned");
        self.memory_repo.find_by_id(&memory.id).await
    }

    // ── Search ──────────────────────────────────────────────────────

    /// Hybrid search: FTS + vector with Reciprocal Rank Fusion.
    #[tracing::instrument(name = "agent_memory.service.search", skip_all, fields(query = %query))]
    pub async fn search(
        &self,
        query: &str,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchResult>, AgentMemoryError> {
        let fetch_limit = limit * 3;

        // 1. FTS keyword search.
        let fts_results = self.search.search_fts(query, fetch_limit).await?;

        // 2. Vector search (if available).
        let vec_results = if let Some(embedder) = &self.embedder
            && self.search.has_embeddings().await?
        {
            match embedder.embed_query(query) {
                Ok(query_embedding) => {
                    self.search
                        .search_vector(&query_embedding, fetch_limit)
                        .await?
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Vector search failed, using FTS only");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // 3. Reciprocal Rank Fusion.
        let k = 60.0_f64;
        let mut scores: HashMap<String, f64> = HashMap::new();

        for (rank, result) in fts_results.iter().enumerate() {
            *scores.entry(result.id.clone()).or_default() += 1.0 / (k + rank as f64 + 1.0);
        }
        for (rank, result) in vec_results.iter().enumerate() {
            *scores.entry(result.id.clone()).or_default() += 1.0 / (k + rank as f64 + 1.0);
        }

        // Sort by RRF score descending.
        let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Fetch full memories and build results with decay scoring.
        let mut results = Vec::new();

        for (id, rrf_score) in &ranked {
            let result = match self.memory_repo.find_by_id(id).await {
                Ok(m) => {
                    if let Some(proj) = project
                        && m.project.as_deref() != Some(proj)
                    {
                        continue;
                    }

                    let exempt = m.pinned || m.persistent;
                    let df = if !self.decay.enabled || exempt {
                        1.0
                    } else {
                        let accessed = m.last_accessed.unwrap_or(m.created_at);
                        decay_factor(accessed, self.decay.half_life_days)
                    };

                    // Filter below min_strength.
                    if self.decay.enabled && !exempt && df < self.decay.min_strength {
                        continue;
                    }

                    let adjusted_score = rrf_score * df;
                    Some(SearchResult {
                        id: m.id.clone(),
                        title: m.title.clone(),
                        content: m.content.clone(),
                        tags: m.tags.clone(),
                        project: m.project.clone(),
                        score: adjusted_score,
                        decay_factor: df,
                        pinned: m.pinned,
                        persistent: m.persistent,
                    })
                }
                Err(_) => None,
            };

            if let Some(r) = result {
                results.push(r);
            }
        }

        // Re-sort by adjusted score descending.
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);

        // Record access for returned memories.
        let returned_ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
        if !returned_ids.is_empty()
            && let Err(e) = self.memory_repo.record_access(&returned_ids).await
        {
            tracing::warn!(error = %e, "Failed to record access for search results");
        }

        Ok(results)
    }

    // ── Get / Delete ────────────────────────────────────────────────

    /// Get a memory by ID or prefix. Records access (reinforcement).
    #[tracing::instrument(name = "agent_memory.service.get", skip_all, fields(id = %id_or_prefix))]
    pub async fn get(&self, id_or_prefix: &str) -> Result<Memory, AgentMemoryError> {
        let m = self.memory_repo.resolve_id(id_or_prefix).await?;
        // Record access (reinforcement).
        if let Err(e) = self
            .memory_repo
            .record_access(std::slice::from_ref(&m.id))
            .await
        {
            tracing::warn!(error = %e, "Failed to record access");
        }
        Ok(m)
    }

    /// Delete a memory by ID or prefix.
    #[tracing::instrument(name = "agent_memory.service.delete", skip_all, fields(id = %id_or_prefix))]
    pub async fn delete(&self, id_or_prefix: &str) -> Result<(), AgentMemoryError> {
        let memory = self.memory_repo.resolve_id(id_or_prefix).await?;
        self.markdown.delete(&memory.file_path)?;
        self.memory_repo.delete(&memory.id).await?;
        self.search.delete_fts(&memory.id).await?;
        self.search.delete_embedding(&memory.id).await?;
        tracing::info!(id = %memory.id, "Memory deleted");
        Ok(())
    }

    // ── Reindex / Stats ─────────────────────────────────────────────

    /// Rebuild the store from markdown files.
    #[tracing::instrument(name = "agent_memory.service.reindex", skip_all)]
    pub async fn reindex(&self) -> Result<usize, AgentMemoryError> {
        index::reindex(
            &self.memory_repo,
            &self.search,
            &self.markdown,
            self.embedder.as_ref(),
        )
        .await
    }

    /// Get database stats.
    pub async fn stats(&self) -> Result<Stats, AgentMemoryError> {
        Ok(Stats {
            memory_count: self.memory_repo.count().await?,
            has_embeddings: self.search.has_embeddings().await?,
            embedder_available: self.embedder.is_some(),
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
