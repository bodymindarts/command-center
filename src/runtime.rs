use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub struct SpawnResult {
    pub window_id: String,
    pub pane_id: String,
}

pub trait Runtime {
    fn create_worktree(&self, repo_root: &Path, name: &str) -> Result<PathBuf>;
    fn spawn_agent(&self, task_name: &str, prompt: &str, work_dir: &Path) -> Result<SpawnResult>;
    fn resume_agent(&self, task_name: &str, work_dir: &Path) -> Result<SpawnResult>;
    fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> Result<()>;
    fn capture_pane_output(&self, pane_id: &str) -> Result<String>;
    fn kill_tmux_window(&self, window_id: &str) -> Result<()>;
    fn select_window(&self, window_id: &str) -> Result<()>;
}

pub struct TmuxRuntime;

impl TmuxRuntime {
    fn tmux_cmd(&self, args: &[&str]) -> Result<String> {
        tmux_cmd(args)
    }

    fn resolve_binary(&self, name: &str) -> Result<String> {
        let output = Command::new("which")
            .arg(name)
            .output()
            .with_context(|| format!("failed to find {name}"))?;

        if !output.status.success() {
            bail!("{name} not found in PATH");
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn launch_agent_window(
        &self,
        task_name: &str,
        work_dir: &Path,
        claude_cmd: &str,
    ) -> Result<SpawnResult> {
        if std::env::var("TMUX").is_err() {
            bail!("clat spawn must be run inside a tmux session");
        }

        let work_dir_str = work_dir.display().to_string();
        let window_name = format!("cc:{task_name}");

        let window_id = self.tmux_cmd(&[
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

        let top_pane = self.tmux_cmd(&["list-panes", "-t", &window_id, "-F", "#{pane_id}"])?;

        let bottom_pane = self.tmux_cmd(&[
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

        self.tmux_cmd(&["resize-pane", "-t", &top_pane, "-D", "8"])?;

        let claude_pane = self.tmux_cmd(&[
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

        self.tmux_cmd(&["send-keys", "-t", &claude_pane, claude_cmd, "Enter"])?;
        self.tmux_cmd(&["send-keys", "-t", &top_pane, "nvim .", "Enter"])?;

        Ok(SpawnResult {
            window_id,
            pane_id: claude_pane,
        })
    }
}

impl Runtime for TmuxRuntime {
    fn create_worktree(&self, repo_root: &Path, name: &str) -> Result<PathBuf> {
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

        // Copy hooks config into worktree so spawned agents route permissions
        // through the dashboard. We copy instead of symlinking because Claude
        // replaces symlinked .claude/ dirs with real ones when writing settings,
        // which loses the hooks config.
        let source_claude_dir = repo_root.join(".claude");
        let target_claude_dir = worktree_path.join(".claude");
        if source_claude_dir.is_dir() {
            std::fs::create_dir_all(&target_claude_dir)?;

            // Copy hooks directory
            let source_hooks = source_claude_dir.join("hooks");
            let target_hooks = target_claude_dir.join("hooks");
            if source_hooks.is_dir() {
                copy_dir_recursive(&source_hooks, &target_hooks)?;
            }

            // Write settings with just the hooks section from the source.
            // Agents earn their own permissions — we only propagate hooks.
            let source_settings = source_claude_dir.join("settings.local.json");
            if source_settings.is_file()
                && let Ok(content) = std::fs::read_to_string(&source_settings)
                && let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&content)
                && parsed.get("hooks").is_some()
            {
                parsed.as_object_mut().unwrap().retain(|k, _| k == "hooks");
                let target_settings = target_claude_dir.join("settings.local.json");
                std::fs::write(&target_settings, parsed.to_string())?;
            }
        }

        Ok(worktree_path)
    }

    fn spawn_agent(
        &self,
        task_name: &str,
        rendered_prompt: &str,
        work_dir: &Path,
    ) -> Result<SpawnResult> {
        let claude_bin = self.resolve_binary("claude")?;

        // Write skill prompt to TASK.md in the worktree so claude has context
        let task_md = work_dir.join("TASK.md");
        std::fs::write(&task_md, rendered_prompt).context("failed to write TASK.md")?;

        let claude_cmd =
            format!("{claude_bin} \"Read TASK.md and complete the task described in it.\"");
        self.launch_agent_window(task_name, work_dir, &claude_cmd)
    }

    fn resume_agent(&self, task_name: &str, work_dir: &Path) -> Result<SpawnResult> {
        let claude_bin = self.resolve_binary("claude")?;
        let claude_cmd = format!("{claude_bin} --continue");

        self.launch_agent_window(task_name, work_dir, &claude_cmd)
    }

    fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> Result<()> {
        self.tmux_cmd(&["send-keys", "-t", pane_id, "-l", message])?;
        self.tmux_cmd(&["send-keys", "-t", pane_id, "Enter"])?;
        Ok(())
    }

    fn capture_pane_output(&self, pane_id: &str) -> Result<String> {
        self.tmux_cmd(&["capture-pane", "-p", "-S", "-", "-t", pane_id])
    }

    fn kill_tmux_window(&self, window_id: &str) -> Result<()> {
        self.tmux_cmd(&["kill-window", "-t", window_id])?;
        Ok(())
    }

    fn select_window(&self, window_id: &str) -> Result<()> {
        self.tmux_cmd(&["select-window", "-t", window_id])?;
        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Free function for workspace bootstrapping (cmd_start), not a task operation.
pub fn tmux_cmd(args: &[&str]) -> Result<String> {
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
