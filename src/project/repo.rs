use es_entity::*;
use sqlx::SqlitePool;

use crate::primitives::{ProjectId, ProjectName};

use super::entity::{Project, ProjectEvent};

/// Fetch-all page size — large enough to never paginate, small enough to avoid overflow.
const ALL: usize = i64::MAX as usize;

#[derive(EsRepo, Debug)]
#[es_repo(
    entity = "Project",
    tbl = "projects",
    events_tbl = "project_events",
    delete = "soft",
    columns(name = "ProjectName",)
)]
pub struct ProjectRepo {
    pool: SqlitePool,
}

impl ProjectRepo {
    pub fn new(pool: &SqlitePool) -> Self {
        Self { pool: pool.clone() }
    }

    /// List all projects ordered by created_at ASC.
    pub async fn list_all(&self) -> anyhow::Result<Vec<Project>> {
        let ret = self
            .list_by_created_at(
                PaginatedQueryArgs {
                    first: ALL,
                    after: None,
                },
                ListDirection::Ascending,
            )
            .await?;
        Ok(ret.entities)
    }
}
