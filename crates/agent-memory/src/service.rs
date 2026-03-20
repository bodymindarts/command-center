use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::config::Config;
use crate::embed::Embedder;
use crate::error::AgentMemoryError;
use crate::index;
use crate::markdown::MarkdownStore;
use crate::natural_memory::{NaturalMemory, NaturalMemoryRepo, NewNaturalMemory};
use crate::primitives::ResearchReportId;
use crate::research_report::{NewResearchReport, ReportUpdate, ResearchReport, ResearchReportRepo};
use crate::store::SearchStore;

/// A unified search result that indicates which memory type was matched.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub id: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub memory_type: MemoryType,
    pub score: f64,
    /// Decay factor (1.0 = fully fresh, 0.0 = fully decayed). Reports always 1.0.
    pub decay_factor: f64,
    /// Whether this memory is pinned (always false for reports).
    pub pinned: bool,
}

/// The type of memory in a search result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Natural,
    Report,
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Natural => write!(f, "natural"),
            Self::Report => write!(f, "report"),
        }
    }
}

/// Either type of memory, returned by `get`.
#[derive(Debug, Clone)]
pub enum MemoryItem {
    Natural(NaturalMemory),
    Report(ResearchReport),
}

impl MemoryItem {
    pub fn title(&self) -> &str {
        match self {
            Self::Natural(m) => &m.title,
            Self::Report(r) => &r.title,
        }
    }

    pub fn content(&self) -> &str {
        match self {
            Self::Natural(m) => &m.content,
            Self::Report(r) => &r.content,
        }
    }

    pub fn tags(&self) -> &[String] {
        match self {
            Self::Natural(m) => &m.tags,
            Self::Report(r) => &r.tags,
        }
    }

    pub fn project(&self) -> Option<&str> {
        match self {
            Self::Natural(m) => m.project.as_deref(),
            Self::Report(r) => r.project.as_deref(),
        }
    }

    pub fn memory_type(&self) -> MemoryType {
        match self {
            Self::Natural(_) => MemoryType::Natural,
            Self::Report(_) => MemoryType::Report,
        }
    }
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
/// Exposes operations for both natural memories and research reports,
/// plus unified search across all types.
pub struct MemoryService {
    search: SearchStore,
    natural_repo: NaturalMemoryRepo,
    report_repo: ResearchReportRepo,
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

        let natural_repo = NaturalMemoryRepo::new(&pool);
        let report_repo = ResearchReportRepo::new(&pool);

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
            natural_repo,
            report_repo,
            embedder,
            markdown,
            decay,
        })
    }

    // ── Natural memories ────────────────────────────────────────────

    /// Store a new natural memory.
    #[tracing::instrument(name = "agent_memory.service.store_natural", skip_all, fields(title = %new.title))]
    pub async fn store_natural(
        &self,
        new: NewNaturalMemory,
    ) -> Result<NaturalMemory, AgentMemoryError> {
        let now = Utc::now();
        let id = Uuid::now_v7().to_string();

        let memory = NaturalMemory {
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
        };

        // Write markdown file.
        let file_path = self.markdown.write(&memory)?;
        let memory = NaturalMemory {
            file_path: file_path.to_string_lossy().to_string(),
            ..memory
        };

        // Insert into store.
        self.natural_repo.insert(&memory).await?;

        // Update FTS index.
        let tags_str = memory.tags.join(", ");
        self.search
            .upsert_fts(&id, "natural", &memory.title, &memory.content, &tags_str)
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

        tracing::info!(id = %memory.id, "Natural memory stored");
        Ok(memory)
    }

    /// List natural memories.
    #[tracing::instrument(name = "agent_memory.service.list_natural", skip_all)]
    pub async fn list_natural(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<NaturalMemory>, AgentMemoryError> {
        self.natural_repo.list(project, limit).await
    }

    /// Pin a natural memory (exempt from decay).
    #[tracing::instrument(name = "agent_memory.service.pin", skip_all, fields(id = %id_or_prefix))]
    pub async fn pin(&self, id_or_prefix: &str) -> Result<NaturalMemory, AgentMemoryError> {
        let memory = self.natural_repo.resolve_id(id_or_prefix).await?;
        self.natural_repo.set_pinned(&memory.id, true).await?;
        tracing::info!(id = %memory.id, "Memory pinned");
        self.natural_repo.find_by_id(&memory.id).await
    }

    /// Unpin a natural memory (subject to decay again).
    #[tracing::instrument(name = "agent_memory.service.unpin", skip_all, fields(id = %id_or_prefix))]
    pub async fn unpin(&self, id_or_prefix: &str) -> Result<NaturalMemory, AgentMemoryError> {
        let memory = self.natural_repo.resolve_id(id_or_prefix).await?;
        self.natural_repo.set_pinned(&memory.id, false).await?;
        tracing::info!(id = %memory.id, "Memory unpinned");
        self.natural_repo.find_by_id(&memory.id).await
    }

    // ── Research reports ────────────────────────────────────────────

    /// Store a new research report.
    #[tracing::instrument(name = "agent_memory.service.store_report", skip_all, fields(title = %new.title))]
    pub async fn store_report(
        &self,
        new: NewResearchReport,
    ) -> Result<ResearchReport, AgentMemoryError> {
        let id = new.id;
        let title = new.title.clone();
        let content = new.content.clone();
        let tags = new.tags.clone();

        let report = self
            .report_repo
            .create(new)
            .await
            .map_err(|e| AgentMemoryError::Other(e.to_string()))?;

        // Update FTS index.
        let tags_str = tags.join(", ");
        self.search
            .upsert_fts(&id.to_string(), "report", &title, &content, &tags_str)
            .await?;

        // Generate embedding if available.
        if let Some(embedder) = &self.embedder {
            let text = format!("{}\n\n{}", title, content);
            match embedder.embed_document(&text) {
                Ok(embedding) => {
                    self.search
                        .upsert_embedding(&id.to_string(), &embedding)
                        .await?;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to generate embedding");
                }
            }
        }

        tracing::info!(id = %id, "Research report stored");
        Ok(report)
    }

    /// Update a research report.
    #[tracing::instrument(name = "agent_memory.service.update_report", skip_all)]
    pub async fn update_report(
        &self,
        id: &str,
        update: ReportUpdate,
    ) -> Result<ResearchReport, AgentMemoryError> {
        let report_id: ResearchReportId = id
            .parse()
            .map_err(|_| AgentMemoryError::NotFound(format!("report '{id}'")))?;
        let mut report = self
            .report_repo
            .find_by_id(report_id)
            .await
            .map_err(|e| AgentMemoryError::Other(e.to_string()))?;
        let _ = report.update(update);
        self.report_repo
            .update(&mut report)
            .await
            .map_err(|e| AgentMemoryError::Other(e.to_string()))?;

        // Update FTS index.
        let tags_str = report.tags.join(", ");
        self.search
            .upsert_fts(
                &report.id.to_string(),
                "report",
                &report.title,
                &report.content,
                &tags_str,
            )
            .await?;

        // Update embedding if available.
        if let Some(embedder) = &self.embedder {
            let text = format!("{}\n\n{}", report.title, report.content);
            match embedder.embed_document(&text) {
                Ok(embedding) => {
                    self.search
                        .upsert_embedding(&report.id.to_string(), &embedding)
                        .await?;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to generate embedding for update");
                }
            }
        }

        Ok(report)
    }

    /// List research reports.
    #[tracing::instrument(name = "agent_memory.service.list_reports", skip_all)]
    pub async fn list_reports(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ResearchReport>, AgentMemoryError> {
        if let Some(proj) = project {
            let ret = self
                .report_repo
                .list_for_project_by_created_at(
                    Some(proj.to_string()),
                    es_entity::PaginatedQueryArgs {
                        first: limit,
                        after: None,
                    },
                    es_entity::ListDirection::Descending,
                )
                .await
                .map_err(|e| AgentMemoryError::Other(e.to_string()))?;
            Ok(ret.entities)
        } else {
            let ret = self
                .report_repo
                .list_by_created_at(
                    es_entity::PaginatedQueryArgs {
                        first: limit,
                        after: None,
                    },
                    es_entity::ListDirection::Descending,
                )
                .await
                .map_err(|e| AgentMemoryError::Other(e.to_string()))?;
            Ok(ret.entities)
        }
    }

    // ── Unified search ──────────────────────────────────────────────

    /// Hybrid search: FTS + vector with Reciprocal Rank Fusion across both types.
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
        let mut type_map: HashMap<String, String> = HashMap::new();

        for (rank, result) in fts_results.iter().enumerate() {
            *scores.entry(result.id.clone()).or_default() += 1.0 / (k + rank as f64 + 1.0);
            type_map.insert(result.id.clone(), result.memory_type.clone());
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
            let mem_type = type_map.get(id).map(|s| s.as_str()).unwrap_or("natural");
            let result = match mem_type {
                "report" => {
                    // Reports never decay.
                    let report_id: Result<ResearchReportId, _> = id.parse();
                    if let Ok(rid) = report_id {
                        match self.report_repo.find_by_id(rid).await {
                            Ok(r) => {
                                if let Some(proj) = project
                                    && r.project.as_deref() != Some(proj)
                                {
                                    continue;
                                }
                                Some(SearchResult {
                                    id: r.id.to_string(),
                                    title: r.title.clone(),
                                    content: r.content.clone(),
                                    tags: r.tags.clone(),
                                    project: r.project.clone(),
                                    memory_type: MemoryType::Report,
                                    score: *rrf_score,
                                    decay_factor: 1.0,
                                    pinned: false,
                                })
                            }
                            Err(_) => None,
                        }
                    } else {
                        None
                    }
                }
                _ => match self.natural_repo.find_by_id(id).await {
                    Ok(m) => {
                        if let Some(proj) = project
                            && m.project.as_deref() != Some(proj)
                        {
                            continue;
                        }

                        let df = if !self.decay.enabled || m.pinned {
                            1.0
                        } else {
                            let accessed = m.last_accessed.unwrap_or(m.created_at);
                            decay_factor(accessed, self.decay.half_life_days)
                        };

                        // Filter below min_strength.
                        if self.decay.enabled && !m.pinned && df < self.decay.min_strength {
                            continue;
                        }

                        let adjusted_score = rrf_score * df;
                        Some(SearchResult {
                            id: m.id.clone(),
                            title: m.title.clone(),
                            content: m.content.clone(),
                            tags: m.tags.clone(),
                            project: m.project.clone(),
                            memory_type: MemoryType::Natural,
                            score: adjusted_score,
                            decay_factor: df,
                            pinned: m.pinned,
                        })
                    }
                    Err(_) => None,
                },
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

        // Collect IDs of natural memories actually returned, then record access.
        let returned_natural_ids: Vec<String> = results
            .iter()
            .filter(|r| r.memory_type == MemoryType::Natural)
            .map(|r| r.id.clone())
            .collect();
        if !returned_natural_ids.is_empty()
            && let Err(e) = self.natural_repo.record_access(&returned_natural_ids).await
        {
            tracing::warn!(error = %e, "Failed to record access for search results");
        }

        Ok(results)
    }

    // ── Shared operations ───────────────────────────────────────────

    /// Get a memory by ID or prefix (returns either type).
    /// Records access for NaturalMemory (reinforcement).
    #[tracing::instrument(name = "agent_memory.service.get", skip_all, fields(id = %id_or_prefix))]
    pub async fn get(&self, id_or_prefix: &str) -> Result<MemoryItem, AgentMemoryError> {
        // Try natural memory first.
        match self.natural_repo.resolve_id(id_or_prefix).await {
            Ok(m) => {
                // Record access (reinforcement).
                if let Err(e) = self
                    .natural_repo
                    .record_access(std::slice::from_ref(&m.id))
                    .await
                {
                    tracing::warn!(error = %e, "Failed to record access");
                }
                return Ok(MemoryItem::Natural(m));
            }
            Err(AgentMemoryError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }

        // Try research report by parsing as UUID.
        if let Ok(rid) = id_or_prefix.parse::<ResearchReportId>()
            && let Ok(r) = self.report_repo.find_by_id(rid).await
        {
            return Ok(MemoryItem::Report(r));
        }

        Err(AgentMemoryError::NotFound(format!(
            "memory '{id_or_prefix}'"
        )))
    }

    /// Delete a memory by ID or prefix (either type).
    #[tracing::instrument(name = "agent_memory.service.delete", skip_all, fields(id = %id_or_prefix))]
    pub async fn delete(&self, id_or_prefix: &str) -> Result<(), AgentMemoryError> {
        // Try natural memory first.
        match self.natural_repo.resolve_id(id_or_prefix).await {
            Ok(memory) => {
                self.markdown.delete(&memory.file_path)?;
                self.natural_repo.delete(&memory.id).await?;
                self.search.delete_fts(&memory.id).await?;
                self.search.delete_embedding(&memory.id).await?;
                tracing::info!(id = %memory.id, "Natural memory deleted");
                return Ok(());
            }
            Err(AgentMemoryError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }

        // Try research report.
        if let Ok(rid) = id_or_prefix.parse::<ResearchReportId>()
            && let Ok(mut report) = self.report_repo.find_by_id(rid).await
        {
            let _ = report.supersede(ResearchReportId::new());
            self.report_repo
                .update(&mut report)
                .await
                .map_err(|e| AgentMemoryError::Other(e.to_string()))?;
            self.search.delete_fts(&rid.to_string()).await?;
            self.search.delete_embedding(&rid.to_string()).await?;
            tracing::info!(id = %rid, "Research report superseded");
            return Ok(());
        }

        Err(AgentMemoryError::NotFound(format!(
            "memory '{id_or_prefix}'"
        )))
    }

    /// Rebuild the store from markdown files.
    #[tracing::instrument(name = "agent_memory.service.reindex", skip_all)]
    pub async fn reindex(&self) -> Result<usize, AgentMemoryError> {
        index::reindex(
            &self.natural_repo,
            &self.search,
            &self.markdown,
            self.embedder.as_ref(),
        )
        .await
    }

    /// Get database stats.
    pub async fn stats(&self) -> Result<Stats, AgentMemoryError> {
        Ok(Stats {
            natural_count: self.natural_repo.count().await?,
            has_embeddings: self.search.has_embeddings().await?,
            embedder_available: self.embedder.is_some(),
        })
    }
}

/// Database statistics.
#[derive(Debug)]
pub struct Stats {
    pub natural_count: u64,
    pub has_embeddings: bool,
    pub embedder_available: bool,
}
