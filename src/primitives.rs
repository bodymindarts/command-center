use std::fmt;

/// Generates a newtype wrapper around `String` with common trait impls:
/// `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`, `Display`, `From<String>`,
/// `PartialEq<str>`, `PartialEq<String>`, and an `as_str()` accessor.
macro_rules! string_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

/// Generates a newtype wrapper around `uuid::Uuid` with a cached string
/// representation. Provides the same API surface as `string_newtype!`
/// (`as_str()`, `Display`, `From<String>`, equality) but stores a real UUID.
macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(Clone)]
        pub struct $name {
            id: uuid::Uuid,
            repr: String,
        }

        impl $name {
            pub fn generate() -> Self {
                let id = uuid::Uuid::now_v7();
                Self {
                    repr: id.to_string(),
                    id,
                }
            }

            pub fn as_str(&self) -> &str {
                &self.repr
            }

            pub fn short(&self) -> &str {
                &self.repr[..8.min(self.repr.len())]
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.id).finish()
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }

        impl Eq for $name {}

        impl std::hash::Hash for $name {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.id.hash(state);
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.repr)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                let id = uuid::Uuid::parse_str(&s).unwrap_or_else(|e| {
                    panic!("invalid UUID for {}: '{}': {e}", stringify!($name), s)
                });
                Self { id, repr: s }
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.repr == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.repr == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                self.repr == *other
            }
        }
    };
}

id_newtype!(TaskId);
string_newtype!(TaskName);
id_newtype!(ProjectId);
string_newtype!(ProjectName);
string_newtype!(PaneId);
string_newtype!(WindowId);

/// Identifies the chat channel a message belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatId {
    /// ExO-level orchestration chat.
    Exo,
    /// Project-management chat scoped to a project.
    Pm(ProjectId),
    /// Per-task agent chat.
    Task(TaskId),
}

impl ChatId {
    pub fn as_db_key(&self) -> String {
        match self {
            Self::Exo => "exo".to_string(),
            Self::Pm(pid) => format!("pm:{}", pid.as_str()),
            Self::Task(tid) => tid.as_str().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
