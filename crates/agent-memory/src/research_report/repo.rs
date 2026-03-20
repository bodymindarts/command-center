use es_entity::*;
use sqlx::SqlitePool;

use crate::primitives::ResearchReportId;

use super::entity::{ResearchReport, ResearchReportEvent};

#[derive(EsRepo, Debug)]
#[es_repo(
    entity = "ResearchReport",
    tbl = "research_reports",
    events_tbl = "research_report_events",
    columns(
        title(ty = "String"),
        project(ty = "Option<String>", list_for(by(created_at))),
        status(ty = "String", create(accessor = "status()"), update(persist = false)),
    )
)]
pub struct ResearchReportRepo {
    pool: SqlitePool,
}

impl ResearchReportRepo {
    pub fn new(pool: &SqlitePool) -> Self {
        Self { pool: pool.clone() }
    }
}
