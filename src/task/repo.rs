use es_entity::*;
use sqlx::SqlitePool;

use crate::primitives::{ProjectId, TaskId, TaskName, TaskStatus};

use super::entity::{Task, TaskEvent};

/// Fetch-all page size — large enough to never paginate, small enough to avoid overflow.
const ALL: usize = i64::MAX as usize;

#[derive(EsRepo, Debug)]
#[es_repo(
    entity = "Task",
    tbl = "tasks",
    events_tbl = "task_events",
    delete = "soft",
    columns(
        name = "TaskName",
        status(ty = "TaskStatus", create(persist = false), list_for(by(created_at))),
        started_at(
            ty = "String",
            create(accessor = "started_at()"),
            update(persist = false),
        ),
        project_id(
            ty = "Option<ProjectId>",
            update(persist = false),
            list_for(by(created_at))
        ),
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
        let ret = self
            .list_by_created_at(
                PaginatedQueryArgs {
                    first: ALL,
                    after: None,
                },
                ListDirection::Descending,
            )
            .await?;
        Ok(ret.entities)
    }

    /// List tasks with status = 'running'.
    pub async fn list_active(&self) -> anyhow::Result<Vec<Task>> {
        let ret = self
            .list_for_status_by_created_at(
                TaskStatus::Running,
                PaginatedQueryArgs {
                    first: ALL,
                    after: None,
                },
                ListDirection::Descending,
            )
            .await?;
        Ok(ret.entities)
    }

    /// List tasks scoped to a project (running tasks sorted first).
    ///
    /// `None` matches tasks with no project (IS NULL).
    pub async fn list_visible_for_project(
        &self,
        project_id: Option<&ProjectId>,
    ) -> anyhow::Result<Vec<Task>> {
        let mut tasks = self
            .list_for_project_id_by_created_at(
                project_id.copied(),
                PaginatedQueryArgs {
                    first: ALL,
                    after: None,
                },
                ListDirection::Descending,
            )
            .await?
            .entities;

        // Sort running tasks before non-running (stable sort preserves created_at order).
        tasks.sort_by_key(|t| if t.status.is_running() { 0 } else { 1 });
        Ok(tasks)
    }
}
