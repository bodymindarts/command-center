use es_entity::*;
use sqlx::SqlitePool;

use crate::primitives::{ProjectId, TaskId, TaskName, TaskStatus};

use super::entity::{Task, TaskEvent};

#[derive(EsRepo, Debug)]
#[es_repo(
    entity = "Task",
    tbl = "tasks",
    events_tbl = "task_events",
    delete = "soft",
    columns(
        name = "TaskName",
        status(ty = "TaskStatus", create(persist = false)),
        // NOT NULL in schema — must be persisted on create, but not used for lookup.
        skill_name(ty = "String", update(persist = false)),
        params_json(ty = "String", update(persist = false)),
        started_at(
            ty = "String",
            create(accessor = "started_at()"),
            update(persist = false),
        ),
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
        let (tasks, _) = es_query!(
            "SELECT id FROM tasks WHERE id LIKE $1 AND deleted = FALSE LIMIT 2",
            pattern as &str,
        )
        .fetch_n(self.pool(), 2)
        .await?;

        if tasks.len() > 1 {
            anyhow::bail!("ambiguous prefix '{prefix}': matches {} tasks", tasks.len());
        }

        Ok(tasks.into_iter().next())
    }

    /// List all tasks ordered by created_at DESC.
    pub async fn list_all(&self) -> anyhow::Result<Vec<Task>> {
        let (tasks, _) = es_query!(
            "SELECT id, created_at FROM tasks WHERE deleted = FALSE ORDER BY created_at DESC",
        )
        .fetch_n(self.pool(), usize::MAX)
        .await?;
        Ok(tasks)
    }

    /// List tasks with status = 'running'.
    pub async fn list_active(&self) -> anyhow::Result<Vec<Task>> {
        let status = "running";
        let (tasks, _) = es_query!(
            "SELECT id, created_at FROM tasks WHERE status = $1 AND deleted = FALSE ORDER BY created_at DESC",
            status as &str,
        )
        .fetch_n(self.pool(), usize::MAX)
        .await?;
        Ok(tasks)
    }

    /// List tasks scoped to a project (running tasks sorted first).
    pub async fn list_visible_for_project(
        &self,
        project_id: Option<&ProjectId>,
    ) -> anyhow::Result<Vec<Task>> {
        let (tasks, _) = match project_id {
            Some(pid) => {
                es_query!(
                    "SELECT id, created_at, CASE WHEN status = 'running' THEN 0 ELSE 1 END AS sort_priority \
                     FROM tasks \
                     WHERE project_id = $1 AND deleted = FALSE \
                     ORDER BY sort_priority, created_at DESC",
                    pid as &ProjectId,
                )
                .fetch_n(self.pool(), usize::MAX)
                .await?
            }
            None => {
                es_query!(
                    "SELECT id, created_at, CASE WHEN status = 'running' THEN 0 ELSE 1 END AS sort_priority \
                     FROM tasks \
                     WHERE project_id IS NULL AND deleted = FALSE \
                     ORDER BY sort_priority, created_at DESC",
                )
                .fetch_n(self.pool(), usize::MAX)
                .await?
            }
        };
        Ok(tasks)
    }
}
