mod cli;
mod config;
mod permission;
mod primitives;
mod runtime;
mod service;
mod skill;
mod store;
mod task;
mod tui;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tabled::{Table, Tabled};

use crate::cli::{AgentCommand, Cli, Command, SkillAction};
use crate::config::Paths;
use crate::primitives::MessageRole;
use crate::runtime::{Runtime, TmuxRuntime};
use crate::service::TaskService;
use crate::store::Store;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve()?;
    paths.ensure_dirs()?;
    let store = Store::open(&paths.db_path)?;
    let runtime = TmuxRuntime;
    let service = TaskService::new(&store, &runtime, &paths);

    let command = cli.command.unwrap_or(Command::Dash { resume: None });

    match command {
        Command::Spawn {
            name,
            skill,
            param,
            repo,
            branch,
            no_worktree,
            scratch,
        } => cmd_spawn(
            &service,
            SpawnOpts {
                name,
                skill,
                params: param,
                repo,
                branch,
                no_worktree,
                scratch,
            },
        )?,
        Command::List { all } => cmd_list(&service, all)?,
        Command::History => cmd_list(&service, true)?,
        Command::Log { id } => cmd_log(&service, &id)?,
        Command::Close { id } => cmd_close(&service, &id)?,
        Command::Delete { id } => cmd_delete(&service, &id)?,
        Command::Dash { resume } => tui::run(&service, resume.as_deref())?,
        Command::Start { resume } => cmd_start(resume.as_deref())?,
        Command::Goto { id } => cmd_goto(&service, &id)?,
        Command::Send { id, message } => cmd_send(&service, &id, &message)?,
        Command::Skill { action } => cmd_skill(action, &service)?,
        Command::Agent { action } => match action {
            AgentCommand::PermissionGate => permission::gate_request()?,
            AgentCommand::PermissionPrompt {
                tool,
                input,
                response_file,
            } => permission::prompt_request(&tool, &input, &response_file)?,
            AgentCommand::Complete {
                id,
                exit_code,
                output_file,
            } => cmd_complete(&service, &id, exit_code, output_file.as_deref())?,
        },
    }

    Ok(())
}

struct SpawnOpts {
    name: String,
    skill: String,
    params: Vec<(String, String)>,
    repo: Option<std::path::PathBuf>,
    branch: Option<String>,
    no_worktree: bool,
    scratch: bool,
}

fn cmd_spawn(service: &TaskService<impl Runtime>, opts: SpawnOpts) -> Result<()> {
    let result = if opts.scratch {
        service.spawn_scratch(&opts.name, &opts.skill, opts.params)?
    } else if opts.no_worktree {
        service.spawn_no_worktree(&opts.name, &opts.skill, opts.params, opts.repo.as_deref())?
    } else {
        service.spawn(
            &opts.name,
            &opts.skill,
            opts.params,
            opts.repo.as_deref(),
            opts.branch.as_deref(),
        )?
    };
    println!(
        "Spawned task {} ({})",
        result.task_name,
        result.task_id.short()
    );
    println!("  skill:  {}", result.skill_name);
    println!("  window: {}", result.window_id);
    Ok(())
}

fn cmd_list(service: &TaskService<impl Runtime>, all: bool) -> Result<()> {
    let tasks = if all {
        service.list_all()?
    } else {
        service.list_active()?
    };

    if tasks.is_empty() {
        println!("No tasks.");
        return Ok(());
    }

    #[derive(Tabled)]
    struct Row {
        #[tabled(rename = "#")]
        win_num: String,
        #[tabled(rename = "ID")]
        id: String,
        #[tabled(rename = "Name")]
        name: String,
        #[tabled(rename = "Skill")]
        skill: String,
        #[tabled(rename = "Status")]
        status: String,
        #[tabled(rename = "Pane")]
        pane: String,
        #[tabled(rename = "Window")]
        window: String,
        #[tabled(rename = "Started")]
        started: String,
        #[tabled(rename = "Exit")]
        exit_code: String,
    }

    let win_numbers = crate::runtime::tmux_window_numbers();
    let rows: Vec<Row> = tasks
        .iter()
        .map(|t| Row {
            win_num: t
                .tmux_window
                .as_deref()
                .and_then(|w| win_numbers.get(w))
                .cloned()
                .unwrap_or_else(|| "-".to_string()),
            id: t.id.short().to_string(),
            name: t.name.clone(),
            skill: t.skill_name.clone(),
            status: t.status.to_string(),
            pane: t.tmux_pane.clone().unwrap_or_else(|| "-".to_string()),
            window: t.tmux_window.clone().unwrap_or_default(),
            started: t.started_at.format("%H:%M:%S").to_string(),
            exit_code: t
                .exit_code
                .map(|c: i32| c.to_string())
                .unwrap_or_else(|| "-".to_string()),
        })
        .collect();

    println!("{}", Table::new(rows));
    Ok(())
}

fn cmd_close(service: &TaskService<impl Runtime>, id: &str) -> Result<()> {
    let result = service.close(id)?;
    println!(
        "Closed task {} ({})",
        result.task_name,
        result.task_id.short()
    );
    Ok(())
}

fn cmd_delete(service: &TaskService<impl Runtime>, id: &str) -> Result<()> {
    service.delete(id)?;
    println!("Deleted task {id}");
    Ok(())
}

fn cmd_log(service: &TaskService<impl Runtime>, id_prefix: &str) -> Result<()> {
    let log = service.log(id_prefix)?;

    if log.messages.is_empty() {
        println!(
            "No messages for task {} ({}).",
            log.task.name,
            log.task.id.short()
        );
        return Ok(());
    }

    for msg in &log.messages {
        let label = match msg.role {
            MessageRole::System => "PROMPT",
            MessageRole::User => "YOU",
            MessageRole::Assistant => "ASSISTANT",
        };
        let time = msg.created_at.format("%H:%M:%S");
        println!("[{time}] {label}:");
        println!("{}", msg.content);
        println!();
    }

    if let Some(output) = &log.live_output {
        let all_lines: Vec<&str> = output.lines().collect();
        let tail = if all_lines.len() > 50 {
            &all_lines[all_lines.len() - 50..]
        } else {
            &all_lines
        };
        println!("--- Live pane (last {} lines) ---", tail.len());
        for line in tail {
            println!("{line}");
        }
    }

    Ok(())
}

fn cmd_goto(service: &TaskService<impl Runtime>, id: &str) -> Result<()> {
    service.goto(id)
}

fn cmd_start(resume: Option<&str>) -> Result<()> {
    let exe = std::env::current_exe()
        .context("failed to resolve current executable")?
        .display()
        .to_string();

    let mut dash_cmd = format!("{exe} dash");
    if let Some(sid) = resume {
        dash_cmd.push_str(&format!(" --resume {sid}"));
    }

    if std::env::var("TMUX").is_ok() {
        let top_pane = runtime::tmux_cmd(&["display-message", "-p", "#{pane_id}"])?;
        runtime::tmux_cmd(&["split-window", "-v", "-t", &top_pane])?;
        runtime::tmux_cmd(&["resize-pane", "-t", &top_pane, "-D", "8"])?;
        runtime::tmux_cmd(&["send-keys", "-t", &top_pane, &dash_cmd, "Enter"])?;
    } else {
        runtime::tmux_cmd(&["new-session", "-d", "-s", "exo", "-n", "exo"])?;
        let top_pane = runtime::tmux_cmd(&["list-panes", "-t", "exo:exo", "-F", "#{pane_id}"])?;
        runtime::tmux_cmd(&["send-keys", "-t", &top_pane, &dash_cmd, "Enter"])?;
        runtime::tmux_cmd(&["split-window", "-v", "-t", "exo:exo"])?;
        runtime::tmux_cmd(&["resize-pane", "-t", &top_pane, "-D", "8"])?;

        let status = std::process::Command::new("tmux")
            .args(["attach-session", "-t", "exo"])
            .status()?;

        if !status.success() {
            bail!("tmux attach-session failed");
        }
    }

    Ok(())
}

fn cmd_send(service: &TaskService<impl Runtime>, id: &str, message: &str) -> Result<()> {
    let result = service.send(id, message)?;
    println!(
        "Sent message to {} ({})",
        result.task_name,
        result.task_id.short()
    );
    Ok(())
}

fn cmd_complete(
    service: &TaskService<impl Runtime>,
    id: &str,
    exit_code: i32,
    output_file: Option<&str>,
) -> Result<()> {
    let output = output_file.and_then(|path| std::fs::read_to_string(path).ok());
    service.complete(id, exit_code, output.as_deref())?;

    let status = if exit_code == 0 {
        "completed"
    } else {
        "failed"
    };
    println!("Task {id} marked as {status} (exit code: {exit_code})");

    Ok(())
}

fn cmd_skill(action: SkillAction, service: &TaskService<impl Runtime>) -> Result<()> {
    match action {
        SkillAction::List => {
            let skills = service.list_skills()?;
            if skills.is_empty() {
                println!("No skills found.");
                return Ok(());
            }

            #[derive(Tabled)]
            struct Row {
                #[tabled(rename = "Name")]
                name: String,
                #[tabled(rename = "Description")]
                description: String,
                #[tabled(rename = "Params")]
                params: String,
            }

            let rows: Vec<Row> = skills
                .iter()
                .map(|s| Row {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    params: s.params.join(", "),
                })
                .collect();

            println!("{}", Table::new(rows));
        }
    }
    Ok(())
}
