mod app;
mod assistant;
mod cli;
mod config;
mod jwt;
mod mcp;
mod permission;
mod permission_log;
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
use crate::cli::{AgentCommand, Cli, Command, MemoryAction, ProjectAction, SkillAction};
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
    let app = ClatApp::init(TmuxRuntime, skip_permissions).await?;

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
        Command::Send {
            project,
            from,
            args,
        } => match (project, args.len()) {
            (None, 2) if args[0] == "exo" => cmd_exo_send(&app, &args[1], from.as_deref())?,
            (None, 2) => cmd_send(app, &args[0], &args[1]).await?,
            (Some(name), 1) => cmd_project_send(&app, &name, &args[0], from.as_deref()).await?,
            (None, 1) => bail!("missing message: usage: clat send <ID> <message>"),
            (Some(_), n) if n >= 2 => {
                bail!("unexpected task ID when --project is provided")
            }
            _ => unreachable!(),
        },
        Command::Skill { action } => cmd_skill(action, app)?,
        Command::Project { action } => cmd_project(action, app).await?,
        Command::Memory { action } => cmd_memory(action, &app).await?,
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

async fn cmd_dash<R: Runtime>(
    app: ClatApp<R>,
    resume: Option<&str>,
    caffeinate: bool,
) -> anyhow::Result<()> {
    let app = Arc::new(app);
    let project_root = app.project_root().to_path_buf();

    // Start MCP server on localhost.
    const MCP_PORT: u16 = 9111;
    match mcp::start_mcp_server(Arc::clone(&app), MCP_PORT).await {
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
    // Use Full prompt mode when task params are provided (agent has work to do).
    // Interactive mode is only for truly interactive sessions (no task).
    let has_task = opts.params.iter().any(|(k, _)| k == "task");
    let (work_dir_mode, prompt_mode) = if opts.scratch {
        (WorkDirMode::Scratch, PromptMode::Full)
    } else if opts.no_worktree {
        let pm = if has_task {
            PromptMode::Full
        } else {
            PromptMode::Interactive
        };
        match opts.repo.as_deref() {
            Some(dir) => (WorkDirMode::Existing { dir }, pm),
            None => (WorkDirMode::Scratch, pm),
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

async fn cmd_memory(action: MemoryAction, app: &ClatApp<impl Runtime>) -> anyhow::Result<()> {
    let mem = app.memory();

    match action {
        MemoryAction::Store {
            title,
            content,
            tag,
            project,
            memory_type,
        } => match memory_type.as_str() {
            "report" => {
                let new = agent_memory::research_report::NewResearchReport {
                    id: agent_memory::primitives::ResearchReportId::new(),
                    title,
                    content,
                    tags: tag,
                    project,
                    source_task: None,
                };
                let report = mem.store_report(new).await?;
                println!(
                    "Stored report {} — {}",
                    &report.id.to_string()[..8],
                    report.title
                );
            }
            _ => {
                let new = agent_memory::natural_memory::NewNaturalMemory {
                    title,
                    content,
                    tags: tag,
                    project,
                    source_task: None,
                    source_type: "cli".to_string(),
                };
                let memory = mem.store_natural(new).await?;
                println!("Stored memory {} — {}", &memory.id[..8], memory.title);
            }
        },
        MemoryAction::Search {
            query,
            project,
            limit,
        } => {
            let results = mem.search(&query, project.as_deref(), limit).await?;
            if results.is_empty() {
                println!("No results.");
                return Ok(());
            }

            #[derive(Tabled)]
            struct Row {
                #[tabled(rename = "ID")]
                id: String,
                #[tabled(rename = "Type")]
                memory_type: String,
                #[tabled(rename = "Score")]
                score: String,
                #[tabled(rename = "Decay")]
                decay: String,
                #[tabled(rename = "📌")]
                pinned: String,
                #[tabled(rename = "Title")]
                title: String,
                #[tabled(rename = "Tags")]
                tags: String,
                #[tabled(rename = "Project")]
                project: String,
                #[tabled(rename = "Snippet")]
                snippet: String,
            }

            let rows: Vec<Row> = results
                .iter()
                .map(|r| {
                    let snippet: String = r.content.chars().take(80).collect();
                    Row {
                        id: r.id[..8].to_string(),
                        memory_type: r.memory_type.to_string(),
                        score: format!("{:.4}", r.score),
                        decay: format!("{:.0}%", r.decay_factor * 100.0),
                        pinned: if r.pinned {
                            "y".to_string()
                        } else {
                            String::new()
                        },
                        title: r.title.clone(),
                        tags: r.tags.join(", "),
                        project: r.project.clone().unwrap_or_else(|| "-".to_string()),
                        snippet: snippet.replace('\n', " "),
                    }
                })
                .collect();

            println!("{}", Table::new(rows));
        }
        MemoryAction::List {
            project,
            memory_type,
            limit,
        } => {
            #[derive(Tabled)]
            struct Row {
                #[tabled(rename = "ID")]
                id: String,
                #[tabled(rename = "Type")]
                memory_type: String,
                #[tabled(rename = "Title")]
                title: String,
                #[tabled(rename = "Tags")]
                tags: String,
                #[tabled(rename = "Project")]
                project: String,
                #[tabled(rename = "Created")]
                created: String,
            }

            let mut rows: Vec<Row> = Vec::new();
            let show_natural = memory_type.is_none() || memory_type.as_deref() == Some("natural");
            let show_report = memory_type.is_none() || memory_type.as_deref() == Some("report");

            if show_natural {
                let memories = mem.list_natural(project.as_deref(), limit).await?;
                for m in &memories {
                    rows.push(Row {
                        id: m.id[..8].to_string(),
                        memory_type: "natural".to_string(),
                        title: m.title.clone(),
                        tags: m.tags.join(", "),
                        project: m.project.clone().unwrap_or_else(|| "-".to_string()),
                        created: m.created_at.format("%Y-%m-%d").to_string(),
                    });
                }
            }

            if show_report {
                let reports = mem.list_reports(project.as_deref(), limit).await?;
                for r in &reports {
                    rows.push(Row {
                        id: r.id.to_string()[..8].to_string(),
                        memory_type: "report".to_string(),
                        title: r.title.clone(),
                        tags: r.tags.join(", "),
                        project: r.project.clone().unwrap_or_else(|| "-".to_string()),
                        created: r.created_at.format("%Y-%m-%d").to_string(),
                    });
                }
            }

            if rows.is_empty() {
                println!("No memories.");
                return Ok(());
            }

            println!("{}", Table::new(rows));
        }
        MemoryAction::Get { id } => {
            use agent_memory::service::MemoryItem;
            let item = mem.get(&id).await?;
            match item {
                MemoryItem::Natural(m) => {
                    println!("ID:      {}", m.id);
                    println!("Type:    natural");
                    println!("Title:   {}", m.title);
                    println!("Tags:    {}", m.tags.join(", "));
                    println!("Project: {}", m.project.as_deref().unwrap_or("-"));
                    println!("Created: {}", m.created_at.format("%Y-%m-%d %H:%M"));
                    println!("File:    {}", m.file_path);
                    println!();
                    println!("{}", m.content);
                }
                MemoryItem::Report(r) => {
                    println!("ID:      {}", r.id);
                    println!("Type:    report");
                    println!("Title:   {}", r.title);
                    println!("Tags:    {}", r.tags.join(", "));
                    println!("Project: {}", r.project.as_deref().unwrap_or("-"));
                    println!("Status:  {}", r.status);
                    println!("Created: {}", r.created_at.format("%Y-%m-%d %H:%M"));
                    println!();
                    println!("{}", r.content);
                }
            }
        }
        MemoryAction::Update {
            id,
            title,
            content,
            tag,
        } => {
            if title.is_none() && content.is_none() && tag.is_none() {
                bail!("at least one of --title, --content, or --tag must be provided");
            }
            // Verify it's a report, not a natural memory.
            use agent_memory::service::MemoryItem;
            let item = mem.get(&id).await?;
            match item {
                MemoryItem::Natural(_) => {
                    bail!(
                        "'{id}' is a natural memory — update is only supported for research reports"
                    );
                }
                MemoryItem::Report(_) => {}
            }
            let update = agent_memory::research_report::ReportUpdate {
                title,
                content,
                tags: tag,
            };
            let report = mem.update_report(&id, update).await?;
            println!(
                "Updated report {} — {}",
                &report.id.to_string()[..8],
                report.title
            );
        }
        MemoryAction::Reindex => {
            let count = mem.reindex().await?;
            println!("Reindexed {count} memories from disk.");
        }
    }

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
            cmd_project_send(&app, &name, &message, None).await?;
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

fn cmd_exo_send(
    app: &ClatApp<impl Runtime>,
    message: &str,
    from: Option<&str>,
) -> anyhow::Result<()> {
    let content = format_with_sender(message, from);
    crate::permission::send_exo_message(app.project_root(), &content)?;
    println!("Sent message to ExO");
    Ok(())
}

async fn cmd_project_send(
    app: &ClatApp<impl Runtime>,
    name: &str,
    message: &str,
    from: Option<&str>,
) -> anyhow::Result<()> {
    let project = app.resolve_project(name).await?;
    let content = format_with_sender(message, from);
    crate::permission::send_pm_message(app.project_root(), project.name.as_str(), &content)?;
    println!("Sent message to PM for project '{}'", project.name);
    Ok(())
}

fn format_with_sender(message: &str, from: Option<&str>) -> String {
    match from {
        Some(name) => format!("[from {name}] {message}"),
        None => message.to_string(),
    }
}
