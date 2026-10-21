use std::io;

#[derive(Debug, thiserror::Error)]
pub enum DupeError {
    #[error("abgebrochen")]
    Cancelled,
    #[error("{0}: {1}")]
    Io(String, #[source] io::Error),
    #[error("{0}")]
    Other(String),
    #[error("Cache: {0}")]
    Db(#[from] rusqlite::Error),
}

impl DupeError {
    pub fn io(what: impl ToString, e: io::Error) -> Self {
        Self::Io(what.to_string(), e)
    }
}

pub type Result<T> = std::result::Result<T, DupeError>;
