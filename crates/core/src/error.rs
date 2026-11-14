//! Error type shared by the core modules.

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed {what}: {detail}")]
    Malformed { what: &'static str, detail: String },
    #[error("database schema {found} is not supported (expected {expected})")]
    SchemaVersion { found: i64, expected: i64 },
}

pub type Result<T> = std::result::Result<T, CoreError>;

pub(crate) fn malformed(what: &'static str, detail: impl Into<String>) -> CoreError {
    CoreError::Malformed {
        what,
        detail: detail.into(),
    }
}
