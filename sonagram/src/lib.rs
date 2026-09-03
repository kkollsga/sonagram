//! # sonagram
//!
//! sonagram scans a music library and structures it into a queryable
//! [kglite](https://github.com/kkollsga/kglite) knowledge graph, so AI agents
//! can interact with a music collection directly through the graph.
//!
//! It is a **graph builder over two upstreams**: `sonara` supplies per-track
//! analysis (typed `TrackAnalysis`), `kglite` supplies the graph engine.
//! sonagram owns the **mapping and the schema** between them and nothing else:
//! analysis stays in `sonara`, and storage, Cypher and embeddings stay in
//! `kglite`. A fix that wants to live upstream is routed there rather than
//! growing a shadow implementation here.
//!
//! ## Module map
//! The modules below are stubs filled in as the phased build lands:
//! - [`record`] — serde DTO mirroring `sonara`'s `TrackAnalysis` (P2).
//! - [`scan`] — library walk, content hashing, cache, incremental rescan (P3).
//! - [`graph`] — records → `kglite` `DirGraph`, per the music schema (P4).
//! - [`playlist`] — M3U export from a track set (P7).

pub mod error;

pub mod cli;

pub mod config;

pub mod record;

pub mod scan;

pub mod graph;

pub mod enrich;

pub mod pipeline;

pub mod playlist;

pub mod curation;

pub mod progress;

pub mod skill;

pub mod mcp;

pub mod mcp_server;

pub use error::{Result, SonagramError};

/// The sonagram crate version, sourced from `Cargo.toml` at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Serializes tests that mutate or depend on process-global env vars
/// (`SONAGRAM_HOME`, `LASTFM_API_KEY`), which cargo otherwise runs in parallel.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_pinned() {
        // The lockstep version fields (Cargo.toml workspace, pyproject.toml,
        // this constant) must all agree on the release version.
        assert_eq!(VERSION, "0.2.16");
    }
}
