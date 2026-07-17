//! Deterministic, library-owned playlist curation.
//!
//! Agents translate user intent into [`PlaylistBrief`] / [`PlaylistPolicy`].
//! Selection, sequencing, auditing, and explanation remain library behaviour.

mod audit;
mod profile;
mod project;
mod types;

pub use audit::{audit_playlist, explain_playlist};
pub use profile::profile_library;
pub use types::*;
