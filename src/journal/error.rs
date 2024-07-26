use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
#[non_exhaustive]
pub enum JournalError {
    #[error("Duplicate entry")]
    DuplicateEntry,

    #[error("SystemTime error: {0}")]
    SystemTime(#[from] std::time::SystemTimeError),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("JSON (De)Serialization error: {0}")]
    JsonSerialization(#[from] serde_json::Error),

    #[error("I/O error: {0}")]
    IO(#[from] std::io::Error),
}
