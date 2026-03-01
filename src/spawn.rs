use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub fn create_worktree(repo_root: &Path, name: &str) -> Result<PathBuf> {
    let worktree_dir = repo_root.join(".claude").join("worktrees");
    std::fs::create_dir_all(&worktree_dir)?;

    let worktree_path = worktree_dir.join(name);
    let branch_name = format!("task/{name}");

    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            &worktree_path.display().to_string(),
            "-b",
            &branch_name,
        ])
        .current_dir(repo_root)
        .output()
        .context("failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {stderr}");
    }

    // Symlink project .claude/ into worktree so spawned agents inherit hooks
    let source_claude_dir = repo_root.join(".claude");
    let target_claude_dir = worktree_path.join(".claude");
    if source_claude_dir.is_dir() && !target_claude_dir.exists() {
        std::os::unix::fs::symlink(&source_claude_dir, &target_claude_dir)
            .context("failed to symlink .claude/ into worktree")?;
    }

    Ok(worktree_path)
}

pub struct SpawnResult {
    pub window_id: String,
    pub pane_id: String,
}

pub fn spawn_agent(task_name: &str, rendered_prompt: &str, work_dir: &Path) -> Result<SpawnResult> {
    if std::env::var("TMUX").is_err() {
        bail!("clat spawn must be run inside a tmux session");
    }

    // Resolve claude absolute path while we're still in the nix shell
    let claude_bin = resolve_binary("claude")?;

    // Write skill prompt to TASK.md in the worktree so claude has context
    let task_md = work_dir.join("TASK.md");
    std::fs::write(&task_md, rendered_prompt).context("failed to write TASK.md")?;

    let work_dir_str = work_dir.display().to_string();
    let window_name = format!("cc:{task_name}");

    // 1. Create new window in background (-d) so current pane keeps focus
    let window_id = tmux_cmd(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-n",
        &window_name,
        "-c",
        &work_dir_str,
    ])?;

    // Get the initial pane ID (top pane)
    let top_pane = tmux_cmd(&["list-panes", "-t", &window_id, "-F", "#{pane_id}"])?;

    // 2. Split vertically to create a bottom pane
    let bottom_pane = tmux_cmd(&[
        "split-window",
        "-v",
        "-t",
        &top_pane,
        "-P",
        "-F",
        "#{pane_id}",
        "-c",
        &work_dir_str,
    ])?;

    // 3. Resize: push the divider down so top pane is bigger
    tmux_cmd(&["resize-pane", "-t", &top_pane, "-D", "8"])?;

    // 4. Split bottom pane horizontally → bottom-left (shell) and bottom-right (claude)
    let claude_pane = tmux_cmd(&[
        "split-window",
        "-h",
        "-t",
        &bottom_pane,
        "-P",
        "-F",
        "#{pane_id}",
        "-c",
        &work_dir_str,
    ])?;

    // 5. Launch interactive claude in the agent pane (stays open for chatting)
    tmux_cmd(&["send-keys", "-t", &claude_pane, &claude_bin, "Enter"])?;

    // 6. Open nvim in top pane
    tmux_cmd(&["send-keys", "-t", &top_pane, "nvim .", "Enter"])?;

    Ok(SpawnResult {
        window_id,
        pane_id: claude_pane,
    })
}

fn resolve_binary(name: &str) -> Result<String> {
    let output = Command::new("which")
        .arg(name)
        .output()
        .with_context(|| format!("failed to find {name}"))?;

    if !output.status.success() {
        bail!("{name} not found in PATH");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn tmux_cmd(args: &[&str]) -> Result<String> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .with_context(|| format!("failed to run tmux {}", args[0]))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("tmux {} failed: {stderr}", args[0]);
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn send_keys_to_pane(pane_id: &str, message: &str) -> Result<()> {
    tmux_cmd(&["send-keys", "-t", pane_id, "-l", message])?;
    tmux_cmd(&["send-keys", "-t", pane_id, "Enter"])?;
    Ok(())
}

pub fn kill_tmux_window(window_id: &str) -> Result<()> {
    tmux_cmd(&["kill-window", "-t", window_id])?;
    Ok(())
}

pub fn capture_pane_output(pane_id: &str) -> Result<String> {
    tmux_cmd(&["capture-pane", "-p", "-S", "-", "-t", pane_id])
}
