//! # sonagram
//!
//! sonagram scans a music library and structures it into a queryable
//! [kglite](https://github.com/kkollsga/kglite) knowledge graph, so AI agents
//! can interact with a music collection directly through the graph.
//!
//! It is a **graph builder over two upstreams**: `sonara` supplies per-track
//! analysis (typed `TrackAnalysis`), `kglite` supplies the graph engine.
//! sonagram owns the **mapping and the schema** between them and nothing else
//! (see `dev-docs/designs/upstream-contracts.md`).
//!
//! ## Module map
//! The modules below are stubs filled in as the phased build lands:
//! - [`record`] — serde DTO mirroring `sonara`'s `TrackAnalysis` (P2).
//! - [`scan`] — library walk, content hashing, cache, incremental rescan (P3).
//! - [`graph`] — records → `kglite` `DirGraph`, per the music schema (P4).
//! - [`playlist`] — M3U export from a track set (P7).

pub mod error;

pub mod cli;

pub mod record;

pub mod scan;

pub mod graph;

pub mod enrich;

pub mod playlist;

pub use error::{Result, SonagramError};

/// The sonagram crate version, sourced from `Cargo.toml` at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_pinned() {
        // The three lockstep version fields must all read 0.1.0 at bootstrap.
        assert_eq!(VERSION, "0.1.0");
    }
}
