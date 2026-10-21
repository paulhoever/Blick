//! Duplicate search for Blick, modeled on dupeGuru, but with a cache and
//! an index instead of pairwise comparison. Synchronous, no GTK.

pub mod error;
#[cfg(feature = "gio")]
pub mod gio_storage;
pub mod hashdb;
pub mod immich;
pub mod results;
pub mod scan;
pub mod sig;
pub mod source;

pub use error::{DupeError, Result};
pub use scan::{Group, Member, Mode, Options, Progress, Scanner, SourceSpec, Stage};
pub use source::{Entry, LocalStorage, Storage};
