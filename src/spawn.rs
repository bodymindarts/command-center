use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::skill::SkillFile;

pub struct SpawnResult {
    pub window_id: String,
    pub pane_id: String,
}

pub fn spawn_agent(
    task_id: &str,
    skill: &SkillFile,
    rendered_prompt: &str,
    cc_bin: &Path,
    work_dir: &Path,
) -> Result<SpawnResult> {
    if std::env::var("TMUX").is_err() {
        bail!("cc spawn must be run inside a tmux session");
    }

    // Write prompt to temp file
    let prompt_path = std::env::temp_dir().join(format!("cc-prompt-{task_id}.txt"));
    std::fs::write(&prompt_path, rendered_prompt).context("failed to write prompt to temp file")?;

    let output_path = std::env::temp_dir().join(format!("cc-task-{task_id}.out"));

    let work_dir_str = work_dir.display().to_string();
    let window_name = format!("cc:{}", skill.skill.name);

    // 1. Create new window (starts with a single pane — this becomes the top/nvim pane)
    let window_id = tmux_cmd(&[
        "new-window",
        "-P",
        "-F",
        "#{window_id}",
        "-n",
        &window_name,
        "-c",
        &work_dir_str,
    ])?;

    // Get the initial pane ID (top pane)
    let top_pane = tmux_cmd(&["list-panes", "-t", &window_id, "-F", "#{pane_id}"])?;

    // 2. Split vertically to create a bottom pane
    let bottom_pane = tmux_cmd(&[
        "split-window",
        "-v",
        "-t",
        &top_pane,
        "-P",
        "-F",
        "#{pane_id}",
        "-c",
        &work_dir_str,
    ])?;

    // 3. Resize: push the divider down so top pane is bigger
    tmux_cmd(&["resize-pane", "-D", "9", "-t", &top_pane])?;

    // 4. Split bottom pane horizontally → bottom-left (shell) and bottom-right (claude)
    let tools = skill.agent.allowed_tools.join(",");
    let model = &skill.agent.model;
    let cc_bin_str = cc_bin.display();
    let prompt_path_str = prompt_path.display();
    let output_path_str = output_path.display();

    let claude_cmd = format!(
        r#"claude -p "$(cat {prompt_path_str})" --allowedTools '{tools}' --model {model} 2>&1 | tee {output_path_str}; {cc_bin_str} complete {task_id} $? {output_path_str}; read -p 'Press enter to close...'"#
    );

    let claude_pane = tmux_cmd(&[
        "split-window",
        "-h",
        "-t",
        &bottom_pane,
        "-P",
        "-F",
        "#{pane_id}",
        "-c",
        &work_dir_str,
        &claude_cmd,
    ])?;

    // 5. Send nvim to top pane
    tmux_cmd(&["send-keys", "-t", &top_pane, "nvim .", "Enter"])?;

    Ok(SpawnResult {
        window_id,
        pane_id: claude_pane,
    })
}

fn tmux_cmd(args: &[&str]) -> Result<String> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .with_context(|| format!("failed to run tmux {}", args[0]))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("tmux {} failed: {stderr}", args[0]);
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
