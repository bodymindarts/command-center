use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::primitives::{PaneId, WindowId};
use crate::skill::BaseTools;

pub struct SpawnResult {
    pub window_id: WindowId,
    pub pane_id: PaneId,
}

pub struct LaunchConfig<'a> {
    pub task_name: &'a str,
    pub session_id: &'a str,
    pub system_prompt: Option<&'a str>,
    pub work_dir: &'a Path,
    /// Some = Full mode (--system-prompt), None = Interactive (--append-system-prompt + idle prompt)
    pub user_prompt: Option<&'a str>,
    /// When true, pass `--dangerously-skip-permissions` to the claude subprocess.
    pub skip_permissions: bool,
}

/// Bundled permission info extracted from a skill's `[agent]` section.
/// Passed to worktree/config setup so the correct tools are auto-approved.
pub struct SkillPermissions<'a> {
    pub allowed_tools: &'a [String],
    pub base_tools: &'a BaseTools,
    pub bash_patterns: &'a [String],
}

impl Default for SkillPermissions<'_> {
    fn default() -> Self {
        Self {
            allowed_tools: &[],
            base_tools: &BaseTools::Full,
            bash_patterns: &[],
        }
    }
}

pub trait Runtime {
    fn create_worktree(
        &self,
        repo_root: &Path,
        name: &str,
        perms: &SkillPermissions,
        branch: Option<&str>,
        hooks_source: &Path,
        jwt_token: &str,
    ) -> anyhow::Result<PathBuf>;
    fn recreate_worktree(
        &self,
        repo_root: &Path,
        work_dir: &Path,
        jwt_token: &str,
    ) -> anyhow::Result<()>;
    fn setup_dir_config(
        &self,
        hooks_source: &Path,
        work_dir: &Path,
        perms: &SkillPermissions,
        jwt_token: &str,
    ) -> anyhow::Result<()>;
    fn init_scratch_dir(&self, scratch_dir: &Path) -> anyhow::Result<()>;
    fn launch_agent(&self, config: LaunchConfig) -> anyhow::Result<SpawnResult>;
    fn resume_agent(
        &self,
        task_name: &str,
        session_id: &str,
        work_dir: &Path,
        skip_permissions: bool,
    ) -> anyhow::Result<SpawnResult>;
    fn relaunch_agent(&self, task_name: &str, work_dir: &Path) -> anyhow::Result<SpawnResult>;
    fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> anyhow::Result<()>;
    fn capture_pane_output(&self, pane_id: &str) -> anyhow::Result<String>;
    fn remove_worktree(&self, path: &Path) -> anyhow::Result<()>;
    fn kill_tmux_window(&self, window_id: &str) -> anyhow::Result<()>;
    fn select_window(&self, window_id: &str) -> anyhow::Result<()>;
}

pub struct TmuxRuntime;

impl TmuxRuntime {
    fn tmux_cmd(&self, args: &[&str]) -> anyhow::Result<String> {
        tmux_cmd(args)
    }

    fn resolve_binary(&self, name: &str) -> anyhow::Result<String> {
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

        self.tmux_cmd(&["send-keys", "-t", &claude_pane, "-l", claude_cmd])?;
        self.tmux_cmd(&["send-keys", "-t", &claude_pane, "Enter"])?;
        self.tmux_cmd(&["send-keys", "-t", &top_pane, "-l", "nvim ."])?;
        self.tmux_cmd(&["send-keys", "-t", &top_pane, "Enter"])?;

        Ok(SpawnResult {
            window_id: WindowId::from(window_id),
            pane_id: PaneId::from(claude_pane),
        })
    }
}

impl Runtime for TmuxRuntime {
    fn setup_dir_config(
        &self,
        hooks_source: &Path,
        work_dir: &Path,
        perms: &SkillPermissions,
        jwt_token: &str,
    ) -> anyhow::Result<()> {
        setup_worktree_config(hooks_source, work_dir, perms, jwt_token)
    }

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
        perms: &SkillPermissions,
        branch: Option<&str>,
        hooks_source: &Path,
        jwt_token: &str,
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

        setup_worktree_config(hooks_source, &worktree_path, perms, jwt_token)?;
        merge_repo_settings(repo_root, &worktree_path)?;

        Ok(worktree_path)
    }

    fn recreate_worktree(
        &self,
        repo_root: &Path,
        work_dir: &Path,
        jwt_token: &str,
    ) -> anyhow::Result<()> {
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

        setup_worktree_config(repo_root, work_dir, &SkillPermissions::default(), jwt_token)?;
        Ok(())
    }

    fn launch_agent(&self, config: LaunchConfig) -> anyhow::Result<SpawnResult> {
        let claude_bin = self.resolve_binary("claude")?;

        let claude_dir = config.work_dir.join(".claude");
        std::fs::create_dir_all(&claude_dir)?;

        let mut script = format!("#!/bin/sh\nunset CLAUDECODE\nexec {claude_bin}");
        if config.skip_permissions {
            script.push_str(" --dangerously-skip-permissions");
        }
        script.push_str(&format!(" --session-id {}", config.session_id));

        if let Some(user_prompt) = config.user_prompt {
            // Full mode: write user prompt to file, use --system-prompt
            std::fs::write(claude_dir.join("prompt.txt"), user_prompt)?;
            script.push_str(" \"$(cat .claude/prompt.txt)\"");
            if let Some(sys) = config.system_prompt {
                std::fs::write(claude_dir.join("system-prompt.txt"), sys)?;
                script.push_str(" --system-prompt \"$(cat .claude/system-prompt.txt)\"");
            }
        } else {
            // Interactive mode: idle prompt, use --append-system-prompt
            std::fs::write(
                claude_dir.join("idle-prompt.txt"),
                "Await further instructions.",
            )?;
            script.push_str(" \"$(cat .claude/idle-prompt.txt)\"");
            if let Some(sys) = config.system_prompt {
                std::fs::write(claude_dir.join("system-prompt.txt"), sys)?;
                script.push_str(" --append-system-prompt \"$(cat .claude/system-prompt.txt)\"");
            }
        }

        let script_path = claude_dir.join("launch.sh");
        std::fs::write(&script_path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
        }

        self.launch_agent_window(config.task_name, config.work_dir, "sh .claude/launch.sh")
    }

    fn resume_agent(
        &self,
        task_name: &str,
        session_id: &str,
        work_dir: &Path,
        skip_permissions: bool,
    ) -> anyhow::Result<SpawnResult> {
        let claude_bin = self.resolve_binary("claude")?;
        let skip_flag = if skip_permissions {
            " --dangerously-skip-permissions"
        } else {
            ""
        };
        let claude_cmd = format!("env -u CLAUDECODE {claude_bin}{skip_flag} --resume {session_id}");

        self.launch_agent_window(task_name, work_dir, &claude_cmd)
    }

    fn relaunch_agent(&self, task_name: &str, work_dir: &Path) -> anyhow::Result<SpawnResult> {
        self.launch_agent_window(task_name, work_dir, "sh .claude/launch.sh")
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

/// Copy hooks config and write settings into a worktree's `.claude/` directory.
/// This is shared between initial creation and worktree recreation (reopen).
fn setup_worktree_config(
    repo_root: &Path,
    worktree_path: &Path,
    perms: &SkillPermissions,
    jwt_token: &str,
) -> anyhow::Result<()> {
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
        let target_settings = target_claude_dir.join("settings.local.json");
        let mut settings = serde_json::json!({
            "hooks": hooks_json()
        });
        // Merge skill-level tools (Read, Glob, Edit, etc.) with base
        // Bash-pattern tools (nix develop, cargo fmt, etc.) into a single
        // permissions.allow list.  Claude Code reads this key from settings
        // files — "allowedTools" is only valid as a CLI flag.
        let mut allowed: Vec<String> = perms.allowed_tools.to_vec();
        for tool in base_tools_for(perms.base_tools) {
            allowed.push(tool.to_string());
        }
        for pattern in perms.bash_patterns {
            allowed.push(format!("Bash({pattern})"));
        }
        settings["permissions"] = serde_json::json!({"allow": allowed});
        // Embed CC_PERM_SOCKET into hook commands so agents connect
        // to this dashboard's session-scoped permission socket.
        // Try env var first (TUI process), then breadcrumb file (CLI spawns).
        let sock_path = std::env::var(crate::permission::SOCKET_ENV)
            .ok()
            .or_else(|| crate::permission::read_socket_breadcrumb(repo_root));
        if let Some(sock_path) = sock_path {
            embed_socket_in_hooks(&mut settings, &sock_path);
        }

        // Write .mcp.json at worktree root so Claude Code discovers the MCP server.
        // Claude Code reads MCP servers from .mcp.json (project scope), NOT from
        // the mcpServers key in settings.local.json.
        if let Some(mcp_url) = crate::mcp::read_mcp_url_breadcrumb(repo_root) {
            // Include the JWT in both the URL query param and Authorization header
            // for resilience against Claude Code header bugs.
            let mcp_server = serde_json::json!({
                "type": "http",
                "url": format!("{mcp_url}?token={jwt_token}"),
                "headers": {
                    "Authorization": format!("Bearer {jwt_token}")
                }
            });
            let mcp_json = serde_json::json!({
                "mcpServers": {
                    "clat": mcp_server
                }
            });
            std::fs::write(
                worktree_path.join(".mcp.json"),
                serde_json::to_string_pretty(&mcp_json)?,
            )?;
            settings["enableAllProjectMcpServers"] = serde_json::json!(true);

            // Ensure .mcp.json is gitignored in the worktree.
            let gitignore_path = worktree_path.join(".gitignore");
            let content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
            if !content.lines().any(|l| l.trim() == ".mcp.json") {
                let mut new_content = content;
                if !new_content.is_empty() && !new_content.ends_with('\n') {
                    new_content.push('\n');
                }
                new_content.push_str(".mcp.json\n");
                std::fs::write(&gitignore_path, new_content)?;
            }
        }

        std::fs::write(&target_settings, settings.to_string())?;

        // Ignore all generated files so agents don't commit them.
        std::fs::write(
            target_claude_dir.join(".gitignore"),
            "launch.sh\nprompt.txt\nidle-prompt.txt\nsystem-prompt.txt\nsettings.local.json\nhooks/\n.gitignore\nperm-socket\nskip-permissions\n",
        )?;
    }
    Ok(())
}

/// Merge non-managed keys from the source repo's `.claude/settings.local.json`
/// into the worktree's settings. Keys like `mcpServers` are preserved while
/// managed keys (`hooks`, `permissions`) already set by [`setup_worktree_config`]
/// take precedence.
fn merge_repo_settings(repo_root: &Path, worktree_path: &Path) -> anyhow::Result<()> {
    let repo_settings_path = repo_root.join(".claude").join("settings.local.json");
    if !repo_settings_path.is_file() {
        return Ok(());
    }

    let repo_content = std::fs::read_to_string(&repo_settings_path)?;
    let repo_settings: serde_json::Value = serde_json::from_str(&repo_content)?;
    let Some(repo_obj) = repo_settings.as_object() else {
        return Ok(());
    };

    let wt_settings_path = worktree_path.join(".claude").join("settings.local.json");
    let wt_content = std::fs::read_to_string(&wt_settings_path).unwrap_or_default();
    let mut wt_settings: serde_json::Value =
        serde_json::from_str(&wt_content).unwrap_or_else(|_| serde_json::json!({}));

    if let Some(wt_obj) = wt_settings.as_object_mut() {
        for (key, value) in repo_obj {
            if !wt_obj.contains_key(key) {
                wt_obj.insert(key.clone(), value.clone());
            }
        }
    }

    std::fs::write(&wt_settings_path, wt_settings.to_string())?;
    Ok(())
}

/// Generate the hooks JSON for spawned agent settings.
///
/// Hook events:
/// - `Notification` with matchers for idle/active detection
/// - `PostToolUse` for in-pane permission clearing
/// - `PermissionRequest` for routing permissions to the dashboard
/// - `PreToolUse` for pre-execution observation
/// - `Stop` for agent stop signals
/// - `UserPromptSubmit` for user prompt tracking
/// - `SubagentStop` for sub-agent lifecycle tracking
fn hooks_json() -> serde_json::Value {
    let hook = |script: &str, timeout: u64| -> serde_json::Value {
        serde_json::json!({
            "type": "command",
            "command": format!("\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/{script}"),
            "timeout": timeout
        })
    };

    serde_json::json!({
        "Notification": [
            {
                "matcher": "idle_prompt",
                "hooks": [hook("notification-idle.sh", 10)]
            },
            {
                "matcher": "permission_prompt",
                "hooks": [hook("notification-active.sh", 10)]
            },
            {
                "matcher": "elicitation_dialog",
                "hooks": [hook("notification-active.sh", 10)]
            }
        ],
        "PostToolUse": [
            { "hooks": [hook("post-tool-resolved.sh", 10)] }
        ],
        "PermissionRequest": [
            { "hooks": [hook("permission-gate.sh", 620)] }
        ],
        "PreToolUse": [
            { "hooks": [hook("pre-tool-use.sh", 10)] }
        ],
        "Stop": [
            { "hooks": [hook("stop.sh", 10)] }
        ],
        "UserPromptSubmit": [
            { "hooks": [hook("user-prompt-submit.sh", 10)] }
        ],
        "SubagentStop": [
            { "hooks": [hook("subagent-stop.sh", 10)] }
        ]
    })
}

/// Return the base tool set for the given tier.
fn base_tools_for(bt: &BaseTools) -> Vec<&'static str> {
    match bt {
        BaseTools::Full => base_allowed_tools_full(),
        BaseTools::Minimal => base_allowed_tools_minimal(),
        BaseTools::None => vec![],
    }
}

/// Full base set: all git/cargo/nix/shell tools.
/// Used by engineer, reviewer, researcher, and other dev-oriented skills.
fn base_allowed_tools_full() -> Vec<&'static str> {
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

/// Minimal base set: only basic read-only shell commands.
/// Used by non-dev skills like reporter that don't need git/cargo/nix.
fn base_allowed_tools_minimal() -> Vec<&'static str> {
    vec![
        "Bash(ls:*)",
        "Bash(cat:*)",
        "Bash(head:*)",
        "Bash(tail:*)",
        "Bash(wc:*)",
        "Bash(which:*)",
        "Bash(pwd)",
    ]
}

/// Rewrite hook commands in settings JSON to prefix `CC_PERM_SOCKET=<path>`,
/// so spawned agents' hooks connect to the correct dashboard socket.
/// Matches any hook command under `.claude/hooks/`.
fn embed_socket_in_hooks(settings: &mut serde_json::Value, sock_path: &str) {
    let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return;
    };
    for hook_list in hooks.values_mut() {
        let Some(matchers) = hook_list.as_array_mut() else {
            continue;
        };
        for matcher in matchers {
            let Some(hook_arr) = matcher.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                continue;
            };
            for hook in hook_arr {
                if hook.get("type").and_then(|t| t.as_str()) == Some("command")
                    && let Some(cmd) = hook.get("command").and_then(|c| c.as_str())
                    && cmd.contains(".claude/hooks/")
                {
                    // Strip existing CC_PERM_SOCKET= prefix to avoid stacking
                    let clean_cmd =
                        if let Some(rest) = cmd.strip_prefix(crate::permission::SOCKET_ENV) {
                            // Skip "=<path> " to get the original command
                            rest.split_once(' ')
                                .map(|(_, c)| c)
                                .unwrap_or(rest)
                                .trim_start_matches('=')
                        } else {
                            cmd
                        };
                    hook["command"] = serde_json::json!(format!(
                        "{}={} {}",
                        crate::permission::SOCKET_ENV,
                        sock_path,
                        clean_cmd
                    ));
                }
            }
        }
    }
}

/// Re-embed the current socket path into all active worktrees' settings.
/// Called at dashboard startup so hooks from pre-existing tasks connect
/// to the new socket.
pub fn reembed_socket_in_worktrees(work_dirs: &[String], sock_path: &str) {
    for wd in work_dirs {
        let settings_path = Path::new(wd).join(".claude/settings.local.json");
        let Ok(content) = std::fs::read_to_string(&settings_path) else {
            continue;
        };
        let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        embed_socket_in_hooks(&mut settings, sock_path);
        let _ = std::fs::write(&settings_path, settings.to_string());
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
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

/// Returns the set of pane IDs that appear idle by inspecting the Claude Code UI.
/// A pane is idle when its last non-empty line does NOT contain "esc" (case-insensitive),
/// since Claude Code shows "esc to interrupt" / "Esc to cancel" while actively working.
pub fn idle_panes(pane_ids: &[&PaneId]) -> HashSet<PaneId> {
    let mut set = HashSet::new();
    for pane_id in pane_ids {
        if let Ok(output) = tmux_cmd(&["capture-pane", "-p", "-t", pane_id.as_str()]) {
            let last_line = output
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            if !last_line.to_ascii_lowercase().contains("esc") {
                set.insert((*pane_id).clone());
            }
        }
    }
    set
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
