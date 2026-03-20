use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::AgentMemoryError;
use crate::natural_memory::NaturalMemory;

/// YAML frontmatter for markdown memory files.
#[derive(Debug, Serialize, Deserialize)]
struct Frontmatter {
    id: String,
    title: String,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_task: Option<String>,
    source_type: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    last_accessed: Option<DateTime<Utc>>,
    #[serde(default)]
    access_count: i64,
    #[serde(default)]
    pinned: bool,
}

/// Handles reading and writing markdown memory files.
#[derive(Debug, Clone)]
pub struct MarkdownStore {
    base_dir: PathBuf,
}

impl MarkdownStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Build the file path for a memory: base_dir/YYYY-MM/<id-short>-<slug>.md
    pub fn memory_path(&self, memory: &NaturalMemory) -> PathBuf {
        let month_dir = memory.created_at.format("%Y-%m").to_string();
        let id_short = &memory.id[..8.min(memory.id.len())];
        let slug = slugify(&memory.title);
        self.base_dir
            .join(month_dir)
            .join(format!("{id_short}-{slug}.md"))
    }

    /// Write a memory to a markdown file.
    pub fn write(&self, memory: &NaturalMemory) -> Result<PathBuf, AgentMemoryError> {
        let path = self.memory_path(memory);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let frontmatter = Frontmatter {
            id: memory.id.clone(),
            title: memory.title.clone(),
            tags: memory.tags.clone(),
            project: memory.project.clone(),
            source_task: memory.source_task.clone(),
            source_type: memory.source_type.clone(),
            created_at: memory.created_at,
            updated_at: memory.updated_at,
            last_accessed: memory.last_accessed,
            access_count: memory.access_count,
            pinned: memory.pinned,
        };

        let yaml = serde_yaml::to_string(&frontmatter)?;
        let content = format!("---\n{yaml}---\n\n{}\n", memory.content);
        std::fs::write(&path, content)?;

        Ok(path)
    }

    /// Read a memory from a markdown file.
    pub fn read(&self, path: &Path) -> Result<NaturalMemory, AgentMemoryError> {
        let raw = std::fs::read_to_string(path)?;
        parse_markdown(&raw, path)
    }

    /// Walk all markdown files in the base directory.
    pub fn walk_all(&self) -> Result<Vec<PathBuf>, AgentMemoryError> {
        if !self.base_dir.exists() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        for entry in walkdir::WalkDir::new(&self.base_dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.path().extension().is_some_and(|e| e == "md") {
                paths.push(entry.path().to_path_buf());
            }
        }
        Ok(paths)
    }

    /// Delete the markdown file for a memory.
    pub fn delete(&self, file_path: &str) -> Result<(), AgentMemoryError> {
        let path = Path::new(file_path);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

/// Parse a markdown file with YAML frontmatter into a NaturalMemory.
fn parse_markdown(raw: &str, path: &Path) -> Result<NaturalMemory, AgentMemoryError> {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return Err(AgentMemoryError::Other(format!(
            "no YAML frontmatter in {}",
            path.display()
        )));
    }

    let after_first = &trimmed[3..];
    let end_idx = after_first.find("\n---").ok_or_else(|| {
        AgentMemoryError::Other(format!("unterminated frontmatter in {}", path.display()))
    })?;

    let yaml_str = &after_first[..end_idx];
    let content_start = end_idx + 4; // skip \n---
    let content = after_first[content_start..].trim().to_string();

    let fm: Frontmatter = serde_yaml::from_str(yaml_str)?;

    Ok(NaturalMemory {
        id: fm.id,
        title: fm.title,
        content,
        tags: fm.tags,
        project: fm.project,
        source_task: fm.source_task,
        source_type: fm.source_type,
        file_path: path.to_string_lossy().to_string(),
        created_at: fm.created_at,
        updated_at: fm.updated_at,
        last_accessed: fm.last_accessed,
        access_count: fm.access_count,
        pinned: fm.pinned,
    })
}

/// Turn a title into a URL-safe slug.
fn slugify(title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse multiple dashes
    let mut result = String::new();
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash && !result.is_empty() {
                result.push('-');
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }
    // Trim trailing dash and limit length
    let trimmed = result.trim_end_matches('-');
    if trimmed.len() > 50 {
        trimmed[..50].trim_end_matches('-').to_string()
    } else {
        trimmed.to_string()
    }
}
