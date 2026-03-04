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

use crate::cli::{AgentCommand, Cli, Command, ProjectAction, SkillAction};
use crate::config::Paths;
use crate::primitives::MessageRole;
use crate::runtime::{Runtime, TmuxRuntime};
use crate::service::{PromptMode, SpawnRequest, TaskService, WorkDirMode};
use crate::store::Store;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve()?;
    paths.ensure_dirs()?;
    let store = Store::open(&paths.db_path)?;
    let runtime = TmuxRuntime;
    let service = TaskService::new(&store, &runtime, &paths);

    let command = cli.command.unwrap_or(Command::Dash {
        resume: None,
        caffeinate: false,
    });

    match command {
        Command::Spawn {
            name,
            skill,
            param,
            repo,
            branch,
            no_worktree,
            scratch,
            project,
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
                project,
            },
        )?,
        Command::List {
            all,
            project,
            filter,
        } => cmd_list(&service, all, project, filter)?,
        Command::History => cmd_list(&service, true, None, None)?,
        Command::Log { id } => cmd_log(&service, &id)?,
        Command::Close { id } => cmd_close(&service, &id)?,
        Command::Reopen { id } => cmd_reopen(&service, &id)?,
        Command::Delete { id } => cmd_delete(&service, &id)?,
        Command::Dash { resume, caffeinate } => tui::run(&service, resume.as_deref(), caffeinate)?,
        Command::Start { resume, caffeinate } => cmd_start(resume.as_deref(), caffeinate)?,
        Command::Goto { id } => cmd_goto(&service, &id)?,
        Command::Send { id, message } => cmd_send(&service, &id, &message)?,
        Command::StreamHooks { task } => cmd_stream_hooks(service.project_root(), task.as_deref())?,
        Command::Skill { action } => cmd_skill(action, &service)?,
        Command::Project { action } => cmd_project(action, &service)?,
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
    project: Option<String>,
}

fn cmd_spawn(service: &TaskService<impl Runtime>, opts: SpawnOpts) -> Result<()> {
    use crate::primitives::ProjectId;
    let project_id: Option<ProjectId> = opts
        .project
        .as_deref()
        .map(|name| service.resolve_project_id(name))
        .transpose()?;

    let repo_path = opts.repo.as_deref();
    let default_repo = service.project_root().to_path_buf();

    let (work_dir_mode, prompt_mode) = if opts.scratch {
        (WorkDirMode::Scratch, PromptMode::Full)
    } else if opts.no_worktree {
        let dir = repo_path.unwrap_or(&default_repo);
        (WorkDirMode::Existing { dir }, PromptMode::Interactive)
    } else {
        let repo = repo_path.unwrap_or(&default_repo);
        (
            WorkDirMode::Worktree {
                repo,
                branch: opts.branch.as_deref(),
            },
            PromptMode::Full,
        )
    };

    let result = service.spawn(SpawnRequest {
        task_name: &opts.name,
        skill_name: &opts.skill,
        params: opts.params,
        work_dir_mode,
        prompt_mode,
        project_id,
    })?;
    println!(
        "Spawned task {} ({})",
        result.task_name,
        result.task_id.short()
    );
    println!("  skill:  {}", result.skill_name);
    println!("  window: {}", result.window_id);
    Ok(())
}

fn cmd_list(
    service: &TaskService<impl Runtime>,
    all: bool,
    project: Option<String>,
    filter: Option<String>,
) -> Result<()> {
    let mut tasks = if all {
        service.list_all()?
    } else if let Some(ref name) = project {
        let pid = service.resolve_project_id(name)?;
        service.list_visible(Some(&pid))?
    } else {
        service.list_active()?
    };

    if let Some(ref pattern) = filter {
        let pattern_lower = pattern.to_lowercase();
        tasks.retain(|t| t.name.as_str().to_lowercase().contains(&pattern_lower));
    }

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
        #[tabled(rename = "Activity")]
        activity: String,
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
    let running_pane_ids: Vec<crate::primitives::PaneId> = tasks
        .iter()
        .filter(|t| t.status.is_running())
        .filter_map(|t| t.tmux_pane.clone())
        .collect();
    let pane_refs: Vec<&crate::primitives::PaneId> = running_pane_ids.iter().collect();
    let idle = crate::runtime::idle_panes(&pane_refs);
    let rows: Vec<Row> = tasks
        .iter()
        .map(|t| {
            let activity = if !t.status.is_running() {
                "-".to_string()
            } else if let Some(ref pane) = t.tmux_pane {
                if idle.contains(pane) {
                    "idle".to_string()
                } else {
                    "active".to_string()
                }
            } else {
                "-".to_string()
            };
            Row {
                win_num: t
                    .tmux_window
                    .as_ref()
                    .and_then(|w| win_numbers.get(w))
                    .cloned()
                    .unwrap_or_else(|| "-".to_string()),
                id: t.id.short().to_string(),
                name: t.name.as_str().to_string(),
                skill: t.skill_name.clone(),
                status: t.status.to_string(),
                activity,
                pane: t
                    .tmux_pane
                    .as_ref()
                    .map(|p| p.as_str().to_string())
                    .unwrap_or_else(|| "-".to_string()),
                window: t
                    .tmux_window
                    .as_ref()
                    .map(|w| w.to_string())
                    .unwrap_or_default(),
                started: t.started_at.format("%H:%M:%S").to_string(),
                exit_code: t
                    .exit_code
                    .map(|c: i32| c.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            }
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

fn cmd_reopen(service: &TaskService<impl Runtime>, id: &str) -> Result<()> {
    let window_id = service.reopen(id)?;
    println!("Reopened task {id} (window: {window_id})");
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

fn cmd_start(resume: Option<&str>, caffeinate: bool) -> Result<()> {
    let exe = std::env::current_exe()
        .context("failed to resolve current executable")?
        .display()
        .to_string();

    let mut dash_cmd = format!("{exe} dash");
    if let Some(sid) = resume {
        dash_cmd.push_str(&format!(" --resume {sid}"));
    }
    if caffeinate {
        dash_cmd.push_str(" --caffeinate");
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

fn cmd_stream_hooks(project_root: &std::path::Path, task_filter: Option<&str>) -> Result<()> {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;

    // Find the dashboard socket via breadcrumb or well-known path.
    let sock_path = crate::permission::read_socket_breadcrumb(project_root)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::permission::session_socket_path);

    let mut stream = UnixStream::connect(&sock_path).with_context(|| {
        format!(
            "failed to connect to dashboard socket at {}",
            sock_path.display()
        )
    })?;

    // Register as observer.
    stream
        .write_all(b"{\"_observe\": true}")
        .context("failed to send observer registration")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to shutdown write half")?;

    eprintln!(
        "Connected to dashboard socket{}",
        task_filter
            .map(|t| format!(" (filter: {t})"))
            .unwrap_or_default()
    );
    eprintln!("Press Ctrl+C to stop.\n");

    let reader = std::io::BufReader::new(stream);
    for line_result in reader.lines() {
        let line = line_result.context("error reading from dashboard socket")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let task = entry["task"].as_str().unwrap_or("?");
            if let Some(filter) = task_filter
                && !task.to_lowercase().contains(&filter.to_lowercase())
            {
                continue;
            }
            let ts = entry["ts"].as_str().unwrap_or("");
            let msg_type = entry["type"].as_str().unwrap_or("");
            let cwd = entry["cwd"].as_str().unwrap_or("");
            print_hook_line(ts, msg_type, task, cwd);
        } else {
            println!("{trimmed}");
        }
    }

    eprintln!("Dashboard disconnected.");
    Ok(())
}

fn print_hook_line(ts: &str, msg_type: &str, task: &str, cwd: &str) {
    // ANSI colors: gray=90, green=32, blue=34, yellow=33, red=31
    let (color_code, label) = match msg_type {
        "_idle" => ("90", "idle"),
        "_active" => ("32", "active"),
        "_resolved" => ("34", "resolved"),
        "permission" => ("33", "permission"),
        other => ("0", other),
    };
    println!("\x1b[2m{ts}\x1b[0m \x1b[{color_code}m{label:<10}\x1b[0m \x1b[1m{task}\x1b[0m  {cwd}");
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

fn cmd_project(action: ProjectAction, service: &TaskService<impl Runtime>) -> Result<()> {
    match action {
        ProjectAction::Create { name, description } => {
            let project = service.create_project(&name, &description)?;
            println!(
                "Created project '{}' ({})",
                project.name,
                project.id.short()
            );
        }
        ProjectAction::List => {
            let projects = service.list_projects()?;
            if projects.is_empty() {
                println!("No projects.");
                return Ok(());
            }

            #[derive(Tabled)]
            struct Row {
                #[tabled(rename = "ID")]
                id: String,
                #[tabled(rename = "Name")]
                name: String,
                #[tabled(rename = "Description")]
                description: String,
                #[tabled(rename = "Created")]
                created: String,
            }

            let rows: Vec<Row> = projects
                .iter()
                .map(|p| Row {
                    id: p.id.short().to_string(),
                    name: p.name.as_str().to_string(),
                    description: p.description.clone(),
                    created: p.created_at.format("%Y-%m-%d %H:%M").to_string(),
                })
                .collect();

            println!("{}", Table::new(rows));
        }
        ProjectAction::Delete { name } => {
            service.delete_project(&name)?;
            println!("Deleted project '{name}'");
        }
    }
    Ok(())
}
