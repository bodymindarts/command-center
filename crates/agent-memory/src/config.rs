use std::path::PathBuf;

/// Configuration for agent-memory.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory where markdown memory files are stored.
    pub memories_dir: PathBuf,
    /// Path to the SQLite database file.
    pub db_path: PathBuf,
    /// Half-life for memory decay in days (default: 14.0).
    pub decay_half_life_days: f64,
    /// Minimum strength before a memory is filtered out (default: 0.05).
    pub decay_min_strength: f64,
    /// Whether decay is enabled (default: true).
    pub decay_enabled: bool,
}

impl Config {
    /// Default decay half-life in days.
    pub const DEFAULT_HALF_LIFE_DAYS: f64 = 14.0;
    /// Default minimum strength threshold.
    pub const DEFAULT_MIN_STRENGTH: f64 = 0.05;
}
