use chrono::{DateTime, Utc};

#[derive(Debug)]
#[allow(dead_code)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub skill_name: String,
    pub params_json: String,
    pub status: String,
    pub tmux_pane: Option<String>,
    pub tmux_window: Option<String>,
    pub work_dir: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
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
