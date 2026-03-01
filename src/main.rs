mod cli;
mod config;
mod permission;
mod skill;
mod spawn;
mod store;
mod tui;

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Parser;
use tabled::{Table, Tabled};

use crate::cli::{Cli, Command};
use crate::config::Paths;
use crate::skill::SkillFile;
use crate::store::{Store, Task};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve()?;
    paths.ensure_dirs()?;
    let store = Store::open(&paths.db_path)?;

    match cli.command {
        Command::Spawn { name, skill, param } => cmd_spawn(&paths, &store, &name, &skill, param)?,
        Command::List { all } => cmd_list(&store, all)?,
        Command::History => cmd_list(&store, true)?,
        Command::Close { id } => cmd_close(&store, &id)?,
        Command::Dash { resume } => tui::run(&store, resume.as_deref())?,
        Command::Start { resume } => cmd_start(resume.as_deref())?,
        Command::Goto { id } => cmd_goto(&store, &id)?,
        Command::Send { id, message } => cmd_send(&store, &id, &message)?,
        Command::Complete {
            id,
            exit_code,
            output_file,
        } => cmd_complete(&store, &id, exit_code, output_file.as_deref())?,
    }

    Ok(())
}

fn cmd_spawn(
    paths: &Paths,
    store: &Store,
    task_name: &str,
    skill_name: &str,
    params: Vec<(String, String)>,
) -> Result<()> {
    let skill = SkillFile::load(&paths.skills_dir, skill_name)?;

    let params_map: HashMap<String, String> = params.into_iter().collect();
    skill.validate_params(&params_map)?;

    let rendered = skill.render_prompt(&params_map)?;

    let task_id = uuid::Uuid::new_v4().to_string();
    let short_id = &task_id[..8];
    let worktree_name = format!("{task_name}-{short_id}");
    let worktree_path = spawn::create_worktree(&paths.root, &worktree_name)?;

    let task = Task {
        id: task_id.clone(),
        name: task_name.to_string(),
        skill_name: skill_name.to_string(),
        params_json: serde_json::to_string(&params_map)?,
        status: "running".to_string(),
        tmux_pane: None,
        tmux_window: None,
        work_dir: Some(worktree_path.display().to_string()),
        started_at: Utc::now(),
        completed_at: None,
        exit_code: None,
        output: None,
    };

    store.insert_task(&task)?;

    let result = spawn::spawn_agent(task_name, &rendered, &worktree_path)?;
    store.update_tmux_pane(&task_id, &result.pane_id)?;
    store.update_tmux_window(&task_id, &result.window_id)?;

    println!("Spawned task {task_name} ({short_id})");
    println!("  skill:  {skill_name}");
    println!("  window: {}", result.window_id);

    Ok(())
}

fn cmd_list(store: &Store, all: bool) -> Result<()> {
    let tasks = if all {
        store.list_tasks()?
    } else {
        store.list_active_tasks()?
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

fn cmd_close(store: &Store, id_prefix: &str) -> Result<()> {
    let task = store
        .get_task_by_prefix(id_prefix)?
        .ok_or_else(|| anyhow::anyhow!("no task found matching '{id_prefix}'"))?;

    if task.status != "running" {
        bail!(
            "task {} ({}) is '{}', not 'running'",
            task.name,
            &task.id[..8],
            task.status
        );
    }

    let output = task
        .tmux_pane
        .as_deref()
        .and_then(|pane| spawn::capture_pane_output(pane).ok());

    if let Some(window_id) = &task.tmux_window {
        let _ = spawn::kill_tmux_window(window_id);
    }

    let closed = store.close_task(&task.id, output.as_deref())?;
    if !closed {
        bail!("failed to close task {} ({})", task.name, &task.id[..8]);
    }

    println!("Closed task {} ({})", task.name, &task.id[..8]);
    Ok(())
}

fn cmd_goto(store: &Store, id_prefix: &str) -> Result<()> {
    let task = store
        .get_task_by_prefix(id_prefix)?
        .ok_or_else(|| anyhow::anyhow!("no task found matching '{id_prefix}'"))?;

    let window_id = task
        .tmux_window
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("task {} has no tmux window", &task.id[..8]))?;

    let output = std::process::Command::new("tmux")
        .args(["select-window", "-t", window_id])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("tmux select-window failed: {stderr}");
    }

    Ok(())
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
        // Use the current pane as the dashboard, split below for shell
        let top_pane = spawn::tmux_cmd(&["display-message", "-p", "#{pane_id}"])?;
        spawn::tmux_cmd(&["split-window", "-v", "-t", &top_pane])?;
        spawn::tmux_cmd(&["resize-pane", "-t", &top_pane, "-D", "8"])?;
        spawn::tmux_cmd(&["send-keys", "-t", &top_pane, &dash_cmd, "Enter"])?;
    } else {
        spawn::tmux_cmd(&["new-session", "-d", "-s", "exo", "-n", "exo"])?;
        let top_pane = spawn::tmux_cmd(&["list-panes", "-t", "exo:exo", "-F", "#{pane_id}"])?;
        spawn::tmux_cmd(&["send-keys", "-t", &top_pane, &dash_cmd, "Enter"])?;
        spawn::tmux_cmd(&["split-window", "-v", "-t", "exo:exo"])?;
        spawn::tmux_cmd(&["resize-pane", "-t", &top_pane, "-D", "8"])?;

        let status = std::process::Command::new("tmux")
            .args(["attach-session", "-t", "exo"])
            .status()?;

        if !status.success() {
            bail!("tmux attach-session failed");
        }
    }

    Ok(())
}

fn cmd_send(store: &Store, id_prefix: &str, message: &str) -> Result<()> {
    let task = store
        .get_task_by_prefix(id_prefix)?
        .ok_or_else(|| anyhow::anyhow!("no task found matching '{id_prefix}'"))?;

    let pane_id = task
        .tmux_pane
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("task {} has no tmux pane", &task.id[..8]))?;

    spawn::send_keys_to_pane(pane_id, message)?;
    println!("Sent message to {} ({})", task.name, &task.id[..8]);

    Ok(())
}

fn cmd_complete(store: &Store, id: &str, exit_code: i32, output_file: Option<&str>) -> Result<()> {
    let output = output_file.and_then(|path| std::fs::read_to_string(path).ok());
    store.complete_task(id, exit_code, output.as_deref())?;

    let status = if exit_code == 0 {
        "completed"
    } else {
        "failed"
    };
    println!("Task {id} marked as {status} (exit code: {exit_code})");

    Ok(())
}
