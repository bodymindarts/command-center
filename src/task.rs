use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::primitives::{
    ClaudeSessionId, MessageRole, PaneId, ProjectId, ProjectName, TaskId, TaskName, TaskStatus,
    WindowId,
};

/// Visual status of a task, combining persisted `TaskStatus` with runtime
/// idle-detection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStatus {
    Active,
    Idle,
    Completed,
    Failed,
    Closed,
}

impl DisplayStatus {
    /// Single-char indicator for the task list.
    pub fn indicator(&self) -> &str {
        match self {
            Self::Active => "●",
            Self::Idle => "◉",
            Self::Completed => "✓",
            Self::Failed => "✗",
            Self::Closed => "○",
        }
    }

    /// Whether the task should be rendered with DIM modifier.
    pub fn is_dim(&self) -> bool {
        !matches!(self, Self::Active | Self::Idle)
    }
}

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
    pub session_id: Option<ClaudeSessionId>,
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
            session_id: Some(ClaudeSessionId::generate()),
            started_at: Utc::now(),
            completed_at: None,
            exit_code: None,
            output: None,
            project_id,
        }
    }

    fn is_idle(&self, idle_panes: &HashSet<PaneId>) -> bool {
        self.status.is_running()
            && self
                .tmux_pane
                .as_ref()
                .is_some_and(|p| idle_panes.contains(p))
    }

    /// Derive the visual display status from persisted status + idle set.
    pub fn display_status(&self, idle_panes: &HashSet<PaneId>) -> DisplayStatus {
        {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("data/hook-received.log")
            {
                let now = chrono::Local::now().format("%H:%M:%S%.3f");
                let _ = writeln!(
                    f,
                    "[{now}] display_status: {} status={:?} pane={:?} idle_panes={:?}",
                    self.name, self.status, self.tmux_pane, idle_panes
                );
            }
        }
        match self.status {
            TaskStatus::Running if self.is_idle(idle_panes) => DisplayStatus::Idle,
            TaskStatus::Running => DisplayStatus::Active,
            TaskStatus::Completed => DisplayStatus::Completed,
            TaskStatus::Failed => DisplayStatus::Failed,
            TaskStatus::Closed => DisplayStatus::Closed,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct TaskMessage {
    pub id: String,
    pub chat_id: String,
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
