mod app;
mod assistant;
mod cli;
mod config;
mod jwt;
mod mcp;
mod permission;
mod primitives;
mod project;
mod runtime;
mod skill;
mod store;
mod task;
mod tui;
mod watch;

use anyhow::{Context, bail};
use clap::Parser;
use tabled::{Table, Tabled};

use std::sync::Arc;

use crate::app::{ClatApp, PromptMode, SpawnRequest, WorkDirMode};
use crate::cli::{AgentCommand, Cli, Command, ProjectAction, SkillAction};
use crate::primitives::MessageRole;
use crate::runtime::{Runtime, TmuxRuntime};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let command = cli.command.unwrap_or(Command::Dash {
        resume: None,
        caffeinate: false,
        dangerously_skip_permissions: false,
    });

    let skip_permissions = match &command {
        Command::Dash {
            dangerously_skip_permissions,
            ..
        }
        | Command::Start {
            dangerously_skip_permissions,
            ..
        }
        | Command::Spawn {
            dangerously_skip_permissions,
            ..
        } => *dangerously_skip_permissions,
        _ => false,
    };
    let app = ClatApp::try_new(TmuxRuntime, skip_permissions).await?;

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
            ..
        } => {
            cmd_spawn(
                app,
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
            )
            .await?
        }
        Command::List { all, project } => cmd_list(app, all, project).await?,
        Command::History => cmd_list(app, true, None).await?,
        Command::Log { id } => cmd_log(app, &id).await?,
        Command::Close { id } => cmd_close(app, &id).await?,
        Command::Reopen { id } => cmd_reopen(app, &id).await?,
        Command::Move { id, project } => cmd_move(app, &id, &project).await?,
        Command::Delete { id } => cmd_delete(app, &id).await?,
        Command::Dash {
            resume, caffeinate, ..
        } => cmd_dash(app, resume.as_deref(), caffeinate).await?,
        Command::Start {
            resume, caffeinate, ..
        } => cmd_start(resume.as_deref(), caffeinate, skip_permissions)?,
        Command::Goto { id } => cmd_goto(app, &id).await?,
        Command::Send { id, message } => cmd_send(app, &id, &message).await?,
        Command::Skill { action } => cmd_skill(action, app)?,
        Command::Project { action } => cmd_project(action, app).await?,
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
            } => cmd_complete(app, &id, exit_code, output_file.as_deref()).await?,
        },
    }

    Ok(())
}

async fn cmd_dash<R: Runtime + Send + Sync + 'static>(
    app: ClatApp<R>,
    resume: Option<&str>,
    caffeinate: bool,
) -> anyhow::Result<()> {
    let app = Arc::new(app);
    let project_root = app.project_root().to_path_buf();

    // Initialize WatchService (job scheduler for background timers).
    let watch_service = Arc::new(
        watch::WatchService::init(app.store().pool(), Arc::clone(&app))
            .await
            .context("failed to initialize watch service")?,
    );

    // Start MCP server on localhost.
    const MCP_PORT: u16 = 9111;
    match mcp::start_mcp_server(Arc::clone(&app), Arc::clone(&watch_service), MCP_PORT).await {
        Ok(url) => {
            mcp::write_mcp_url_breadcrumb(&project_root, &url);
        }
        Err(e) => {
            eprintln!("warning: failed to start MCP server: {e}");
        }
    }

    let result = tui::run(app, resume, caffeinate).await;
    mcp::remove_mcp_url_breadcrumb(&project_root);
    result
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

async fn cmd_spawn(app: ClatApp<impl Runtime>, opts: SpawnOpts) -> anyhow::Result<()> {
    let (work_dir_mode, prompt_mode) = if opts.scratch {
        (WorkDirMode::Scratch, PromptMode::Full)
    } else if opts.no_worktree {
        match opts.repo.as_deref() {
            Some(dir) => (WorkDirMode::Existing { dir }, PromptMode::Interactive),
            None => (WorkDirMode::Scratch, PromptMode::Interactive),
        }
    } else {
        let repo = opts
            .repo
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--repo is required for worktree tasks"))?;
        (
            WorkDirMode::Worktree {
                repo,
                branch: opts.branch.as_deref(),
            },
            PromptMode::Full,
        )
    };

    // Inherit project from parent task's breadcrumb if not explicitly set.
    let project = opts.project.or_else(|| {
        std::fs::read_to_string(".claude/project")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    });

    let result = app
        .spawn(SpawnRequest {
            task_name: &opts.name,
            skill_name: &opts.skill,
            params: opts.params,
            work_dir_mode,
            prompt_mode,
            project,
        })
        .await?;
    println!(
        "Spawned task {} ({})",
        result.task_name,
        result.task_id.short()
    );
    println!("  skill:  {}", result.skill_name);
    println!("  window: {}", result.window_id);
    Ok(())
}

async fn cmd_list(
    app: ClatApp<impl Runtime>,
    all: bool,
    project: Option<String>,
) -> anyhow::Result<()> {
    let tasks = app.list_tasks(all, project.as_deref()).await?;

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
        #[tabled(rename = "Started")]
        started: String,
        #[tabled(rename = "Exit")]
        exit_code: String,
    }

    let win_numbers = app.window_numbers();
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

async fn cmd_close(app: ClatApp<impl Runtime>, id: &str) -> anyhow::Result<()> {
    let result = app.close(id).await?;
    println!(
        "Closed task {} ({})",
        result.task_name,
        result.task_id.short()
    );
    Ok(())
}

async fn cmd_reopen(app: ClatApp<impl Runtime>, id: &str) -> anyhow::Result<()> {
    let window_id = app.reopen(id).await?;
    println!("Reopened task {id} (window: {window_id})");
    Ok(())
}

async fn cmd_delete(app: ClatApp<impl Runtime>, id: &str) -> anyhow::Result<()> {
    let result = app.delete(id).await?;
    println!(
        "Deleted task {} ({})",
        result.task_name,
        result.task_id.short()
    );
    Ok(())
}

async fn cmd_move(app: ClatApp<impl Runtime>, id: &str, project: &str) -> anyhow::Result<()> {
    let result = app.move_task(id, project).await?;
    println!(
        "Moved task {} ({}) to project '{}'",
        result.task_name,
        result.task_id.short(),
        result.project_name,
    );
    Ok(())
}

async fn cmd_log(app: ClatApp<impl Runtime>, id_prefix: &str) -> anyhow::Result<()> {
    let log = app.log(id_prefix).await?;

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

async fn cmd_goto(app: ClatApp<impl Runtime>, id: &str) -> anyhow::Result<()> {
    app.goto(id).await
}

fn cmd_start(
    resume: Option<&str>,
    caffeinate: bool,
    dangerously_skip_permissions: bool,
) -> anyhow::Result<()> {
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
    if dangerously_skip_permissions {
        dash_cmd.push_str(" --dangerously-skip-permissions");
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

async fn cmd_send(app: ClatApp<impl Runtime>, id: &str, message: &str) -> anyhow::Result<()> {
    let result = app.send(id, message).await?;
    println!(
        "Sent message to {} ({})",
        result.task_name,
        result.task_id.short()
    );
    Ok(())
}

async fn cmd_complete(
    app: ClatApp<impl Runtime>,
    id: &str,
    exit_code: i32,
    output_file: Option<&str>,
) -> anyhow::Result<()> {
    let output = output_file.and_then(|path| std::fs::read_to_string(path).ok());
    let result = app.complete(id, exit_code, output.as_deref()).await?;

    let status = if exit_code == 0 {
        "completed"
    } else {
        "failed"
    };
    println!(
        "Task {} ({}) marked as {status} (exit code: {exit_code})",
        result.task_name,
        result.task_id.short()
    );

    Ok(())
}

fn cmd_skill(action: SkillAction, app: ClatApp<impl Runtime>) -> anyhow::Result<()> {
    match action {
        SkillAction::List => {
            let skills = app.list_skills()?;
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

async fn cmd_project(action: ProjectAction, app: ClatApp<impl Runtime>) -> anyhow::Result<()> {
    match action {
        ProjectAction::Create { name, description } => {
            let project = app.create_project(&name, &description).await?;
            println!(
                "Created project '{}' ({})",
                project.name,
                &project.id.to_string()[..8]
            );
        }
        ProjectAction::List => {
            let projects = app.list_projects().await?;
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
                    id: p.id.to_string()[..8].to_string(),
                    name: p.name.as_str().to_string(),
                    description: p.description.clone(),
                    created: p.created_at.format("%Y-%m-%d %H:%M").to_string(),
                })
                .collect();

            println!("{}", Table::new(rows));
        }
        ProjectAction::Delete { name } => {
            app.delete_project(&name).await?;
            println!("Deleted project '{name}'");
        }
        ProjectAction::Send { name, message } => {
            cmd_project_send(&app, &name, &message).await?;
        }
        ProjectAction::Log { name, last } => {
            cmd_project_log(&app, &name, last).await?;
        }
    }
    Ok(())
}

async fn cmd_project_log(
    app: &ClatApp<impl Runtime>,
    name: &str,
    last: Option<u32>,
) -> anyhow::Result<()> {
    let (project, messages) = app.project_log(name, last).await?;

    if messages.is_empty() {
        println!("No messages for project '{}'.", project.name);
        return Ok(());
    }

    for msg in &messages {
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

    Ok(())
}

async fn cmd_project_send(
    app: &ClatApp<impl Runtime>,
    name: &str,
    message: &str,
) -> anyhow::Result<()> {
    // Verify the project exists
    let project = app.resolve_project(name).await?;
    crate::permission::send_pm_message(app.project_root(), project.name.as_str(), message)?;
    println!("Sent message to PM for project '{}'", project.name);
    Ok(())
}
