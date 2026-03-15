use es_entity::*;
use sqlx::SqlitePool;

use crate::primitives::{TaskId, WatchId, WatchStatus};

use super::entity::{Watch, WatchEvent};

/// Fetch-all page size — large enough to never paginate, small enough to avoid overflow.
const ALL: usize = i64::MAX as usize;

#[derive(EsRepo, Debug)]
#[es_repo(
    entity = "Watch",
    tbl = "watches",
    events_tbl = "watch_events",
    columns(
        task_id(ty = "TaskId", update(persist = false), list_for(by(created_at))),
        name(ty = "String", update(persist = false)),
        status(
            ty = "WatchStatus",
            create(accessor = "status()"),
            list_for(by(created_at)),
        ),
        job_id(ty = "String"),
    )
)]
pub struct WatchRepo {
    pool: SqlitePool,
}

impl WatchRepo {
    pub fn new(pool: &SqlitePool) -> Self {
        Self { pool: pool.clone() }
    }

    /// Find the active watch with a given (task_id, name) pair.
    /// Returns None if no active watch with that name exists.
    pub async fn find_active_by_task_and_name(
        &self,
        task_id: TaskId,
        name: &str,
    ) -> anyhow::Result<Option<Watch>> {
        let status = WatchStatus::Active;
        let (watches, _) = es_query!(
            "SELECT id FROM watches WHERE task_id = $1 AND name = $2 AND status = $3 LIMIT 1",
            task_id as TaskId,
            name as &str,
            status as WatchStatus,
        )
        .fetch_n(self.pool(), 1)
        .await?;

        Ok(watches.into_iter().next())
    }

    /// List all active watches for a given task.
    pub async fn list_active_for_task(&self, task_id: TaskId) -> anyhow::Result<Vec<Watch>> {
        let status = WatchStatus::Active;
        let ret = es_query!(
            "SELECT id FROM watches WHERE task_id = $1 AND status = $2 ORDER BY created_at ASC",
            task_id as TaskId,
            status as WatchStatus,
        )
        .fetch_n(self.pool(), ALL)
        .await?;

        Ok(ret.0)
    }

    /// Find a watch by ID prefix. Returns None if no match, errors if ambiguous.
    pub async fn maybe_find_by_id_prefix(&self, prefix: &str) -> anyhow::Result<Option<Watch>> {
        let pattern = format!("{prefix}%");
        let (watches, _) = es_query!(
            "SELECT id FROM watches WHERE id LIKE $1 LIMIT 2",
            pattern as &str,
        )
        .fetch_n(self.pool(), 2)
        .await?;

        if watches.len() > 1 {
            anyhow::bail!(
                "ambiguous prefix '{prefix}': matches {} watches",
                watches.len()
            );
        }

        Ok(watches.into_iter().next())
    }
}
