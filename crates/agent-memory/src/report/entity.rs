use chrono::{DateTime, Utc};
use derive_builder::Builder;
use es_entity::*;
use serde::{Deserialize, Serialize};

use crate::primitives::ReportId;

// ── Events ───────────────────────────────────────────────────────────

#[derive(EsEvent, Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[es_event(id = "ReportId")]
pub enum ReportEvent {
    Initialized {
        id: ReportId,
        title: String,
        content: String,
        tags: Vec<String>,
        project: Option<String>,
        source_task: Option<String>,
    },
    Updated {
        title: Option<String>,
        content: Option<String>,
        tags: Option<Vec<String>>,
    },
    Superseded {
        superseded_by: ReportId,
    },
}

// ── Entity ───────────────────────────────────────────────────────────

#[derive(EsEntity, Builder, Clone)]
#[builder(pattern = "owned", build_fn(error = "EntityHydrationError"))]
pub struct Report {
    pub id: ReportId,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub source_task: Option<String>,
    pub status: String,
    #[builder(default)]
    pub created_at: DateTime<Utc>,
    #[builder(default)]
    pub superseded_by: Option<ReportId>,
    events: EntityEvents<ReportEvent>,
}

impl std::fmt::Debug for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Report")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("status", &self.status)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Report: {} ({})", self.title, self.id)
    }
}

// ── Hydration ────────────────────────────────────────────────────────

impl TryFromEvents<ReportEvent> for Report {
    fn try_from_events(events: EntityEvents<ReportEvent>) -> Result<Self, EntityHydrationError> {
        let mut builder = ReportBuilder::default();

        if let Some(first) = events.entity_first_persisted_at() {
            builder = builder.created_at(first);
        }

        for event in events.iter_all() {
            match event {
                ReportEvent::Initialized {
                    id,
                    title,
                    content,
                    tags,
                    project,
                    source_task,
                } => {
                    builder = builder
                        .id(*id)
                        .title(title.clone())
                        .content(content.clone())
                        .tags(tags.clone())
                        .project(project.clone())
                        .source_task(source_task.clone())
                        .status("active".to_string());
                }
                ReportEvent::Updated {
                    title,
                    content,
                    tags,
                } => {
                    if let Some(t) = title {
                        builder = builder.title(t.clone());
                    }
                    if let Some(c) = content {
                        builder = builder.content(c.clone());
                    }
                    if let Some(tg) = tags {
                        builder = builder.tags(tg.clone());
                    }
                }
                ReportEvent::Superseded { superseded_by } => {
                    builder = builder
                        .status("superseded".to_string())
                        .superseded_by(Some(*superseded_by));
                }
            }
        }

        builder.events(events).build()
    }
}

// ── NewReport (construction) ─────────────────────────────────────────

pub struct NewReport {
    pub id: ReportId,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub source_task: Option<String>,
}

impl NewReport {
    /// Used by EsRepo column accessor for the `status` column.
    pub(super) fn status(&self) -> &str {
        "active"
    }
}

impl IntoEvents<ReportEvent> for NewReport {
    fn into_events(self) -> EntityEvents<ReportEvent> {
        EntityEvents::init(
            self.id,
            [ReportEvent::Initialized {
                id: self.id,
                title: self.title,
                content: self.content,
                tags: self.tags,
                project: self.project,
                source_task: self.source_task,
            }],
        )
    }
}

// ── Mutations ────────────────────────────────────────────────────────

/// Update payload for a report.
pub struct ReportUpdate {
    pub title: Option<String>,
    pub content: Option<String>,
    pub tags: Option<Vec<String>>,
}

impl Report {
    pub fn update(&mut self, update: ReportUpdate) -> Idempotent<()> {
        if update.title.is_none() && update.content.is_none() && update.tags.is_none() {
            return Idempotent::AlreadyApplied;
        }

        if let Some(ref t) = update.title {
            self.title = t.clone();
        }
        if let Some(ref c) = update.content {
            self.content = c.clone();
        }
        if let Some(ref tg) = update.tags {
            self.tags = tg.clone();
        }

        self.events.push(ReportEvent::Updated {
            title: update.title,
            content: update.content,
            tags: update.tags,
        });
        Idempotent::Executed(())
    }

    pub fn supersede(&mut self, superseded_by: ReportId) -> Idempotent<()> {
        idempotency_guard!(
            self.events.iter_all().rev(),
            already_applied: ReportEvent::Superseded { superseded_by: sb }
                if *sb == superseded_by
        );

        self.status = "superseded".to_string();
        self.superseded_by = Some(superseded_by);
        self.events.push(ReportEvent::Superseded { superseded_by });
        Idempotent::Executed(())
    }
}
