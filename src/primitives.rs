use std::fmt;

use serde::{Deserialize, Serialize};

// === UUID types (event-sourced entity IDs) ===
//
// entity_id! generates: sqlx::Type (transparent), Debug, Clone, Copy,
// PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
// Display, FromStr, new() (UUID v7), From<Uuid>.
es_entity::entity_id! { TaskId, ProjectId, ClaudeSessionId }

/// Extra impls the entity_id! macro doesn't provide.
macro_rules! impl_id_from_string {
    ($($name:ident),+) => {
        $(
            impl From<String> for $name {
                fn from(s: String) -> Self {
                    s.parse()
                        .unwrap_or_else(|e| panic!("invalid UUID for {}: '{s}': {e}", stringify!($name)))
                }
            }
        )+
    };
}
impl_id_from_string!(TaskId, ProjectId, ClaudeSessionId);

impl TaskId {
    pub fn short(&self) -> String {
        let s = self.to_string();
        s[..8.min(s.len())].to_string()
    }
}

// === String newtypes ===

/// Generates a newtype wrapper around `String` with serde, sqlx, and common trait impls.
macro_rules! string_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                self.0 == *other
            }
        }
    };
}

string_newtype!(TaskName);
string_newtype!(ProjectName);
string_newtype!(PaneId);
string_newtype!(WindowId);

/// Identifies the chat channel a message belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChatId {
    /// ExO-level orchestration chat.
    Exo,
    /// Project-management chat scoped to a project.
    Project(ProjectId),
    /// Per-task agent chat.
    Task(TaskId),
}

impl ChatId {
    pub fn as_db_key(&self) -> String {
        match self {
            Self::Exo => "exo".to_string(),
            Self::Project(pid) => format!("pm:{pid}"),
            Self::Task(tid) => tid.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Completed,
    Failed,
    Closed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for TaskStatus {
    fn from(s: String) -> Self {
        match s.as_str() {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "closed" => Self::Closed,
            other => {
                tracing::warn!(value = other, "unknown TaskStatus, defaulting to Running");
                Self::Running
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

impl MessageRole {
    pub fn as_str(&self) -> &str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

impl fmt::Display for MessageRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for MessageRole {
    fn from(s: String) -> Self {
        match s.as_str() {
            "system" => Self::System,
            "user" => Self::User,
            "assistant" => Self::Assistant,
            other => {
                tracing::warn!(value = other, "unknown MessageRole, defaulting to User");
                Self::User
            }
        }
    }
}
