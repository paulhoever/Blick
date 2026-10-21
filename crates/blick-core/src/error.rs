use std::path::PathBuf;

pub type Result<T, E = CoreError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Image {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
    #[error("{path}: Metadaten nicht lesbar ({reason})")]
    Metadata { path: PathBuf, reason: String },
    #[error("{path}: lässt sich nicht dekodieren ({reason})")]
    Decode { path: PathBuf, reason: String },
    #[error("Skalierung fehlgeschlagen: {0}")]
    Resize(String),
    #[error("{0}: Format wird nicht unterstützt")]
    Unsupported(PathBuf),
    #[error("abgebrochen")]
    Cancelled,
}

impl CoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn image(path: impl Into<PathBuf>, source: image::ImageError) -> Self {
        Self::Image {
            path: path.into(),
            source,
        }
    }
}
