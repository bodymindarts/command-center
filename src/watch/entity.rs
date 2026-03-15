use chrono::{DateTime, Utc};
use derive_builder::Builder;
use es_entity::*;
use serde::{Deserialize, Serialize};

use crate::primitives::{WatchId, WatchStatus};

use super::error::WatchError;

// ── Events ───────────────────────────────────────────────────────────

#[derive(EsEvent, Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[es_event(id = "WatchId")]
pub enum WatchEvent {
    Initialized {
        id: WatchId,
        task_id: String,
        name: Option<String>,
        label: String,
        job_id: String,
        check_type: String,
        fires_at: String,
    },
    Fired,
    Cancelled {
        reason: String,
    },
    Replaced {
        replaced_by: WatchId,
    },
}

// ── Entity ───────────────────────────────────────────────────────────

#[derive(EsEntity, Builder, Clone)]
#[builder(pattern = "owned", build_fn(error = "EntityHydrationError"))]
pub struct Watch {
    pub id: WatchId,
    pub task_id: String,
    pub name: Option<String>,
    pub label: String,
    pub job_id: String,
    pub check_type: String,
    pub fires_at: DateTime<Utc>,
    pub status: WatchStatus,
    #[builder(default)]
    #[allow(dead_code)]
    pub created_at: DateTime<Utc>,
    events: EntityEvents<WatchEvent>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("id", &self.id)
            .field("task_id", &self.task_id)
            .field("name", &self.name)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Watch: {} ({})", self.label, self.id)
    }
}

// ── Hydration ────────────────────────────────────────────────────────

impl TryFromEvents<WatchEvent> for Watch {
    fn try_from_events(events: EntityEvents<WatchEvent>) -> Result<Self, EntityHydrationError> {
        let mut builder = WatchBuilder::default();

        if let Some(first) = events.entity_first_persisted_at() {
            builder = builder.created_at(first);
        }

        for event in events.iter_all() {
            match event {
                WatchEvent::Initialized {
                    id,
                    task_id,
                    name,
                    label,
                    job_id,
                    check_type,
                    fires_at,
                } => {
                    let fires_at_dt = DateTime::parse_from_rfc3339(fires_at)
                        .unwrap_or_default()
                        .with_timezone(&Utc);
                    builder = builder
                        .id(*id)
                        .task_id(task_id.clone())
                        .name(name.clone())
                        .label(label.clone())
                        .job_id(job_id.clone())
                        .check_type(check_type.clone())
                        .fires_at(fires_at_dt)
                        .status(WatchStatus::Active);
                }
                WatchEvent::Fired => {
                    builder = builder.status(WatchStatus::Fired);
                }
                WatchEvent::Cancelled { .. } => {
                    builder = builder.status(WatchStatus::Cancelled);
                }
                WatchEvent::Replaced { .. } => {
                    builder = builder.status(WatchStatus::Replaced);
                }
            }
        }

        builder.events(events).build()
    }
}

// ── NewWatch (construction) ──────────────────────────────────────────

pub struct NewWatch {
    pub id: WatchId,
    pub task_id: String,
    pub name: Option<String>,
    pub label: String,
    pub job_id: String,
    pub check_type: String,
    pub fires_at: DateTime<Utc>,
}

impl NewWatch {
    /// Used by EsRepo column accessor for the `status` column.
    pub(super) fn status(&self) -> String {
        WatchStatus::Active.as_str().to_string()
    }
}

impl IntoEvents<WatchEvent> for NewWatch {
    fn into_events(self) -> EntityEvents<WatchEvent> {
        EntityEvents::init(
            self.id,
            [WatchEvent::Initialized {
                id: self.id,
                task_id: self.task_id,
                name: self.name,
                label: self.label,
                job_id: self.job_id,
                check_type: self.check_type,
                fires_at: self.fires_at.to_rfc3339(),
            }],
        )
    }
}

// ── Mutations ────────────────────────────────────────────────────────

impl Watch {
    pub fn fire(&mut self) -> Result<Idempotent<()>, WatchError> {
        if !self.status.is_active() {
            return Err(WatchError::NotActive);
        }
        self.status = WatchStatus::Fired;
        self.events.push(WatchEvent::Fired);
        Ok(Idempotent::Executed(()))
    }

    pub fn cancel(&mut self, reason: &str) -> Idempotent<()> {
        if !self.status.is_active() {
            return Idempotent::AlreadyApplied;
        }
        self.status = WatchStatus::Cancelled;
        self.events.push(WatchEvent::Cancelled {
            reason: reason.to_string(),
        });
        Idempotent::Executed(())
    }

    pub fn replace(&mut self, replaced_by: WatchId) -> Idempotent<()> {
        if !self.status.is_active() {
            return Idempotent::AlreadyApplied;
        }
        self.status = WatchStatus::Replaced;
        self.events.push(WatchEvent::Replaced { replaced_by });
        Idempotent::Executed(())
    }
}
