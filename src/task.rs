use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::primitives::{
    MessageRole, PaneId, ProjectId, ProjectName, TaskId, TaskName, TaskStatus, WindowId,
};

#[derive(Debug)]
#[allow(dead_code)]
pub struct Task {
    pub id: TaskId,
    pub name: TaskName,
    pub skill_name: String,
    pub params_json: String,
    pub status: TaskStatus,
    pub tmux_pane: Option<PaneId>,
    pub tmux_window: Option<WindowId>,
    pub work_dir: Option<String>,
    pub session_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
    pub project_id: Option<ProjectId>,
}

impl Task {
    pub fn new(
        id: TaskId,
        name: TaskName,
        skill_name: &str,
        params: &HashMap<String, String>,
        work_dir: &Path,
        project_id: Option<ProjectId>,
    ) -> Self {
        Self {
            id,
            name,
            skill_name: skill_name.to_string(),
            params_json: serde_json::to_string(params).unwrap_or_else(|_| "{}".to_string()),
            status: TaskStatus::Running,
            tmux_pane: None,
            tmux_window: None,
            work_dir: Some(work_dir.display().to_string()),
            session_id: None,
            started_at: Utc::now(),
            completed_at: None,
            exit_code: None,
            output: None,
            project_id,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct TaskMessage {
    pub id: String,
    pub task_id: TaskId,
    pub role: MessageRole,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct Project {
    pub id: ProjectId,
    pub name: ProjectName,
    pub description: String,
    pub created_at: DateTime<Utc>,
}
