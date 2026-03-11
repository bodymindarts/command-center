mod app;
mod assistant;
mod cli;
mod config;
mod permission;
mod primitives;
mod runtime;
mod schedule;
mod skill;
mod store;
mod task;
mod tui;

use anyhow::{Context, bail};
use clap::Parser;
use tabled::{Table, Tabled};

use crate::app::{ClatApp, PromptMode, SpawnRequest, WorkDirMode};
use crate::cli::{AgentCommand, Cli, Command, ProjectAction, ScheduleAction, SkillAction};
use crate::primitives::MessageRole;
use crate::runtime::{Runtime, TmuxRuntime};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let app = ClatApp::try_new(TmuxRuntime)?;

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
            on_complete,
            on_fail,
            on_idle,
        } => cmd_spawn(
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
                on_complete,
                on_fail,
                on_idle,
            },
        )?,
        Command::List { all, project } => cmd_list(app, all, project)?,
        Command::History => cmd_list(app, true, None)?,
        Command::Log { id } => cmd_log(app, &id)?,
        Command::Close { id } => cmd_close(app, &id)?,
        Command::Reopen { id } => cmd_reopen(app, &id)?,
        Command::Delete { id } => cmd_delete(app, &id)?,
        Command::Dash { resume, caffeinate } => tui::run(app, resume.as_deref(), caffeinate)?,
        Command::Start { resume, caffeinate } => cmd_start(resume.as_deref(), caffeinate)?,
        Command::Goto { id } => cmd_goto(app, &id)?,
        Command::Send { id, message } => cmd_send(app, &id, &message)?,
        Command::Skill { action } => cmd_skill(action, app)?,
        Command::Project { action } => cmd_project(action, app)?,
        Command::Schedule { action } => cmd_schedule(action, app)?,
        Command::Watch {
            name,
            check,
            diff,
            every,
            action,
            max_runs,
        } => cmd_watch(app, &name, &check, &diff, &every, &action, max_runs)?,
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
            } => cmd_complete(app, &id, exit_code, output_file.as_deref())?,
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
    on_complete: Option<String>,
    on_fail: Option<String>,
    on_idle: Option<String>,
}

fn cmd_spawn(app: ClatApp<impl Runtime>, opts: SpawnOpts) -> anyhow::Result<()> {
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

    let result = app.spawn(SpawnRequest {
        task_name: &opts.name,
        skill_name: &opts.skill,
        params: opts.params,
        work_dir_mode,
        prompt_mode,
        project,
        on_complete_success: opts.on_complete,
        on_complete_failure: opts.on_fail,
        on_idle: opts.on_idle,
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

fn cmd_list(app: ClatApp<impl Runtime>, all: bool, project: Option<String>) -> anyhow::Result<()> {
    let tasks = app.list_tasks(all, project.as_deref())?;

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

fn cmd_close(app: ClatApp<impl Runtime>, id: &str) -> anyhow::Result<()> {
    let result = app.close(id)?;
    println!(
        "Closed task {} ({})",
        result.task_name,
        result.task_id.short()
    );
    Ok(())
}

fn cmd_reopen(app: ClatApp<impl Runtime>, id: &str) -> anyhow::Result<()> {
    let window_id = app.reopen(id)?;
    println!("Reopened task {id} (window: {window_id})");
    Ok(())
}

fn cmd_delete(app: ClatApp<impl Runtime>, id: &str) -> anyhow::Result<()> {
    let result = app.delete(id)?;
    println!(
        "Deleted task {} ({})",
        result.task_name,
        result.task_id.short()
    );
    Ok(())
}

fn cmd_log(app: ClatApp<impl Runtime>, id_prefix: &str) -> anyhow::Result<()> {
    let log = app.log(id_prefix)?;

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

fn cmd_goto(app: ClatApp<impl Runtime>, id: &str) -> anyhow::Result<()> {
    app.goto(id)
}

fn cmd_start(resume: Option<&str>, caffeinate: bool) -> anyhow::Result<()> {
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

fn cmd_send(app: ClatApp<impl Runtime>, id: &str, message: &str) -> anyhow::Result<()> {
    let result = app.send(id, message)?;
    println!(
        "Sent message to {} ({})",
        result.task_name,
        result.task_id.short()
    );
    Ok(())
}

fn cmd_complete(
    app: ClatApp<impl Runtime>,
    id: &str,
    exit_code: i32,
    output_file: Option<&str>,
) -> anyhow::Result<()> {
    let output = output_file.and_then(|path| std::fs::read_to_string(path).ok());
    let result = app.complete(id, exit_code, output.as_deref())?;

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

fn cmd_schedule(action: ScheduleAction, app: ClatApp<impl Runtime>) -> anyhow::Result<()> {
    match action {
        ScheduleAction::Create {
            name,
            every,
            cron,
            once,
            action,
            max_runs,
            check,
            diff,
        } => {
            let (schedule_type, schedule_expr) = if let Some(expr) = every {
                (schedule::ScheduleType::Interval, expr)
            } else if let Some(expr) = cron {
                (schedule::ScheduleType::Cron, expr)
            } else if let Some(expr) = once {
                (schedule::ScheduleType::Once, expr)
            } else {
                bail!("specify one of --every, --cron, or --once");
            };

            let diff_mode = schedule::DiffMode::from(diff);
            let schedule = app.create_schedule(
                &name,
                schedule_type,
                &schedule_expr,
                &action,
                max_runs,
                check.as_deref(),
                diff_mode,
            )?;
            println!("Created schedule '{}'", schedule.name);
            if let Some(next) = schedule.next_run_at {
                println!("  next run: {}", next.format("%Y-%m-%d %H:%M:%S UTC"));
            }
        }
        ScheduleAction::List => {
            let schedules = app.list_schedules()?;
            if schedules.is_empty() {
                println!("No schedules.");
                return Ok(());
            }

            #[derive(Tabled)]
            struct Row {
                #[tabled(rename = "ID")]
                id: String,
                #[tabled(rename = "Name")]
                name: String,
                #[tabled(rename = "Type")]
                schedule_type: String,
                #[tabled(rename = "Expression")]
                expr: String,
                #[tabled(rename = "Enabled")]
                enabled: String,
                #[tabled(rename = "Runs")]
                runs: String,
                #[tabled(rename = "Next Run")]
                next_run: String,
                #[tabled(rename = "Action")]
                action: String,
            }

            let rows: Vec<Row> = schedules
                .iter()
                .map(|s| Row {
                    id: s.id.short().to_string(),
                    name: s.name.clone(),
                    schedule_type: s.schedule_type.to_string(),
                    expr: s.schedule_expr.clone(),
                    enabled: if s.enabled { "yes" } else { "no" }.to_string(),
                    runs: match s.max_runs {
                        Some(max) => format!("{}/{}", s.run_count, max),
                        None => s.run_count.to_string(),
                    },
                    next_run: s
                        .next_run_at
                        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    action: s.action.clone(),
                })
                .collect();

            println!("{}", Table::new(rows));
        }
        ScheduleAction::Delete { name_or_id } => {
            let name = app.delete_schedule(&name_or_id)?;
            println!("Deleted schedule '{name}'");
        }
        ScheduleAction::Enable { name_or_id } => {
            let name = app.set_schedule_enabled(&name_or_id, true)?;
            println!("Enabled schedule '{name}'");
        }
        ScheduleAction::Disable { name_or_id } => {
            let name = app.set_schedule_enabled(&name_or_id, false)?;
            println!("Disabled schedule '{name}'");
        }
    }
    Ok(())
}

fn cmd_watch(
    app: ClatApp<impl Runtime>,
    name: &str,
    check: &str,
    diff: &str,
    every: &str,
    action: &str,
    max_runs: Option<i64>,
) -> anyhow::Result<()> {
    let diff_mode = schedule::DiffMode::from(diff.to_string());
    let schedule = app.create_schedule(
        name,
        schedule::ScheduleType::Interval,
        every,
        action,
        max_runs,
        Some(check),
        diff_mode,
    )?;
    println!("Created watch '{}'", schedule.name);
    println!("  check: {check}");
    if let Some(next) = schedule.next_run_at {
        println!("  next check: {}", next.format("%Y-%m-%d %H:%M:%S UTC"));
    }
    Ok(())
}

fn cmd_project(action: ProjectAction, app: ClatApp<impl Runtime>) -> anyhow::Result<()> {
    match action {
        ProjectAction::Create { name, description } => {
            let project = app.create_project(&name, &description)?;
            println!(
                "Created project '{}' ({})",
                project.name,
                project.id.short()
            );
        }
        ProjectAction::List => {
            let projects = app.list_projects()?;
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
            app.delete_project(&name)?;
            println!("Deleted project '{name}'");
        }
        ProjectAction::Send { name, message } => {
            cmd_project_send(&app, &name, &message)?;
        }
    }
    Ok(())
}

fn cmd_project_send(app: &ClatApp<impl Runtime>, name: &str, message: &str) -> anyhow::Result<()> {
    // Verify the project exists
    let project = app.resolve_project(name)?;
    crate::permission::send_pm_message(app.project_root(), project.name.as_str(), message)?;
    println!("Sent message to PM for project '{}'", project.name);
    Ok(())
}
