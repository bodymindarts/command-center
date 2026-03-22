use std::collections::HashSet;

use crate::embed::Embedder;
use crate::error::AgentMemoryError;
use crate::markdown::MarkdownStore;
use crate::memory::MemoryRepo;
use crate::store::SearchStore;

/// Sync markdown files into the SQLite store (memories only).
///
/// Returns the number of memories indexed.
pub async fn reindex(
    memory_repo: &MemoryRepo,
    search: &SearchStore,
    markdown: &MarkdownStore,
    embedder: Option<&Embedder>,
) -> Result<usize, AgentMemoryError> {
    tracing::info!("Starting reindex from markdown files");

    // Clear existing memory data and search projections.
    memory_repo.clear_all().await?;
    search.clear_projections().await?;

    let paths = markdown.walk_all()?;
    let mut count = 0;

    for path in &paths {
        match markdown.read(path) {
            Ok(memory) => {
                memory_repo.insert(&memory).await?;

                // Update FTS index.
                let tags_str = memory.tags.join(", ");
                search
                    .upsert_fts(&memory.id, &memory.title, &memory.content, &tags_str)
                    .await?;

                // Generate embedding if embedder is available.
                if let Some(embedder) = embedder {
                    let text = format!("{}\n\n{}", memory.title, memory.content);
                    match embedder.embed_document(&text) {
                        Ok(embedding) => {
                            search.upsert_embedding(&memory.id, &embedding).await?;
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "Failed to generate embedding, skipping"
                            );
                        }
                    }
                }

                count += 1;
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to parse markdown file, skipping"
                );
            }
        }
    }

    // Clean up any DB entries whose markdown files no longer exist.
    let all_ids = memory_repo.all_ids().await?;
    let file_ids: HashSet<String> = paths
        .iter()
        .filter_map(|p| markdown.read(p).ok())
        .map(|m| m.id)
        .collect();

    for id in &all_ids {
        if !file_ids.contains(id) {
            memory_repo.delete(id).await?;
            search.delete_fts(id).await?;
            search.delete_embedding(id).await?;
        }
    }

    tracing::info!(count, "Reindex complete");
    Ok(count)
}
