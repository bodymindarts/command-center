use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub struct SpawnResult {
    pub window_id: String,
    pub pane_id: String,
}

pub trait Runtime {
    fn create_worktree(&self, repo_root: &Path, name: &str) -> Result<PathBuf>;
    fn spawn_agent(
        &self,
        task_name: &str,
        system_prompt: Option<&str>,
        user_prompt: &str,
        work_dir: &Path,
    ) -> Result<SpawnResult>;
    fn resume_agent(&self, task_name: &str, work_dir: &Path) -> Result<SpawnResult>;
    fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> Result<()>;
    fn forward_key(&self, pane_id: &str, key: &str) -> Result<()>;
    fn forward_literal(&self, pane_id: &str, text: &str) -> Result<()>;
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

        // Pass commands directly to new-window / split-window so the pane
        // starts with the process already running — no send-keys race.
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
            "nvim",
            ".",
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
            claude_cmd,
        ])?;

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

            // Write settings with hooks and base allowed tools.
            // Hooks route permission requests to the dashboard.
            // Base allowed tools let agents run common safe commands
            // (git, cargo, nix, etc.) without manual approval each time.
            let source_settings = source_claude_dir.join("settings.local.json");
            let target_settings = target_claude_dir.join("settings.local.json");
            let mut settings = if source_settings.is_file()
                && let Ok(content) = std::fs::read_to_string(&source_settings)
                && let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&content)
                && parsed.get("hooks").is_some()
            {
                parsed.as_object_mut().unwrap().retain(|k, _| k == "hooks");
                parsed
            } else {
                serde_json::json!({})
            };
            settings["allowedTools"] = serde_json::json!(base_allowed_tools());
            std::fs::write(&target_settings, settings.to_string())?;
        }

        Ok(worktree_path)
    }

    fn spawn_agent(
        &self,
        task_name: &str,
        system_prompt: Option<&str>,
        user_prompt: &str,
        work_dir: &Path,
    ) -> Result<SpawnResult> {
        let claude_bin = self.resolve_binary("claude")?;

        // Write prompts to files and build a launcher script.
        // This avoids all shell-escaping and tmux send-keys issues —
        // the pane starts with the script as its command, no send-keys needed.
        std::fs::write(work_dir.join(".claude-prompt.txt"), user_prompt)?;

        let mut script = format!("#!/bin/sh\nunset CLAUDECODE\nexec {claude_bin}");
        script.push_str(" -p \"$(cat .claude-prompt.txt)\"");
        if let Some(sys) = system_prompt {
            std::fs::write(work_dir.join(".claude-system-prompt.txt"), sys)?;
            script.push_str(" --system-prompt \"$(cat .claude-system-prompt.txt)\"");
        }

        let script_path = work_dir.join(".claude-launch.sh");
        std::fs::write(&script_path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
        }

        self.launch_agent_window(task_name, work_dir, "sh .claude-launch.sh")
    }

    fn resume_agent(&self, task_name: &str, work_dir: &Path) -> Result<SpawnResult> {
        let claude_bin = self.resolve_binary("claude")?;
        let claude_cmd = format!("env -u CLAUDECODE {claude_bin} --continue");

        self.launch_agent_window(task_name, work_dir, &claude_cmd)
    }

    fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> Result<()> {
        self.tmux_cmd(&["send-keys", "-t", pane_id, "-l", message])?;
        self.tmux_cmd(&["send-keys", "-t", pane_id, "Enter"])?;
        Ok(())
    }

    fn forward_key(&self, pane_id: &str, key: &str) -> Result<()> {
        self.tmux_cmd(&["send-keys", "-t", pane_id, key])?;
        Ok(())
    }

    fn forward_literal(&self, pane_id: &str, text: &str) -> Result<()> {
        self.tmux_cmd(&["send-keys", "-t", pane_id, "-l", text])?;
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

/// Base set of tool permissions that every spawned agent inherits.
/// These cover common safe operations so agents don't need manual
/// approval for routine dev workflow commands.
fn base_allowed_tools() -> Vec<&'static str> {
    vec![
        // Git (read-only + staging/committing — no push/force)
        "Bash(git status:*)",
        "Bash(git diff:*)",
        "Bash(git add:*)",
        "Bash(git log:*)",
        "Bash(git commit:*)",
        "Bash(git branch:*)",
        "Bash(git show:*)",
        // Nix
        "Bash(nix flake check:*)",
        "Bash(nix develop:*)",
        "Bash(nix build:*)",
        // Cargo (typically run inside nix develop, but allow direct too)
        "Bash(cargo fmt:*)",
        "Bash(cargo clippy:*)",
        "Bash(cargo nextest:*)",
        "Bash(cargo build:*)",
        "Bash(cargo test:*)",
        "Bash(cargo check:*)",
        // Basic read-only shell commands
        "Bash(ls:*)",
        "Bash(cat:*)",
        "Bash(head:*)",
        "Bash(tail:*)",
        "Bash(wc:*)",
        "Bash(which:*)",
        "Bash(pwd)",
    ]
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

/// Returns a mapping from tmux window ID (e.g. "@24") to window index (e.g. "2").
pub fn tmux_window_numbers() -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(output) = tmux_cmd(&["list-windows", "-F", "#{window_id} #{window_index}"]) {
        for line in output.lines() {
            if let Some((id, index)) = line.split_once(' ') {
                map.insert(id.to_string(), index.to_string());
            }
        }
    }
    map
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
