use std::collections::HashSet;

use crate::embed::Embedder;
use crate::error::AgentMemoryError;
use crate::markdown::MarkdownStore;
use crate::store::Store;

/// Sync markdown files into the SQLite store.
///
/// Returns the number of memories indexed.
pub fn reindex(
    store: &Store,
    markdown: &MarkdownStore,
    embedder: Option<&Embedder>,
) -> Result<usize, AgentMemoryError> {
    tracing::info!("Starting reindex from markdown files");

    // Clear existing data
    store.clear_all()?;
    store.migrate()?;

    let paths = markdown.walk_all()?;
    let mut count = 0;

    for path in &paths {
        match markdown.read(path) {
            Ok(memory) => {
                store.insert_memory(&memory)?;

                // Generate embedding if embedder is available
                if let Some(embedder) = embedder {
                    let text = format!("{}\n\n{}", memory.title, memory.content);
                    match embedder.embed_document(&text) {
                        Ok(embedding) => {
                            store.upsert_embedding(&memory.id, &embedding)?;
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

    // Clean up any DB entries whose markdown files no longer exist
    let all_ids = store.all_ids()?;
    let file_ids: HashSet<String> = paths
        .iter()
        .filter_map(|p| markdown.read(p).ok())
        .map(|m| m.id)
        .collect();

    for id in &all_ids {
        if !file_ids.contains(id) {
            store.delete(id)?;
        }
    }

    tracing::info!(count, "Reindex complete");
    Ok(count)
}
