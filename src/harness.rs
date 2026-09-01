//! Agent harness abstraction.
//!
//! A `Harness` knows how to drive a particular agent CLI (today: Claude Code;
//! later: Goose, etc.) on top of a `Runtime`. Where the [`crate::runtime::Runtime`]
//! trait owns the windowing/worktree concerns (and there's exactly one per
//! workspace), there can be many harnesses live at once — one per task —
//! so harnesses are stored in a `HashMap<HarnessKind, Arc<dyn Harness<R>>>`
//! and accessed via dynamic dispatch.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::runtime::{Runtime, SpawnResult};
use crate::skill::BaseTools;

/// Identifies which agent harness drives a task.
///
/// Phase 1 only registers `Claude`; additional variants will be added later
/// (e.g. `Goose`) without changing the registry shape.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum HarnessKind {
    #[default]
    Claude,
}

/// Inputs needed by [`Harness::launch_agent`] for a fresh task.
pub struct LaunchConfig<'a> {
    pub task_name: &'a str,
    pub session_id: &'a str,
    pub system_prompt: Option<&'a str>,
    pub work_dir: &'a Path,
    /// Some = Full mode (--system-prompt), None = Interactive (--append-system-prompt + idle prompt)
    pub user_prompt: Option<&'a str>,
    /// When true, pass `--dangerously-skip-permissions` to the underlying CLI.
    pub skip_permissions: bool,
    /// Role exported as `CC_SESSION_ROLE` in the launch script (e.g. skill name).
    pub session_role: Option<&'a str>,
}

/// Bundled permission info extracted from a skill's `[agent]` section.
/// Passed to harness setup so the correct tools are auto-approved.
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

/// Encapsulates the agent-CLI–specific behaviour: writing config files
/// (`.claude/`, `.goose/`, …), launching/resuming the CLI inside a runtime
/// pane, and detecting when the agent is idle.
///
/// Methods that need to spawn or interact with a tmux window receive the
/// runtime as `&R` so the runtime stays statically dispatched.
pub trait Harness<R: Runtime>: Send + Sync + 'static {
    /// Write the harness's per-worktree config (hooks, settings, MCP
    /// registration, …). Called after [`Runtime::create_worktree`],
    /// [`Runtime::recreate_worktree`], or [`Runtime::init_scratch_dir`],
    /// and also for `Existing` work-dir mode.
    fn setup_dir_config(
        &self,
        repo_root: &Path,
        work_dir: &Path,
        perms: &SkillPermissions,
        jwt_token: &str,
        task_name: Option<&str>,
    ) -> anyhow::Result<()>;

    /// Spawn a fresh agent for this task.
    fn launch_agent(&self, runtime: &R, config: LaunchConfig) -> anyhow::Result<SpawnResult>;

    /// Resume an existing agent session (called by `clat reopen`).
    fn resume_agent(
        &self,
        runtime: &R,
        task_name: &str,
        session_id: &str,
        work_dir: &Path,
        skip_permissions: bool,
    ) -> anyhow::Result<SpawnResult>;

    /// Re-run the most recent launch (legacy fallback when no session-id
    /// was recorded).
    fn relaunch_agent(
        &self,
        runtime: &R,
        task_name: &str,
        work_dir: &Path,
    ) -> anyhow::Result<SpawnResult>;

    /// Returns whether the captured pane output indicates the agent is idle
    /// (i.e. not actively working on a tool call).
    fn is_pane_idle(&self, pane_output: &str) -> bool;
}

// ---------------------------------------------------------------------------
// Claude harness — drives the `claude` CLI inside a tmux pane
// ---------------------------------------------------------------------------

/// Default harness: drives the Claude Code CLI.
pub struct ClaudeHarness;

impl ClaudeHarness {
    fn resolve_binary(name: &str) -> anyhow::Result<String> {
        let output = Command::new("which")
            .arg(name)
            .output()
            .with_context(|| format!("failed to find {name}"))?;

        if !output.status.success() {
            bail!("{name} not found in PATH");
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

impl<R: Runtime> Harness<R> for ClaudeHarness {
    fn setup_dir_config(
        &self,
        repo_root: &Path,
        work_dir: &Path,
        perms: &SkillPermissions,
        jwt_token: &str,
        task_name: Option<&str>,
    ) -> anyhow::Result<()> {
        setup_worktree_config(repo_root, work_dir, perms, jwt_token, task_name)?;
        merge_repo_settings(repo_root, work_dir)?;
        Ok(())
    }

    fn launch_agent(&self, runtime: &R, config: LaunchConfig) -> anyhow::Result<SpawnResult> {
        let claude_bin = Self::resolve_binary("claude")?;

        let claude_dir = config.work_dir.join(".claude");
        std::fs::create_dir_all(&claude_dir)?;

        let mut script = "#!/bin/sh\nunset CLAUDECODE\n".to_string();
        if let Some(role) = config.session_role {
            script.push_str(&format!("export CC_SESSION_ROLE={role}\n"));
        }
        script.push_str(&format!("exec {claude_bin}"));
        if config.skip_permissions {
            script.push_str(" --dangerously-skip-permissions");
        }
        // Tasks always run on sonnet, regardless of the ambient `claude` CLI
        // default model. ExO and PM roles are pinned to opus separately
        // (see assistant.rs).
        script.push_str(" --model sonnet");
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

        runtime.launch_agent_window(config.task_name, config.work_dir, "sh .claude/launch.sh")
    }

    fn resume_agent(
        &self,
        runtime: &R,
        task_name: &str,
        session_id: &str,
        work_dir: &Path,
        skip_permissions: bool,
    ) -> anyhow::Result<SpawnResult> {
        let claude_bin = Self::resolve_binary("claude")?;
        let skip_flag = if skip_permissions {
            " --dangerously-skip-permissions"
        } else {
            ""
        };
        // No --model here: --resume should continue whatever model the
        // session was last on (the user may have switched it live via
        // /model), not reset it back to the launch-time default.
        let claude_cmd = format!("env -u CLAUDECODE {claude_bin}{skip_flag} --resume {session_id}");

        runtime.launch_agent_window(task_name, work_dir, &claude_cmd)
    }

    fn relaunch_agent(
        &self,
        runtime: &R,
        task_name: &str,
        work_dir: &Path,
    ) -> anyhow::Result<SpawnResult> {
        runtime.launch_agent_window(task_name, work_dir, "sh .claude/launch.sh")
    }

    /// A Claude pane is idle when its last non-empty line does NOT contain
    /// "esc" (case-insensitive); Claude Code shows "esc to interrupt" /
    /// "Esc to cancel" while actively working on a tool call.
    fn is_pane_idle(&self, pane_output: &str) -> bool {
        let last_line = pane_output
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        !last_line.to_ascii_lowercase().contains("esc")
    }
}

// ---------------------------------------------------------------------------
// Claude config helpers — write `.claude/`, `.mcp.json`, etc.
// ---------------------------------------------------------------------------

/// Copy hooks config and write settings into a worktree's `.claude/` directory.
/// This is shared between initial creation and worktree recreation (reopen).
fn setup_worktree_config(
    repo_root: &Path,
    worktree_path: &Path,
    perms: &SkillPermissions,
    jwt_token: &str,
    task_name: Option<&str>,
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
            embed_env_in_hooks(&mut settings, &sock_path, task_name);
            // Write perm-socket breadcrumb into the worktree so that
            // send_pm_message / send_exo_message can discover the socket.
            let _ = std::fs::write(target_claude_dir.join("perm-socket"), &sock_path);
        }

        // Write .mcp.json at worktree root so Claude Code discovers the MCP server.
        // Claude Code reads MCP servers from .mcp.json (project scope), NOT from
        // the mcpServers key in settings.local.json.
        if let Some(mcp_url) = crate::mcp::read_mcp_url_breadcrumb(repo_root) {
            // Read existing .mcp.json (target repo may have committed servers) or start fresh.
            let mcp_path = worktree_path.join(".mcp.json");
            let mut mcp_config: serde_json::Value = if mcp_path.exists() {
                let content = std::fs::read_to_string(&mcp_path)?;
                serde_json::from_str(&content)
                    .unwrap_or_else(|_| serde_json::json!({"mcpServers": {}}))
            } else {
                serde_json::json!({"mcpServers": {}})
            };

            // Ensure mcpServers key exists as an object.
            if !mcp_config.get("mcpServers").is_some_and(|v| v.is_object()) {
                mcp_config["mcpServers"] = serde_json::json!({});
            }
            let servers = mcp_config["mcpServers"].as_object_mut().unwrap();

            // Always add clat MCP server.
            // Include the JWT in both the URL query param and Authorization header
            // for resilience against Claude Code header bugs.
            servers.insert(
                "clat".to_string(),
                serde_json::json!({
                    "type": "http",
                    "url": format!("{mcp_url}?token={jwt_token}"),
                    "headers": {
                        "Authorization": format!("Bearer {jwt_token}")
                    }
                }),
            );

            std::fs::write(&mcp_path, serde_json::to_string_pretty(&mcp_config)?)?;

            // Register MCP servers as local-scoped so Claude Code trusts them
            // immediately — no "N new MCP servers found" approval prompt.
            let clat_url = format!("{mcp_url}?token={jwt_token}");
            let auth_value = format!("Bearer {jwt_token}");
            register_local_mcp_server(
                worktree_path,
                "clat",
                &clat_url,
                &[("Authorization", &auth_value)],
            );
            settings["enableAllProjectMcpServers"] = serde_json::json!(true);

            // Auto-allow MCP tools so agents don't need manual approval.
            if let Some(perms_allow) = settings
                .get_mut("permissions")
                .and_then(|p| p.get_mut("allow"))
                .and_then(|a| a.as_array_mut())
            {
                perms_allow.push(serde_json::json!("mcp__clat__clat_spawn"));
                perms_allow.push(serde_json::json!("mcp__clat__create_watch"));
                perms_allow.push(serde_json::json!("mcp__clat__send_message"));
                perms_allow.push(serde_json::json!("mcp__clat__list_tasks"));
                perms_allow.push(serde_json::json!("mcp__clat__task_log"));
                perms_allow.push(serde_json::json!("mcp__clat__store_memory"));
                perms_allow.push(serde_json::json!("mcp__clat__search_memory"));
                perms_allow.push(serde_json::json!("mcp__clat__list_memories"));
                perms_allow.push(serde_json::json!("mcp__galoy-agents__search_tools"));
                perms_allow.push(serde_json::json!("mcp__galoy-agents__describe_tool"));
                perms_allow.push(serde_json::json!("mcp__galoy-agents__call_tool"));
                perms_allow.push(serde_json::json!("mcp__galoy-agents__hello"));
                perms_allow.push(serde_json::json!("mcp__galoy-agents__search_code"));
            }

            // Use .git/info/exclude instead of .gitignore — never committed.
            exclude_from_git(worktree_path, ".mcp.json")?;
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
        "Bash(git reset:*)",
        "Bash(git checkout:*)",
        "Bash(git worktree:*)",
        "Bash(git cherry-pick:*)",
        "Bash(git rebase:*)",
        "Bash(git fetch:*)",
        "Bash(git -C:*)",
        "Bash(git pull:*)",
        "Bash(git stash:*)",
        "Bash(git rev-parse:*)",
        "Bash(git ls-files:*)",
        "Bash(git remote:*)",
        "Bash(git merge:*)",
        // Nix (blanket — covers flake check, develop, build, run, eval, etc.)
        "Bash(nix:*)",
        // Cargo (typically run inside nix develop, but allow direct too)
        "Bash(cargo fmt:*)",
        "Bash(cargo clippy:*)",
        "Bash(cargo nextest:*)",
        "Bash(cargo build:*)",
        "Bash(cargo test:*)",
        "Bash(cargo check:*)",
        // Basic shell commands
        "Bash(ls:*)",
        "Bash(cat:*)",
        "Bash(head:*)",
        "Bash(tail:*)",
        "Bash(wc:*)",
        "Bash(which:*)",
        "Bash(pwd)",
        "Bash(find:*)",
        "Bash(grep:*)",
        "Bash(rg:*)",
        "Bash(tree:*)",
        "Bash(mkdir:*)",
        "Bash(echo:*)",
        "Bash(sort:*)",
        "Bash(uniq:*)",
        "Bash(jq:*)",
        // Local HTTP (curl restricted to localhost)
        "Bash(curl localhost:*)",
        "Bash(curl 127.0.0.1:*)",
        // GitHub CLI
        "Bash(gh:*)",
        // Containers
        "Bash(podman:*)",
        "Bash(docker:*)",
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

/// Rewrite hook commands in settings JSON to prefix environment variables
/// (`CC_PERM_SOCKET=<path>` and optionally `CC_TASK_NAME=<name>`), so spawned
/// agents' hooks connect to the correct dashboard socket and can be identified
/// even when their CWD is outside the worktree.
pub(crate) fn embed_env_in_hooks(
    settings: &mut serde_json::Value,
    sock_path: &str,
    task_name: Option<&str>,
) {
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
                    // Strip existing env var prefixes to avoid stacking
                    let clean_cmd = strip_env_prefixes(cmd);
                    let mut prefix = format!("{}={}", crate::permission::SOCKET_ENV, sock_path,);
                    if let Some(name) = task_name {
                        prefix
                            .push_str(&format!(" {}={}", crate::permission::TASK_NAME_ENV, name,));
                    }
                    hook["command"] = serde_json::json!(format!("{prefix} {clean_cmd}"));
                }
            }
        }
    }
}

/// Strip leading `CC_PERM_SOCKET=... ` and `CC_TASK_NAME=... ` prefixes from
/// a hook command string, returning the original command.
fn strip_env_prefixes(cmd: &str) -> &str {
    let mut rest = cmd;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.starts_with(crate::permission::SOCKET_ENV)
            || trimmed.starts_with(crate::permission::TASK_NAME_ENV)
        {
            // Skip "VAR=value " — find the next space after the value
            if let Some((_prefix, after)) = trimmed.split_once(' ') {
                rest = after;
                continue;
            }
        }
        break;
    }
    rest
}

/// Re-embed the current socket path and task names into all active worktrees' settings.
/// Called at dashboard startup so hooks from pre-existing tasks connect
/// to the new socket and carry the correct task identity.
///
/// Each entry is `(task_name, work_dir)`.
pub fn reembed_env_in_worktrees(tasks: &[(String, String)], sock_path: &str) {
    for (name, wd) in tasks {
        let settings_path = std::path::Path::new(wd).join(".claude/settings.local.json");
        let Ok(content) = std::fs::read_to_string(&settings_path) else {
            continue;
        };
        let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        embed_env_in_hooks(&mut settings, sock_path, Some(name));
        let _ = std::fs::write(&settings_path, settings.to_string());
        // Also refresh the perm-socket breadcrumb so send_pm_message /
        // send_exo_message can discover the (possibly new) socket path.
        let _ = std::fs::write(
            std::path::Path::new(wd).join(".claude/perm-socket"),
            sock_path,
        );
    }
}

/// Register an MCP server as local-scoped via `claude mcp add`.
///
/// Local-scoped servers are trusted by Claude Code and don't trigger the
/// "N new MCP servers found" approval prompt that project-scoped (.mcp.json)
/// servers do. We keep .mcp.json as a fallback but use this to pre-register
/// so agents can start working immediately.
fn register_local_mcp_server(work_dir: &Path, name: &str, url: &str, headers: &[(&str, &str)]) {
    let mut args = vec![
        "mcp".to_string(),
        "add".to_string(),
        "--transport".to_string(),
        "http".to_string(),
        "--scope".to_string(),
        "local".to_string(),
        name.to_string(),
        url.to_string(),
    ];
    // --header is variadic (<header...>), so it must come after the
    // positional <name> and <url> arguments to avoid swallowing them.
    for (key, value) in headers {
        args.push("--header".to_string());
        args.push(format!("{key}: {value}"));
    }

    let result = Command::new("claude")
        .args(&args)
        .current_dir(work_dir)
        .output();

    match result {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("claude mcp add {name} failed: {stderr}");
        }
        Err(e) => {
            tracing::warn!("could not run claude mcp add {name}: {e}");
        }
    }
}

/// Add an entry to `.git/info/exclude` so it's ignored without touching `.gitignore`.
fn exclude_from_git(worktree_path: &Path, pattern: &str) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(worktree_path)
        .output()
        .context("failed to run git rev-parse --git-common-dir")?;
    if !output.status.success() {
        bail!(
            "git rev-parse --git-common-dir failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let git_common_dir = String::from_utf8(output.stdout)?.trim().to_string();
    let exclude_path = PathBuf::from(&git_common_dir).join("info").join("exclude");

    std::fs::create_dir_all(exclude_path.parent().unwrap())?;

    let content = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if !content.lines().any(|l| l.trim() == pattern) {
        let mut new_content = content;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(pattern);
        new_content.push('\n');
        std::fs::write(&exclude_path, new_content)?;
    }
    Ok(())
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
