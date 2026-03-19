use std::path::PathBuf;

/// Configuration for agent-memory.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory where markdown memory files are stored.
    pub memories_dir: PathBuf,
    /// Path to the SQLite database file.
    pub db_path: PathBuf,
}
