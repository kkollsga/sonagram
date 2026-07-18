//! Deterministic, library-owned playlist curation.
//!
//! Agents translate user intent into [`PlaylistBrief`] / [`PlaylistPolicy`].
//! Selection, sequencing, auditing, and explanation remain library behaviour.

mod audit;
mod profile;
mod project;
mod select;
mod sequence;
mod types;

pub use audit::{
    audit_playlist, audit_playlist_for_brief, explain_playlist, explain_playlist_for_brief,
};
pub use profile::profile_library;
pub use select::curate_playlist;
pub use types::*;
