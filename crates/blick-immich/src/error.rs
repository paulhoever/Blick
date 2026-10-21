pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid server address: {0}")]
    BadUrl(String),
    #[error("API key rejected")]
    Unauthorized,
    #[error("Server responded with {status}: {message}")]
    Api { status: u16, message: String },
    #[error("No connection to the server: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Unerwartete Antwort: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("{path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}
