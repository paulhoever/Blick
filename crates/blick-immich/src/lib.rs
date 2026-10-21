//! Immich-Client für Blick. Spricht die REST-API ab Immich 2.x/3.x
//! (geprüft gegen die `spec.json` von 3.3.1) und meldet sich per API-Schlüssel an.

mod client;
mod error;
pub mod models;
mod time;

pub use client::{Client, ThumbSize};
pub use error::{Error, Result};
pub use models::{
    Album, BulkIdResponse, RemoteAsset, SearchAsset, TimeBucket, UploadResponse, UploadStatus, User,
};
