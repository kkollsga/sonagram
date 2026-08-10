//! Error type for sonagram.
//!
//! sonagram owns the mapping and schema; mapping/schema failures are ours.
//! An error that originates in analysis surfaces as [`SonagramError::Audio`]
//! (from `sonara`), and one that originates in the graph engine as
//! [`SonagramError::Graph`]. Both upstreams are read-only from here: an
//! upstream defect is reproduced and routed to the project that owns it, never
//! absorbed silently behind a mapping-layer workaround.

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

    /// A playlist-materialization failure (empty track set, unresolvable track
    /// ids, or a Cypher result with no usable Track id column). Ours: the
    /// mapping from a graph answer to a `.m3u8` is sonagram's responsibility.
    #[error("playlist error: {0}")]
    Playlist(String),

    /// A Last.fm enrichment failure that aborts the whole run — a missing API
    /// key, or an unwritable `.sonagram/lastfm/` cache. A per-entity fetch
    /// failure never surfaces here: it is soft-failed into the entity's cache
    /// record (`failed: true`) so one bad artist/track/album never aborts the
    /// run (see [`crate::enrich`]).
    #[error("enrich error: {0}")]
    Enrich(String),

    /// A user-config failure (P17): the `~/.sonagram/config.json` registry could
    /// not be read/written, a source directory does not exist or is not a
    /// directory, or the home dir could not be resolved. Ours: the config format
    /// and the source registry are sonagram's responsibility.
    #[error("config error: {0}")]
    Config(String),
}

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, SonagramError>;
