use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "clat", about = "clat: command line agent tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Spawn a worker agent for a task
    Spawn {
        /// Human-friendly task name
        name: String,

        /// Skill to use (default: noop)
        #[arg(short, long, default_value = "noop")]
        skill: String,

        /// Parameters as key=value pairs
        #[arg(short, long, value_parser = parse_param)]
        param: Vec<(String, String)>,
    },

    /// List tasks and their status
    List,

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

    /// Mark a task as completed (called by wrapper script)
    #[command(hide = true)]
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
