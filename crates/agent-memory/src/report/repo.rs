use es_entity::*;
use sqlx::SqlitePool;

use crate::primitives::ReportId;

use super::entity::{Report, ReportEvent};

#[derive(EsRepo, Debug)]
#[es_repo(
    entity = "Report",
    tbl = "reports",
    events_tbl = "report_events",
    columns(
        title(ty = "String"),
        project(ty = "Option<String>", list_for(by(created_at))),
        status(ty = "String", create(accessor = "status()"), update(persist = false)),
    )
)]
pub struct ReportRepo {
    pool: SqlitePool,
}

impl ReportRepo {
    pub fn new(pool: &SqlitePool) -> Self {
        Self { pool: pool.clone() }
    }
}
