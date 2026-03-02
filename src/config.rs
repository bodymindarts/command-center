use std::path::PathBuf;

use anyhow::{Context, Result};

pub struct Paths {
    pub root: PathBuf,
    pub skills_dir: PathBuf,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let root = find_project_root()?;
        let skills_dir = root.join("skills");
        let data_dir = root.join("data");
        let db_path = data_dir.join("cc.db");

        Ok(Self {
            root,
            skills_dir,
            data_dir,
            db_path,
        })
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir).context("failed to create data directory")?;
        Ok(())
    }

    pub fn exo_session_file(&self) -> PathBuf {
        self.data_dir.join("exo-session-id")
    }
}

fn find_project_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir().context("failed to get current directory")?;
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("skills").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    // Fall back to current directory
    std::env::current_dir().context("failed to get current directory")
}
