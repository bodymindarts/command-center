use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::primitives::{TaskId, TaskStatus};

#[derive(Debug)]
#[allow(dead_code)]
pub struct Task {
    pub id: TaskId,
    pub name: String,
    pub skill_name: String,
    pub params_json: String,
    pub status: TaskStatus,
    pub tmux_pane: Option<String>,
    pub tmux_window: Option<String>,
    pub work_dir: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
}

impl Task {
    pub fn new(
        id: TaskId,
        name: &str,
        skill_name: &str,
        params: &HashMap<String, String>,
        work_dir: &Path,
    ) -> Self {
        Self {
            id,
            name: name.to_string(),
            skill_name: skill_name.to_string(),
            params_json: serde_json::to_string(params).unwrap_or_else(|_| "{}".to_string()),
            status: TaskStatus::Running,
            tmux_pane: None,
            tmux_window: None,
            work_dir: Some(work_dir.display().to_string()),
            started_at: Utc::now(),
            completed_at: None,
            exit_code: None,
            output: None,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct TaskMessage {
    pub id: String,
    pub task_id: String,
    pub role: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}
