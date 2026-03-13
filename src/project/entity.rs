use chrono::{DateTime, Utc};
use derive_builder::Builder;
use es_entity::*;
use serde::{Deserialize, Serialize};

use crate::primitives::{ProjectId, ProjectName};

// ── Events ───────────────────────────────────────────────────────────

#[derive(EsEvent, Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[es_event(id = "ProjectId")]
pub enum ProjectEvent {
    Initialized {
        id: ProjectId,
        name: ProjectName,
        description: String,
    },
    Deleted,
}

// ── Entity ───────────────────────────────────────────────────────────

#[derive(EsEntity, Builder, Clone)]
#[builder(pattern = "owned", build_fn(error = "EntityHydrationError"))]
pub struct Project {
    pub id: ProjectId,
    pub name: ProjectName,
    pub description: String,
    #[builder(default)]
    pub created_at: DateTime<Utc>,
    events: EntityEvents<ProjectEvent>,
}

impl std::fmt::Debug for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Project: {} ({})", self.name, self.id)
    }
}

// ── Hydration ────────────────────────────────────────────────────────

impl TryFromEvents<ProjectEvent> for Project {
    fn try_from_events(events: EntityEvents<ProjectEvent>) -> Result<Self, EntityHydrationError> {
        let mut builder = ProjectBuilder::default();

        if let Some(first) = events.entity_first_persisted_at() {
            builder = builder.created_at(first);
        }

        for event in events.iter_all() {
            match event {
                ProjectEvent::Initialized {
                    id,
                    name,
                    description,
                } => {
                    builder = builder
                        .id(*id)
                        .name(name.clone())
                        .description(description.clone());
                }
                ProjectEvent::Deleted => {
                    // Soft delete — repo-level flag; no entity state change.
                }
            }
        }

        builder.events(events).build()
    }
}

// ── NewProject (construction) ────────────────────────────────────────

pub struct NewProject {
    pub id: ProjectId,
    pub name: ProjectName,
    pub description: String,
}

impl IntoEvents<ProjectEvent> for NewProject {
    fn into_events(self) -> EntityEvents<ProjectEvent> {
        EntityEvents::init(
            self.id,
            [ProjectEvent::Initialized {
                id: self.id,
                name: self.name,
                description: self.description,
            }],
        )
    }
}

// ── Mutations ────────────────────────────────────────────────────────

impl Project {
    pub fn delete(&mut self) -> Idempotent<()> {
        idempotency_guard!(
            self.events.iter_all(),
            already_applied: ProjectEvent::Deleted
        );
        self.events.push(ProjectEvent::Deleted);
        Idempotent::Executed(())
    }
}
