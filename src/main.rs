mod cli;
mod config;
mod permission;
mod runtime;
mod service;
mod skill;
mod store;
mod task;
mod tui;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tabled::{Table, Tabled};

use crate::cli::{Cli, Command, PermissionAction};
use crate::config::Paths;
use crate::runtime::TmuxRuntime;
use crate::service::TaskService;
use crate::store::Store;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve()?;
    paths.ensure_dirs()?;
    let store = Store::open(&paths.db_path)?;
    let runtime = TmuxRuntime;
    let service = TaskService::new(&store, &runtime, &paths);

    match cli.command {
        Command::Spawn { name, skill, param } => cmd_spawn(&service, &name, &skill, param)?,
        Command::List { all } => cmd_list(&service, all)?,
        Command::History => cmd_list(&service, true)?,
        Command::Log { id } => cmd_log(&service, &id)?,
        Command::Close { id } => cmd_close(&service, &id)?,
        Command::Dash { resume } => tui::run(&service, resume.as_deref())?,
        Command::Start { resume } => cmd_start(resume.as_deref())?,
        Command::Goto { id } => cmd_goto(&service, &id)?,
        Command::Send { id, message } => cmd_send(&service, &id, &message)?,
        Command::Permission { action } => cmd_permission(action)?,
        Command::Complete {
            id,
            exit_code,
            output_file,
        } => cmd_complete(&service, &id, exit_code, output_file.as_deref())?,
    }

    Ok(())
}

fn cmd_spawn(
    service: &TaskService,
    task_name: &str,
    skill_name: &str,
    params: Vec<(String, String)>,
) -> Result<()> {
    let result = service.spawn(task_name, skill_name, params)?;
    println!("Spawned task {} ({})", result.task_name, result.short_id);
    println!("  skill:  {}", result.skill_name);
    println!("  window: {}", result.window_id);
    Ok(())
}

fn cmd_list(service: &TaskService, all: bool) -> Result<()> {
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
        #[tabled(rename = "ID")]
        id: String,
        #[tabled(rename = "Name")]
        name: String,
        #[tabled(rename = "Skill")]
        skill: String,
        #[tabled(rename = "Status")]
        status: String,
        #[tabled(rename = "Window")]
        window: String,
        #[tabled(rename = "Started")]
        started: String,
        #[tabled(rename = "Exit")]
        exit_code: String,
    }

    let rows: Vec<Row> = tasks
        .iter()
        .map(|t| Row {
            id: t.id[..8].to_string(),
            name: t.name.clone(),
            skill: t.skill_name.clone(),
            status: t.status.clone(),
            window: t.tmux_window.clone().unwrap_or_default(),
            started: t.started_at.format("%H:%M:%S").to_string(),
            exit_code: t
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string()),
        })
        .collect();

    println!("{}", Table::new(rows));
    Ok(())
}

fn cmd_close(service: &TaskService, id: &str) -> Result<()> {
    let result = service.close(id)?;
    println!("Closed task {} ({})", result.task_name, result.short_id);
    Ok(())
}

fn cmd_log(service: &TaskService, id_prefix: &str) -> Result<()> {
    let log = service.log(id_prefix)?;

    if log.messages.is_empty() {
        println!(
            "No messages for task {} ({}).",
            log.task.name,
            &log.task.id[..8]
        );
        return Ok(());
    }

    for msg in &log.messages {
        let label = match msg.role.as_str() {
            "system" => "PROMPT",
            "user" => "YOU",
            _ => &msg.role,
        };
        let time = msg.created_at.format("%H:%M:%S");
        println!("[{time}] {label}:");
        println!("{}", msg.content);
        println!();
    }

    if let Some(output) = &log.live_output {
        let lines: Vec<&str> = output.lines().collect();
        let tail = if lines.len() > 50 {
            &lines[lines.len() - 50..]
        } else {
            &lines
        };
        println!("--- Live pane (last {} lines) ---", tail.len());
        for line in tail {
            println!("{line}");
        }
    }

    Ok(())
}

fn cmd_goto(service: &TaskService, id: &str) -> Result<()> {
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

fn cmd_send(service: &TaskService, id: &str, message: &str) -> Result<()> {
    let result = service.send(id, message)?;
    println!("Sent message to {} ({})", result.task_name, result.short_id);
    Ok(())
}

fn cmd_complete(
    service: &TaskService,
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

fn cmd_permission(action: PermissionAction) -> Result<()> {
    let dir = permission::permissions_dir();

    match action {
        PermissionAction::List => {
            let requests = permission::list_permission_requests(&dir);
            if requests.is_empty() {
                println!("No pending permission requests.");
                return Ok(());
            }

            #[derive(Tabled)]
            struct Row {
                #[tabled(rename = "ID")]
                id: String,
                #[tabled(rename = "Tool")]
                tool: String,
                #[tabled(rename = "Input")]
                input: String,
                #[tabled(rename = "CWD")]
                cwd: String,
            }

            let rows: Vec<Row> = requests
                .iter()
                .map(|r| Row {
                    id: r.req_id.clone(),
                    tool: r.tool_name.clone(),
                    input: r.tool_input_summary.clone(),
                    cwd: r.cwd.clone(),
                })
                .collect();

            println!("{}", Table::new(rows));
        }
        PermissionAction::Approve { req_id } => {
            cmd_permission_respond(&dir, &req_id, true)?;
        }
        PermissionAction::Deny { req_id } => {
            cmd_permission_respond(&dir, &req_id, false)?;
        }
    }

    Ok(())
}

fn cmd_permission_respond(dir: &std::path::Path, req_id_prefix: &str, allow: bool) -> Result<()> {
    let requests = permission::list_permission_requests(dir);

    let matched: Vec<_> = requests
        .iter()
        .filter(|r| r.req_id.starts_with(req_id_prefix))
        .collect();

    match matched.len() {
        0 => bail!("no pending permission request matching '{req_id_prefix}'"),
        1 => {
            let req = &matched[0];
            permission::write_permission_response(dir, &req.req_id, allow);
            let action = if allow { "Approved" } else { "Denied" };
            println!("{action} {} ({})", req.tool_name, req.req_id);
        }
        n => bail!("ambiguous prefix '{req_id_prefix}': matches {n} requests"),
    }

    Ok(())
}
