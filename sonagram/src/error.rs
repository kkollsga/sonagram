//! Error type for sonagram.
//!
//! sonagram owns the mapping and schema; mapping/schema failures are ours.
//! An error that originates in analysis surfaces as [`SonagramError::Audio`]
//! (from `sonara`), and one that originates in the graph engine as
//! [`SonagramError::Graph`]. Per `dev-docs/designs/upstream-contracts.md`,
//! upstream defects are reproduced and routed, not absorbed silently.

use thiserror::Error;

/// The crate-wide error type.
#[derive(Debug, Error)]
pub enum SonagramError {
    /// Filesystem / IO failure while scanning a library or reading a cache.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// An analysis failure surfaced from the `sonara` upstream.
    #[error("audio analysis error: {0}")]
    Audio(#[from] sonara::SonaraError),

    /// A graph mapping / storage failure (schema, node identity, `.kgl` write).
    #[error("graph error: {0}")]
    Graph(String),

    /// A scan-cache failure (per-hash JSON cache under `<lib>/.sonagram/`).
    #[error("cache error: {0}")]
    Cache(String),
}

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, SonagramError>;
