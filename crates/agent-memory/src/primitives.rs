use std::fmt;

use serde::{Deserialize, Serialize};

/// UUID-based entity ID for reports (SQLite TEXT encoding).
///
/// Mirrors the `entity_id!` macro from command-center's primitives.rs,
/// encoding as TEXT in SQLite for compatibility with LIKE prefix queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReportId(uuid::Uuid);

impl ReportId {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl From<uuid::Uuid> for ReportId {
    fn from(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }
}

impl From<&uuid::Uuid> for ReportId {
    fn from(uuid: &uuid::Uuid) -> Self {
        Self(*uuid)
    }
}

impl From<String> for ReportId {
    fn from(s: String) -> Self {
        s.parse()
            .unwrap_or_else(|e| panic!("invalid UUID for ReportId: '{s}': {e}"))
    }
}

impl fmt::Display for ReportId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for ReportId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(uuid::Uuid::parse_str(s)?))
    }
}

// -- SQLite-compatible sqlx impls (TEXT, not BLOB) --

impl sqlx::Type<sqlx::Sqlite> for ReportId {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for ReportId {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        let s = self.0.to_string();
        <String as sqlx::Encode<'q, sqlx::Sqlite>>::encode(s, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for ReportId {
    fn decode(
        value: <sqlx::Sqlite as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <&str as sqlx::Decode<'r, sqlx::Sqlite>>::decode(value)?;
        let uuid = uuid::Uuid::parse_str(s)?;
        Ok(Self(uuid))
    }
}
