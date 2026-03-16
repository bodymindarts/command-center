use std::collections::HashMap;

use chrono::{DateTime, Utc};
use derive_builder::Builder;
use es_entity::*;
use serde::{Deserialize, Serialize};

use crate::primitives::{
    ClaudeSessionId, MessageRole, PaneId, ProjectId, TaskId, TaskName, TaskStatus, WindowId,
};

use super::error::TaskError;

// ── Events ───────────────────────────────────────────────────────────

#[derive(EsEvent, Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[es_event(id = "TaskId")]
pub enum TaskEvent {
    Initialized {
        id: TaskId,
        name: TaskName,
        skill_name: String,
        params_json: String,
        work_dir: Option<String>,
        session_id: ClaudeSessionId,
        project_id: Option<ProjectId>,
    },
    AgentLaunched {
        tmux_pane: PaneId,
        tmux_window: WindowId,
    },
    Completed {
        exit_code: i32,
        output: Option<String>,
    },
    Closed {
        output: Option<String>,
    },
    Reopened {
        tmux_pane: PaneId,
        tmux_window: WindowId,
    },
    Moved {
        project_id: Option<ProjectId>,
    },
    Deleted,
}

// ── Entity ───────────────────────────────────────────────────────────

#[derive(EsEntity, Builder, Clone)]
#[builder(pattern = "owned", build_fn(error = "EntityHydrationError"))]
pub struct Task {
    pub id: TaskId,
    pub name: TaskName,
    pub skill_name: String,
    #[allow(dead_code)]
    pub params_json: String,
    pub status: TaskStatus,
    pub tmux_pane: Option<PaneId>,
    pub tmux_window: Option<WindowId>,
    pub work_dir: Option<String>,
    pub session_id: Option<ClaudeSessionId>,
    #[builder(default)]
    pub started_at: DateTime<Utc>,
    #[builder(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[builder(default)]
    pub exit_code: Option<i32>,
    #[builder(default)]
    pub output: Option<String>,
    pub project_id: Option<ProjectId>,
    events: EntityEvents<TaskEvent>,
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("skill_name", &self.skill_name)
            .field("status", &self.status)
            .field("project_id", &self.project_id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Task: {} ({})", self.name, self.id)
    }
}

// ── Hydration ────────────────────────────────────────────────────────

impl TryFromEvents<TaskEvent> for Task {
    fn try_from_events(events: EntityEvents<TaskEvent>) -> Result<Self, EntityHydrationError> {
        let mut builder = TaskBuilder::default();

        // Derive timestamps from event metadata.
        if let Some(first) = events.entity_first_persisted_at() {
            builder = builder.started_at(first);
        }

        for event in events.iter_all() {
            match event {
                TaskEvent::Initialized {
                    id,
                    name,
                    skill_name,
                    params_json,
                    work_dir,
                    session_id,
                    project_id,
                } => {
                    builder = builder
                        .id(*id)
                        .name(name.clone())
                        .skill_name(skill_name.clone())
                        .params_json(params_json.clone())
                        .status(TaskStatus::Running)
                        .tmux_pane(None)
                        .tmux_window(None)
                        .work_dir(work_dir.clone())
                        .session_id(Some(*session_id))
                        .project_id(*project_id);
                }
                TaskEvent::AgentLaunched {
                    tmux_pane,
                    tmux_window,
                } => {
                    builder = builder
                        .tmux_pane(Some(tmux_pane.clone()))
                        .tmux_window(Some(tmux_window.clone()));
                }
                TaskEvent::Completed { exit_code, output } => {
                    let status = if *exit_code == 0 {
                        TaskStatus::Completed
                    } else {
                        TaskStatus::Failed
                    };
                    builder = builder
                        .status(status)
                        .exit_code(Some(*exit_code))
                        .output(output.clone());
                    if let Some(last) = events.entity_last_modified_at() {
                        builder = builder.completed_at(Some(last));
                    }
                }
                TaskEvent::Closed { output } => {
                    builder = builder.status(TaskStatus::Closed).output(output.clone());
                    if let Some(last) = events.entity_last_modified_at() {
                        builder = builder.completed_at(Some(last));
                    }
                }
                TaskEvent::Reopened {
                    tmux_pane,
                    tmux_window,
                } => {
                    builder = builder
                        .status(TaskStatus::Running)
                        .tmux_pane(Some(tmux_pane.clone()))
                        .tmux_window(Some(tmux_window.clone()))
                        .completed_at(None);
                }
                TaskEvent::Moved { project_id } => {
                    builder = builder.project_id(*project_id);
                }
                TaskEvent::Deleted => {
                    // Soft delete — repo-level flag; no entity state change.
                }
            }
        }

        builder.events(events).build()
    }
}

// ── NewTask (construction) ───────────────────────────────────────────

pub struct NewTask {
    pub id: TaskId,
    pub name: TaskName,
    pub skill_name: String,
    pub params_json: String,
    pub work_dir: Option<String>,
    pub session_id: ClaudeSessionId,
    pub project_id: Option<ProjectId>,
}

impl NewTask {
    /// Used by EsRepo column accessor for the `started_at` NOT NULL column.
    pub(super) fn started_at(&self) -> String {
        Utc::now().to_rfc3339()
    }
}

impl IntoEvents<TaskEvent> for NewTask {
    fn into_events(self) -> EntityEvents<TaskEvent> {
        EntityEvents::init(
            self.id,
            [TaskEvent::Initialized {
                id: self.id,
                name: self.name,
                skill_name: self.skill_name,
                params_json: self.params_json,
                work_dir: self.work_dir,
                session_id: self.session_id,
                project_id: self.project_id,
            }],
        )
    }
}

// ── Mutations ────────────────────────────────────────────────────────

impl Task {
    pub fn launch_agent(&mut self, pane: PaneId, window: WindowId) -> Idempotent<()> {
        idempotency_guard!(
            self.events.iter_all().rev(),
            already_applied: TaskEvent::AgentLaunched { tmux_pane, tmux_window }
                if *tmux_pane == pane && *tmux_window == window
        );

        self.tmux_pane = Some(pane.clone());
        self.tmux_window = Some(window.clone());
        self.events.push(TaskEvent::AgentLaunched {
            tmux_pane: pane,
            tmux_window: window,
        });
        Idempotent::Executed(())
    }

    pub fn complete(
        &mut self,
        exit_code: i32,
        output: Option<String>,
    ) -> Result<Idempotent<()>, TaskError> {
        idempotency_guard!(
            self.events.iter_all().rev(),
            already_applied: TaskEvent::Completed { exit_code: ec, .. } if *ec == exit_code,
            resets_on: TaskEvent::Reopened { .. }
        );

        if !self.status.is_running() {
            return Err(TaskError::NotRunning);
        }

        self.status = if exit_code == 0 {
            TaskStatus::Completed
        } else {
            TaskStatus::Failed
        };
        self.completed_at = Some(Utc::now());
        self.exit_code = Some(exit_code);
        self.output = output.clone();
        self.events.push(TaskEvent::Completed { exit_code, output });
        Ok(Idempotent::Executed(()))
    }

    pub fn close(&mut self, output: Option<String>) -> Idempotent<()> {
        idempotency_guard!(
            self.events.iter_all().rev(),
            already_applied: TaskEvent::Closed { .. },
            resets_on: TaskEvent::Reopened { .. }
        );

        if !self.status.is_running() {
            return Idempotent::AlreadyApplied;
        }

        self.status = TaskStatus::Closed;
        self.completed_at = Some(Utc::now());
        self.output = output.clone();
        self.events.push(TaskEvent::Closed { output });
        Idempotent::Executed(())
    }

    pub fn reopen(&mut self, pane: PaneId, window: WindowId) -> Result<Idempotent<()>, TaskError> {
        idempotency_guard!(
            self.events.iter_all().rev(),
            already_applied: TaskEvent::Reopened { tmux_pane, tmux_window }
                if *tmux_pane == pane && *tmux_window == window,
            resets_on: TaskEvent::Completed { .. } | TaskEvent::Closed { .. }
        );

        if self.status.is_running() {
            return Err(TaskError::AlreadyRunning);
        }

        self.status = TaskStatus::Running;
        self.tmux_pane = Some(pane.clone());
        self.tmux_window = Some(window.clone());
        self.completed_at = None;
        self.events.push(TaskEvent::Reopened {
            tmux_pane: pane,
            tmux_window: window,
        });
        Ok(Idempotent::Executed(()))
    }

    pub fn move_to_project(&mut self, project_id: Option<ProjectId>) -> Idempotent<()> {
        if self.project_id == project_id {
            return Idempotent::AlreadyApplied;
        }
        self.project_id = project_id;
        self.events.push(TaskEvent::Moved { project_id });
        Idempotent::Executed(())
    }

    pub fn delete(&mut self) -> Idempotent<()> {
        idempotency_guard!(
            self.events.iter_all(),
            already_applied: TaskEvent::Deleted
        );
        self.events.push(TaskEvent::Deleted);
        Idempotent::Executed(())
    }

    // ── Query helpers (not event-sourced) ────────────────────────────

    fn is_active(&self, active_panes: &HashMap<PaneId, bool>) -> bool {
        self.status.is_running()
            && self
                .tmux_pane
                .as_ref()
                .is_some_and(|p| active_panes.contains_key(p))
    }

    /// Derive the visual display status from persisted status + active map.
    pub fn display_status(&self, active_panes: &HashMap<PaneId, bool>) -> DisplayStatus {
        match self.status {
            TaskStatus::Running if self.is_active(active_panes) => DisplayStatus::Active,
            TaskStatus::Running => DisplayStatus::Idle,
            TaskStatus::Completed => DisplayStatus::Completed,
            TaskStatus::Failed => DisplayStatus::Failed,
            TaskStatus::Closed => DisplayStatus::Closed,
        }
    }
}

// ── DisplayStatus (TUI-layer concern, not event-sourced) ─────────────

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
            Self::Active => "r",
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

// ── Non-event-sourced models (kept as-is) ────────────────────────────

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TaskMessage {
    pub id: String,
    pub chat_id: String,
    pub role: MessageRole,
    pub content: String,
    pub created_at: DateTime<Utc>,
}
