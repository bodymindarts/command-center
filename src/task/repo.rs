use es_entity::*;
use sqlx::SqlitePool;

use crate::primitives::{
    ClaudeSessionId, PaneId, ProjectId, TaskId, TaskName, TaskStatus, WindowId,
};

use super::entity::{Task, TaskEvent};

#[derive(EsRepo, Debug)]
#[es_repo(
    entity = "Task",
    tbl = "tasks",
    events_tbl = "task_events",
    columns(
        name = "TaskName",
        skill_name(ty = "String", update(persist = false)),
        params_json(ty = "String", update(persist = false)),
        status(ty = "TaskStatus", create(persist = false)),
        tmux_pane(ty = "Option<PaneId>", create(persist = false)),
        tmux_window(ty = "Option<WindowId>", create(persist = false)),
        work_dir(ty = "Option<String>", update(persist = false)),
        session_id(ty = "Option<ClaudeSessionId>", create(accessor = "session_id_opt()"),),
        started_at(
            ty = "String",
            create(accessor = "started_at()"),
            update(persist = false),
        ),
        completed_at(ty = "Option<String>", create(persist = false),),
        exit_code(ty = "Option<i32>", create(persist = false),),
        output(ty = "Option<String>", create(persist = false),),
        project_id(ty = "Option<ProjectId>", update(persist = false)),
    )
)]
pub struct TaskRepo {
    pool: SqlitePool,
}

impl TaskRepo {
    pub fn new(pool: &SqlitePool) -> Self {
        Self { pool: pool.clone() }
    }

    /// Find a task by ID prefix. Returns None if no match, errors if ambiguous.
    pub async fn find_by_id_prefix(&self, prefix: &str) -> anyhow::Result<Option<Task>> {
        let pattern = format!("{prefix}%");
        let ids = sqlx::query_scalar::<_, String>("SELECT id FROM tasks WHERE id LIKE ?")
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await?;

        if ids.len() > 1 {
            anyhow::bail!("ambiguous prefix '{prefix}': matches {} tasks", ids.len());
        }

        match ids.into_iter().next() {
            Some(id_str) => {
                let id: TaskId = id_str.into();
                let task = self.find_by_id(id).await?;
                Ok(Some(task))
            }
            None => Ok(None),
        }
    }

    /// List all tasks ordered by created_at DESC.
    pub async fn list_all(&self) -> anyhow::Result<Vec<Task>> {
        self.list_with_filter("1=1").await
    }

    /// List tasks with status = 'running'.
    pub async fn list_active(&self) -> anyhow::Result<Vec<Task>> {
        self.list_with_filter("t.status = 'running'").await
    }

    /// List tasks scoped to a project.
    pub async fn list_visible_for_project(
        &self,
        project_id: Option<&ProjectId>,
    ) -> anyhow::Result<Vec<Task>> {
        let sql_base = "SELECT e.id, e.sequence, e.event_type, e.event, e.recorded_at \
                        FROM task_events e \
                        INNER JOIN tasks t ON t.id = e.id";
        let order = "ORDER BY CASE WHEN t.status = 'running' THEN 0 ELSE 1 END, \
                     t.created_at DESC, e.id, e.sequence";

        let rows = match project_id {
            Some(pid) => {
                let pid_str = pid.to_string();
                sqlx::query_as::<_, EventRow>(&format!("{sql_base} WHERE t.project_id = ? {order}"))
                    .bind(&pid_str)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                sqlx::query_as::<_, EventRow>(&format!(
                    "{sql_base} WHERE t.project_id IS NULL {order}"
                ))
                .fetch_all(&self.pool)
                .await?
            }
        };

        let generic: Vec<_> = rows.into_iter().map(|r| r.into_generic()).collect();
        let (tasks, _) = EntityEvents::load_n::<Task>(generic, usize::MAX)?;
        Ok(tasks)
    }

    /// Delete a task and its events (hard delete).
    pub async fn delete_task(&self, id: TaskId) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM task_events WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM tasks WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ── Internal helpers ──

    async fn list_with_filter(&self, filter: &str) -> anyhow::Result<Vec<Task>> {
        let sql = format!(
            "SELECT e.id, e.sequence, e.event_type, e.event, e.recorded_at \
             FROM task_events e \
             INNER JOIN tasks t ON t.id = e.id \
             WHERE {filter} \
             ORDER BY t.created_at DESC, e.id, e.sequence"
        );
        let rows = sqlx::query_as::<_, EventRow>(&sql)
            .fetch_all(&self.pool)
            .await?;
        let generic: Vec<_> = rows.into_iter().map(|r| r.into_generic()).collect();
        let (tasks, _) = EntityEvents::load_n::<Task>(generic, usize::MAX)?;
        Ok(tasks)
    }
}

// Helper struct for mapping sqlx rows to GenericEvent.
#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    sequence: i32,
    #[allow(dead_code)]
    event_type: String,
    event: String,
    recorded_at: String,
}

impl EventRow {
    fn into_generic(self) -> GenericEvent<TaskId> {
        let entity_id: TaskId = self.id.into();
        let event: serde_json::Value =
            serde_json::from_str(&self.event).unwrap_or(serde_json::Value::Null);
        let recorded_at = chrono::DateTime::parse_from_rfc3339(&self.recorded_at)
            .unwrap_or_default()
            .with_timezone(&chrono::Utc);

        GenericEvent {
            entity_id,
            sequence: self.sequence,
            event,
            context: None,
            recorded_at,
        }
    }
}
