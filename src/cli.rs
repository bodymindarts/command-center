use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "clat", about = "clat: command line agent tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Spawn a worker agent for a task
    Spawn {
        /// Human-friendly task name
        name: String,

        /// Skill to use (default: engineer)
        #[arg(short, long, default_value = "engineer")]
        skill: String,

        /// Parameters as key=value pairs
        #[arg(short, long, value_parser = parse_param)]
        param: Vec<(String, String)>,

        /// Path to target git repository (default: command-center repo)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Existing branch to check out in the worktree (instead of creating a new one)
        #[arg(long)]
        branch: Option<String>,

        /// Skip worktree creation; use repo root as working directory (interactive mode)
        #[arg(long, conflicts_with = "scratch")]
        no_worktree: bool,

        /// Create a scratch directory under data/scratch/ (interactive mode)
        #[arg(long, conflicts_with_all = ["no_worktree", "repo"])]
        scratch: bool,

        /// Assign task to a project
        #[arg(long)]
        project: Option<String>,

        /// Bypass all permission checks for the spawned Claude Code session
        #[arg(long)]
        dangerously_skip_permissions: bool,
    },

    /// List tasks and their status
    List {
        /// Show all tasks including closed/completed/failed
        #[arg(long)]
        all: bool,

        /// Filter tasks by project name
        #[arg(long)]
        project: Option<String>,
    },

    /// List all tasks (alias for list --all)
    History,

    /// Show message log for a task
    Log {
        /// Task ID (prefix match)
        id: String,
    },

    /// Close a running task (capture output, kill tmux window)
    Close {
        /// Task ID (prefix match)
        id: String,
    },

    /// Reopen a closed/completed task (resume agent in tmux)
    Reopen {
        /// Task ID (prefix match)
        id: String,
    },

    /// Move a task to a different project
    Move {
        /// Task ID (prefix match)
        id: String,

        /// Project name to move the task to
        project: String,
    },

    /// Permanently delete a task from the database
    Delete {
        /// Task ID (prefix match)
        id: String,
    },

    /// Switch to a task's tmux window
    Goto {
        /// Task ID (prefix match)
        id: String,
    },

    /// Open the interactive TUI dashboard
    Dash {
        /// Resume an existing Claude session by ID
        #[arg(long)]
        resume: Option<String>,

        /// Prevent system sleep while the dashboard is running (macOS only)
        #[arg(long)]
        caffeinate: bool,

        /// Bypass all permission checks for spawned Claude Code sessions
        #[arg(long)]
        dangerously_skip_permissions: bool,
    },

    /// Launch the ExO workspace (tmux + dashboard)
    Start {
        /// Resume an existing Claude session by ID
        #[arg(long)]
        resume: Option<String>,

        /// Prevent system sleep while the dashboard is running (macOS only)
        #[arg(long)]
        caffeinate: bool,

        /// Bypass all permission checks for spawned Claude Code sessions
        #[arg(long)]
        dangerously_skip_permissions: bool,
    },

    /// Send a message to a running agent's tmux pane or a project PM
    Send {
        /// Send to a project's PM session instead of a task
        #[arg(long)]
        project: Option<String>,

        /// Label the message with a sender name (shown in chat as "[from <name>]")
        #[arg(long)]
        from: Option<String>,

        /// [ID] <message> — task ID is required unless --project is provided
        #[arg(required = true, num_args = 1..=2)]
        args: Vec<String>,
    },

    /// Manage available skills
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },

    /// Manage projects
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },

    /// Manage shared agent memory
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },

    /// Commands called by spawned agents (hooks, lifecycle)
    #[command(hide = true)]
    Agent {
        #[command(subcommand)]
        action: AgentCommand,
    },
}

#[derive(Subcommand)]
pub enum SkillAction {
    /// List available skills
    List,
}

#[derive(Subcommand)]
pub enum ProjectAction {
    /// Create a new project
    Create {
        /// Project name (unique identifier)
        name: String,

        /// Optional description
        #[arg(short, long, default_value = "")]
        description: String,
    },

    /// List all projects
    List,

    /// Delete a project
    Delete {
        /// Project name
        name: String,
    },

    /// Send a message to a project's PM session
    Send {
        /// Project name
        name: String,

        /// Message to send
        message: String,
    },

    /// Show PM conversation log for a project
    Log {
        /// Project name
        name: String,

        /// Show only the N most recent messages
        #[arg(long)]
        last: Option<u32>,
    },
}

#[derive(Subcommand)]
pub enum MemoryAction {
    /// Store a new memory
    Store {
        /// Memory title
        #[arg(long)]
        title: String,

        /// Memory content
        #[arg(long)]
        content: String,

        /// Tags (can be repeated)
        #[arg(long)]
        tag: Vec<String>,

        /// Project to associate with
        #[arg(long)]
        project: Option<String>,
    },

    /// Search memories (hybrid keyword + vector)
    Search {
        /// Search query
        query: String,

        /// Filter by project
        #[arg(long)]
        project: Option<String>,

        /// Maximum results to return
        #[arg(long, default_value = "10")]
        limit: usize,
    },

    /// List memories
    List {
        /// Filter by project
        #[arg(long)]
        project: Option<String>,

        /// Filter by tag
        #[arg(long)]
        tag: Vec<String>,

        /// Maximum results to return
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Get a memory by ID or prefix
    Get {
        /// Memory ID (prefix match)
        id: String,
    },

    /// Delete a memory by ID or prefix
    Delete {
        /// Memory ID (prefix match)
        id: String,
    },

    /// Rebuild index from markdown files on disk
    Reindex,
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// Gate a permission request (stdin→socket/popup→stdout)
    PermissionGate,

    /// Interactive y/n prompt inside tmux popup
    PermissionPrompt {
        #[arg(long)]
        tool: String,
        #[arg(long)]
        input: String,
        #[arg(long)]
        response_file: String,
    },

    /// Mark a task as completed (called by wrapper script)
    Complete {
        /// Task ID
        id: String,

        /// Exit code from the agent process
        exit_code: i32,

        /// Path to output file
        output_file: Option<String>,
    },
}

fn parse_param(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid param format '{s}', expected key=value"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}
