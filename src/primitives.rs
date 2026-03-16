use std::fmt;

use serde::{Deserialize, Serialize};

// === UUID entity IDs (SQLite TEXT encoding) ===
//
// Like es_entity::entity_id! but encodes as TEXT in SQLite instead of BLOB.
// This is needed because:
//   - our schema uses TEXT id columns
//   - prefix-match queries (LIKE) require text storage
//   - custom repo methods read ids as String
macro_rules! entity_id {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
                     Serialize, Deserialize)]
            #[serde(transparent)]
            pub struct $name(uuid::Uuid);

            impl $name {
                #[allow(clippy::new_without_default)]
                pub fn new() -> Self {
                    Self(uuid::Uuid::now_v7())
                }
            }

            impl From<uuid::Uuid> for $name {
                fn from(uuid: uuid::Uuid) -> Self { Self(uuid) }
            }

            impl From<&uuid::Uuid> for $name {
                fn from(uuid: &uuid::Uuid) -> Self { Self(*uuid) }
            }

            impl From<String> for $name {
                fn from(s: String) -> Self {
                    s.parse()
                        .unwrap_or_else(|e| panic!("invalid UUID for {}: '{s}': {e}", stringify!($name)))
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    self.0.fmt(f)
                }
            }

            impl std::str::FromStr for $name {
                type Err = uuid::Error;
                fn from_str(s: &str) -> Result<Self, Self::Err> {
                    Ok(Self(uuid::Uuid::parse_str(s)?))
                }
            }

            // -- SQLite-compatible sqlx impls (TEXT, not BLOB) --

            impl sqlx::Type<sqlx::Sqlite> for $name {
                fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
                    <String as sqlx::Type<sqlx::Sqlite>>::type_info()
                }
                fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
                    <String as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
                }
            }

            impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for $name {
                fn encode_by_ref(
                    &self,
                    buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
                ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
                    let s = self.0.to_string();
                    <String as sqlx::Encode<'q, sqlx::Sqlite>>::encode(s, buf)
                }
            }

            impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for $name {
                fn decode(
                    value: <sqlx::Sqlite as sqlx::Database>::ValueRef<'r>,
                ) -> Result<Self, sqlx::error::BoxDynError> {
                    let s = <&str as sqlx::Decode<'r, sqlx::Sqlite>>::decode(value)?;
                    let uuid = uuid::Uuid::parse_str(s)?;
                    Ok(Self(uuid))
                }
            }
        )+
    };
}

entity_id! { TaskId, ProjectId, ClaudeSessionId, WatchId }

impl TaskId {
    pub fn short(&self) -> String {
        let s = self.to_string();
        s[..8.min(s.len())].to_string()
    }
}

impl WatchId {
    pub fn short(&self) -> String {
        let s = self.to_string();
        s[..8.min(s.len())].to_string()
    }
}

// === WatchStatus ===

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum WatchStatus {
    Active,
    Fired,
    Cancelled,
}

impl WatchStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Active => "active",
            Self::Fired => "fired",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

impl fmt::Display for WatchStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for WatchStatus {
    fn from(s: String) -> Self {
        match s.as_str() {
            "active" => Self::Active,
            "fired" => Self::Fired,
            "cancelled" => Self::Cancelled,
            other => {
                tracing::warn!(value = other, "unknown WatchStatus, defaulting to Active");
                Self::Active
            }
        }
    }
}

// === CheckType ===

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum CheckType {
    Timer,
    Command,
}

impl CheckType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Timer => "timer",
            Self::Command => "command",
        }
    }
}

impl fmt::Display for CheckType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for CheckType {
    fn from(s: String) -> Self {
        match s.as_str() {
            "timer" => Self::Timer,
            "command" => Self::Command,
            other => {
                tracing::warn!(value = other, "unknown CheckType, defaulting to Timer");
                Self::Timer
            }
        }
    }
}

// === ActivationSource ===

/// Whether pane activity was triggered organically (user/agent work)
/// or by a watch notification firing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationSource {
    Organic,
    Watch,
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
