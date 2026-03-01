use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::runtime::{Runtime, TmuxRuntime};

pub use crate::runtime::SpawnResult;

pub fn create_worktree(repo_root: &Path, name: &str) -> Result<PathBuf> {
    TmuxRuntime.create_worktree(repo_root, name)
}

pub fn spawn_agent(task_name: &str, rendered_prompt: &str, work_dir: &Path) -> Result<SpawnResult> {
    TmuxRuntime.spawn_agent(task_name, rendered_prompt, work_dir)
}

pub fn send_keys_to_pane(pane_id: &str, message: &str) -> Result<()> {
    TmuxRuntime.send_keys_to_pane(pane_id, message)
}

pub fn capture_pane_output(pane_id: &str) -> Result<String> {
    TmuxRuntime.capture_pane_output(pane_id)
}

pub fn kill_tmux_window(window_id: &str) -> Result<()> {
    TmuxRuntime.kill_tmux_window(window_id)
}
