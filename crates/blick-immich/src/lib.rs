//! Immich client for Blick. Talks to the REST API of Immich 2.x/3.x
//! (checked against the `spec.json` of 3.3.1) and signs in with an API key.

mod client;
mod error;
pub mod models;
mod time;

pub use client::{Client, ThumbSize};
pub use error::{Error, Result};
pub use models::{
    Album, BulkIdResponse, RemoteAsset, SearchAsset, TimeBucket, UploadResponse, UploadStatus, User,
};
