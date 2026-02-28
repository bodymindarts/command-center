mod cli;
mod config;
mod skill;
mod spawn;
mod store;

use std::collections::HashMap;

use anyhow::{Result, bail};
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
        Command::Spawn { skill, param } => cmd_spawn(&paths, &store, &skill, param)?,
        Command::List => cmd_list(&store)?,
        Command::Goto { id } => cmd_goto(&store, &id)?,
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
    skill_name: &str,
    params: Vec<(String, String)>,
) -> Result<()> {
    let skill = SkillFile::load(&paths.skills_dir, skill_name)?;

    let params_map: HashMap<String, String> = params.into_iter().collect();
    skill.validate_params(&params_map)?;

    let rendered = skill.render_prompt(&params_map)?;

    let task_id = uuid::Uuid::new_v4().to_string();
    let task = Task {
        id: task_id.clone(),
        skill_name: skill_name.to_string(),
        params_json: serde_json::to_string(&params_map)?,
        status: "running".to_string(),
        tmux_pane: None,
        tmux_window: None,
        work_dir: Some(paths.root.display().to_string()),
        started_at: Utc::now(),
        completed_at: None,
        exit_code: None,
        output: None,
    };

    store.insert_task(&task)?;

    let result = spawn::spawn_agent(&task_id, &skill, &rendered, &paths.cc_bin, &paths.root)?;
    store.update_tmux_pane(&task_id, &result.pane_id)?;
    store.update_tmux_window(&task_id, &result.window_id)?;

    println!("Spawned task {task_id}");
    println!("  skill:  {skill_name}");
    println!("  window: {}", result.window_id);

    Ok(())
}

fn cmd_list(store: &Store) -> Result<()> {
    let tasks = store.list_tasks()?;

    if tasks.is_empty() {
        println!("No tasks.");
        return Ok(());
    }

    #[derive(Tabled)]
    struct Row {
        #[tabled(rename = "ID")]
        id: String,
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
