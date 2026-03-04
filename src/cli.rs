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
    },

    /// List tasks and their status
    List {
        /// Show all tasks including closed/completed/failed
        #[arg(long)]
        all: bool,

        /// Filter tasks by project name
        #[arg(long)]
        project: Option<String>,

        /// Filter tasks by name (case-insensitive substring match)
        #[arg(long)]
        filter: Option<String>,
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
    },

    /// Launch the ExO workspace (tmux + dashboard)
    Start {
        /// Resume an existing Claude session by ID
        #[arg(long)]
        resume: Option<String>,
    },

    /// Send a message to a running agent's tmux pane
    Send {
        /// Task ID (prefix match)
        id: String,

        /// Message to send
        message: String,
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
