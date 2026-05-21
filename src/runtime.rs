use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::primitives::{PaneId, WindowId};

pub struct SpawnResult {
    pub window_id: WindowId,
    pub pane_id: PaneId,
}

/// `Runtime` abstracts how task agents are laid out in their environment —
/// today this means git worktrees + tmux windows/panes. Exactly one runtime
/// is in use at a time (the workspace's window manager), so `ClatApp` is
/// generic over `R: Runtime` rather than dynamically dispatched.
///
/// Anything harness-specific (writing `.claude/`, launching the `claude`
/// CLI, idle heuristics) lives in [`crate::harness::Harness`] instead.
pub trait Runtime: Send + Sync + 'static {
    // ── git worktree / scratch dir ──────────────────────────────────
    fn create_worktree(
        &self,
        repo_root: &Path,
        name: &str,
        branch: Option<&str>,
    ) -> anyhow::Result<PathBuf>;
    fn recreate_worktree(&self, repo_root: &Path, work_dir: &Path) -> anyhow::Result<()>;
    fn remove_worktree(&self, path: &Path) -> anyhow::Result<()>;
    fn init_scratch_dir(&self, scratch_dir: &Path) -> anyhow::Result<()>;

    // ── tmux window / pane ──────────────────────────────────────────
    /// Open a new tmux window with the standard 3-pane layout (editor on top,
    /// shell bottom-left, agent bottom-right) and run `agent_cmd` in the
    /// agent pane. Returns the window/pane IDs.
    fn launch_agent_window(
        &self,
        task_name: &str,
        work_dir: &Path,
        agent_cmd: &str,
    ) -> anyhow::Result<SpawnResult>;
    fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> anyhow::Result<()>;
    fn capture_pane_output(&self, pane_id: &str) -> anyhow::Result<String>;
    fn select_window(&self, window_id: &str) -> anyhow::Result<()>;
    fn kill_tmux_window(&self, window_id: &str) -> anyhow::Result<()>;
}

/// Default `Runtime` impl: tmux for windowing, `git worktree` for isolation.
pub struct TmuxRuntime;

impl TmuxRuntime {
    fn tmux_cmd(&self, args: &[&str]) -> anyhow::Result<String> {
        tmux_cmd(args)
    }
}

impl Runtime for TmuxRuntime {
    fn init_scratch_dir(&self, scratch_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(scratch_dir)?;

        let output = Command::new("git")
            .args(["init"])
            .current_dir(scratch_dir)
            .output()
            .context("failed to run git init")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git init failed: {stderr}");
        }

        Ok(())
    }

    fn create_worktree(
        &self,
        repo_root: &Path,
        name: &str,
        branch: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        let worktree_dir = repo_root.join(".claude").join("worktrees");
        std::fs::create_dir_all(&worktree_dir)?;

        let worktree_path = worktree_dir.join(name);

        let mut git_args = vec![
            "worktree".to_string(),
            "add".to_string(),
            worktree_path.display().to_string(),
        ];
        if let Some(existing_branch) = branch {
            // Check out an existing branch
            git_args.push(existing_branch.to_string());
        } else {
            // Create a new branch from HEAD
            let branch_name = format!("task/{name}");
            git_args.push("-b".to_string());
            git_args.push(branch_name);
        }

        let output = Command::new("git")
            .args(&git_args)
            .current_dir(repo_root)
            .output()
            .context("failed to run git worktree add")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git worktree add failed: {stderr}");
        }

        Ok(worktree_path)
    }

    fn recreate_worktree(&self, repo_root: &Path, work_dir: &Path) -> anyhow::Result<()> {
        let name = work_dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid worktree path: {}", work_dir.display()))?;
        let branch_name = format!("task/{name}");

        // Clean up stale worktree bookkeeping so git doesn't reject the add.
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(repo_root)
            .output();

        // Check whether the branch still exists (it usually survives merge).
        let branch_check = Command::new("git")
            .args(["branch", "--list", &branch_name])
            .current_dir(repo_root)
            .output()
            .context("failed to check branch existence")?;
        let branch_exists = !String::from_utf8_lossy(&branch_check.stdout)
            .trim()
            .is_empty();

        let output = if branch_exists {
            Command::new("git")
                .args([
                    "worktree",
                    "add",
                    &work_dir.display().to_string(),
                    &branch_name,
                ])
                .current_dir(repo_root)
                .output()
                .context("failed to run git worktree add")?
        } else {
            // Branch was deleted after merge — create a fresh one from HEAD.
            Command::new("git")
                .args([
                    "worktree",
                    "add",
                    &work_dir.display().to_string(),
                    "-b",
                    &branch_name,
                ])
                .current_dir(repo_root)
                .output()
                .context("failed to run git worktree add")?
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git worktree add failed: {stderr}");
        }

        Ok(())
    }

    fn launch_agent_window(
        &self,
        task_name: &str,
        work_dir: &Path,
        agent_cmd: &str,
    ) -> anyhow::Result<SpawnResult> {
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

        let agent_pane = self.tmux_cmd(&[
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

        self.tmux_cmd(&["send-keys", "-t", &agent_pane, "-l", agent_cmd])?;
        self.tmux_cmd(&["send-keys", "-t", &agent_pane, "Enter"])?;
        self.tmux_cmd(&["send-keys", "-t", &top_pane, "-l", "nvim ."])?;
        self.tmux_cmd(&["send-keys", "-t", &top_pane, "Enter"])?;

        Ok(SpawnResult {
            window_id: WindowId::from(window_id),
            pane_id: PaneId::from(agent_pane),
        })
    }

    fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> anyhow::Result<()> {
        use std::io::Write as _;

        if message.trim().is_empty() {
            bail!("refusing to send empty message");
        }

        // Claude Code uses Ink which enables bracketed paste mode.
        // `send-keys -l` delivers individual key events without paste
        // markers, so Ink handles the input unreliably.  Using tmux's
        // paste buffer with `-p` wraps content in bracketed paste
        // escapes (\e[200~ … \e[201~) so Ink receives a proper paste
        // event.
        //
        // Named buffers keyed to pane ID prevent concurrent sends to
        // different agents from clobbering each other's paste content.
        let buf_name = format!("cc-{pane_id}");

        let mut child = Command::new("tmux")
            .args(["load-buffer", "-b", &buf_name, "-"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .context("failed to spawn tmux load-buffer")?;

        let mut stdin = child
            .stdin
            .take()
            .context("stdin not available despite Stdio::piped()")?;
        stdin.write_all(message.as_bytes())?;
        drop(stdin); // close → EOF for tmux

        let status = child.wait().context("tmux load-buffer failed")?;
        if !status.success() {
            bail!("tmux load-buffer exited with non-zero status");
        }

        // Paste with bracketed-paste markers (-p), suppress LF→CR
        // substitution (-r), and delete the buffer afterwards (-d).
        self.tmux_cmd(&[
            "paste-buffer",
            "-p",
            "-r",
            "-d",
            "-b",
            &buf_name,
            "-t",
            pane_id,
        ])?;

        // Brief pause for Ink to process the paste event.
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Submit the pasted text.
        self.tmux_cmd(&["send-keys", "-t", pane_id, "Enter"])?;
        Ok(())
    }

    fn capture_pane_output(&self, pane_id: &str) -> anyhow::Result<String> {
        self.tmux_cmd(&["capture-pane", "-p", "-S", "-", "-t", pane_id])
    }

    fn remove_worktree(&self, path: &Path) -> anyhow::Result<()> {
        let output = Command::new("git")
            .args(["worktree", "remove", "--force", &path.display().to_string()])
            .output()
            .context("failed to run git worktree remove")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git worktree remove failed: {stderr}");
        }

        Ok(())
    }

    fn kill_tmux_window(&self, window_id: &str) -> anyhow::Result<()> {
        self.tmux_cmd(&["kill-window", "-t", window_id])?;
        Ok(())
    }

    fn select_window(&self, window_id: &str) -> anyhow::Result<()> {
        self.tmux_cmd(&["select-window", "-t", window_id])?;
        Ok(())
    }
}

/// Returns a mapping from tmux window ID (e.g. "@24") to window index (e.g. "2").
pub fn tmux_window_numbers() -> HashMap<WindowId, String> {
    let mut map = HashMap::new();
    if let Ok(output) = tmux_cmd(&["list-windows", "-F", "#{window_id} #{window_index}"]) {
        for line in output.lines() {
            if let Some((id, index)) = line.split_once(' ') {
                map.insert(WindowId::from(id.to_string()), index.to_string());
            }
        }
    }
    map
}

/// Free function for workspace bootstrapping (cmd_start), not a task operation.
pub fn tmux_cmd(args: &[&str]) -> anyhow::Result<String> {
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
