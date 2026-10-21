//! Image processing for Blick. Knows nothing about GTK or an event loop:
//! everything here is synchronous and called by the app from worker threads.

pub mod adjust;
pub mod batch;
pub mod cache;
pub mod crop;
pub mod decode;
pub mod encode;
pub mod error;
pub mod exif;
pub mod export;
pub mod formats;
pub mod info;
pub mod library;
pub mod look;
pub mod raw;
pub mod rotate;
pub mod sidecar;

pub use adjust::Adjustments;
pub use crop::Crop;
pub use decode::Pixels;
pub use error::{CoreError, Result};
pub use rotate::Rotation;
