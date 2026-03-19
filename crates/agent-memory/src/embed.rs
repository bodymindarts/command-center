use crate::error::AgentMemoryError;

/// Max chars to send to the embedding model.
#[cfg(feature = "embed")]
const MAX_CHARS: usize = 6000;

/// Embedding dimensions for all-MiniLM-L6-v2.
pub const DIMENSIONS: usize = 384;

/// In-process text embedder using fastembed (all-MiniLM-L6-v2).
///
/// Only available when the `embed` feature is enabled.
/// Without the feature, all methods return empty results.
pub struct Embedder {
    #[cfg(feature = "embed")]
    model: fastembed::TextEmbedding,
}

impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Embedder").finish_non_exhaustive()
    }
}

impl Embedder {
    /// Create a new embedder, loading the all-MiniLM-L6-v2 model.
    #[cfg(feature = "embed")]
    pub fn new() -> Result<Self, AgentMemoryError> {
        let model = fastembed::TextEmbedding::try_new(
            fastembed::InitOptions::new(fastembed::EmbeddingModel::AllMiniLML6V2)
                .with_show_download_progress(true),
        )
        .map_err(|e| AgentMemoryError::Other(format!("failed to load embedding model: {e}")))?;
        Ok(Self { model })
    }

    /// Create a no-op embedder (embed feature disabled).
    #[cfg(not(feature = "embed"))]
    pub fn new() -> Result<Self, AgentMemoryError> {
        Ok(Self {})
    }

    /// Whether this embedder can actually produce embeddings.
    pub fn is_available(&self) -> bool {
        cfg!(feature = "embed")
    }

    /// Embed a document for storage.
    pub fn embed_document(&self, text: &str) -> Result<Vec<f32>, AgentMemoryError> {
        self.embed(text)
    }

    /// Embed a query for search.
    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>, AgentMemoryError> {
        self.embed(text)
    }

    /// Number of dimensions in the embedding vectors.
    pub fn dimensions(&self) -> usize {
        DIMENSIONS
    }

    #[cfg(feature = "embed")]
    fn embed(&self, text: &str) -> Result<Vec<f32>, AgentMemoryError> {
        let truncated = truncate(text, MAX_CHARS);
        let results = self
            .model
            .embed(vec![truncated], None)
            .map_err(|e| AgentMemoryError::Other(format!("embedding failed: {e}")))?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| AgentMemoryError::Other("embedding returned no results".to_string()))
    }

    #[cfg(not(feature = "embed"))]
    fn embed(&self, _text: &str) -> Result<Vec<f32>, AgentMemoryError> {
        Ok(Vec::new())
    }
}

#[cfg(feature = "embed")]
fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        text.to_string()
    } else {
        text[..limit].to_string()
    }
}
