pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Server-Adresse ungültig: {0}")]
    BadUrl(String),
    #[error("API-Schlüssel abgelehnt")]
    Unauthorized,
    #[error("Server antwortet mit {status}: {message}")]
    Api { status: u16, message: String },
    #[error("Keine Verbindung zum Server: {0}")]
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
