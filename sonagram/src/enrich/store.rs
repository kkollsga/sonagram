//! On-disk Last.fm enrichment cache under `<library>/.sonagram/lastfm/`.
//!
//! Three artifacts live here, each a versioned map written deterministically:
//!
//! - `artists.json` — `BTreeMap<artist_id, ArtistEnrich>` keyed by the graph's
//!   normalized artist name (`graph::normalize::artist_id`).
//! - `tracks.json` — `BTreeMap<content_hash, TrackEnrich>` keyed by the track's
//!   audio content hash, so it joins directly to `Track` nodes.
//! - `albums.json` — `BTreeMap<"artist_id|album", AlbumEnrich>` keyed exactly as
//!   `graph::normalize::album_id` keys `Album` nodes.
//!
//! `BTreeMap` (never `HashMap`) so the serialized bytes are sorted and
//! reproducible. Each file wraps its map in an [`EnrichFile`] carrying
//! [`ENRICH_VERSION`], so a format bump can be detected. Writes are **atomic**
//! (temp sibling + rename), so an interrupted enrichment run never leaves a
//! half-written cache — and the driver saves every N entities, making a long run
//! resumable.
//!
//! ## `fetched` / `failed` = negative cache
//! Every record carries `fetched: true` once the API was queried. A record with
//! `failed: true` (plus a `reason`) is a *soft failure* — the entity was queried
//! but returned nothing usable — and is **still** skipped on re-run, so a run
//! never re-hammers Last.fm for a permanently-missing artist. Clearing the cache
//! file forces a re-fetch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Result, SonagramError};

/// Version of the enrichment cache format. Bump when a record layout changes in
/// a way that invalidates previously written `lastfm/*.json`.
pub const ENRICH_VERSION: u32 = 1;

/// A single similar track from `track.getSimilar`, **keeping the match weight**.
/// The prototype this layer replaced dropped the weight at store time, which
/// flattened every similarity edge to equal strength and cannot be recovered
/// afterwards — so the field is load-bearing, not decorative. `match_weight` is Last.fm's
/// `[0, 1]` co-listening similarity; it becomes the `CROWD_SIMILAR` edge `score`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SimilarTrack {
    /// Similar track's artist name, as Last.fm returned it.
    pub artist: String,
    /// Similar track's title, as Last.fm returned it.
    pub title: String,
    /// Co-listening match weight in `[0, 1]` — **preserved** as the edge score.
    pub match_weight: f32,
}

/// Enrichment for one artist (`artist.getInfo` + `artist.getTopTags`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ArtistEnrich {
    /// The name queried (the normalized artist id, echoed back for provenance).
    pub queried_name: String,
    /// Last.fm's autocorrected name, when it differs from the query.
    #[serde(default)]
    pub correction: Option<String>,
    /// Global listener count.
    #[serde(default)]
    pub listeners: Option<i64>,
    /// Global scrobble count.
    #[serde(default)]
    pub playcount: Option<i64>,
    /// MusicBrainz id, when Last.fm has one.
    #[serde(default)]
    pub mbid: Option<String>,
    /// Last.fm artist page URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Folksonomy tags (lowercased; top-tags filtered to count >= 10 at fetch).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Similar-artist names (co-listening), as Last.fm returned them.
    #[serde(default)]
    pub similar: Vec<String>,
    /// True once the API was queried (whether or not it returned data).
    pub fetched: bool,
    /// True when the query returned nothing usable — a soft failure, still
    /// negative-cached (skipped on re-run).
    #[serde(default)]
    pub failed: bool,
    /// Human-readable reason when `failed`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Enrichment for one track (`track.getInfo` + `track.getSimilar`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct TrackEnrich {
    /// MusicBrainz id, when present.
    #[serde(default)]
    pub mbid: Option<String>,
    /// Last.fm track page URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Track duration in milliseconds, as Last.fm reports it.
    #[serde(default)]
    pub duration_ms: Option<i64>,
    /// Global listener count.
    #[serde(default)]
    pub listeners: Option<i64>,
    /// Global scrobble count.
    #[serde(default)]
    pub playcount: Option<i64>,
    /// Canonical (autocorrected) artist name from Last.fm.
    #[serde(default)]
    pub lastfm_artist: Option<String>,
    /// Canonical (autocorrected) title from Last.fm.
    #[serde(default)]
    pub lastfm_title: Option<String>,
    /// Original-album title (compilation-rip back-mapping).
    #[serde(default)]
    pub album_title: Option<String>,
    /// Original-album MusicBrainz id.
    #[serde(default)]
    pub album_mbid: Option<String>,
    /// Original-album Last.fm URL.
    #[serde(default)]
    pub album_url: Option<String>,
    /// Track position within its original album.
    #[serde(default)]
    pub album_position: Option<i64>,
    /// Folksonomy tags (lowercased).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Similar tracks with **preserved** match weights.
    #[serde(default)]
    pub similar: Vec<SimilarTrack>,
    /// True once the API was queried.
    pub fetched: bool,
    /// True when the query returned nothing usable (soft failure).
    #[serde(default)]
    pub failed: bool,
    /// Human-readable reason when `failed`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Enrichment for one album (`album.getInfo`). No art fetching in P12 (a
/// separate backlog item).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct AlbumEnrich {
    /// MusicBrainz id, when present.
    #[serde(default)]
    pub mbid: Option<String>,
    /// Last.fm album page URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Global listener count.
    #[serde(default)]
    pub listeners: Option<i64>,
    /// Global scrobble count.
    #[serde(default)]
    pub playcount: Option<i64>,
    /// Folksonomy tags (lowercased).
    #[serde(default)]
    pub tags: Vec<String>,
    /// First 500 chars of the album wiki summary, HTML stripped.
    #[serde(default)]
    pub wiki_summary: Option<String>,
    /// True once the API was queried.
    pub fetched: bool,
    /// True when the query returned nothing usable (soft failure).
    #[serde(default)]
    pub failed: bool,
    /// Human-readable reason when `failed`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// A versioned enrichment file: the [`ENRICH_VERSION`] plus the id→record map.
/// `entries` is a `BTreeMap` so serialization is sorted and deterministic.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EnrichFile<T> {
    /// Format version — [`ENRICH_VERSION`] at write time.
    pub enrich_version: u32,
    /// Normalized-id → record. Sorted keys by construction.
    pub entries: BTreeMap<String, T>,
}

/// Handle to a library's `.sonagram/lastfm/` enrichment cache directory.
pub struct EnrichStore {
    root: PathBuf,
}

impl EnrichStore {
    /// The store rooted at `<library_root>/.sonagram/lastfm/`. Touches no disk.
    pub fn new(library_root: &Path) -> Self {
        EnrichStore {
            root: library_root.join(".sonagram").join("lastfm"),
        }
    }

    /// `<lib>/.sonagram/lastfm/` itself.
    pub fn dir(&self) -> &Path {
        &self.root
    }

    /// `<lib>/.sonagram/lastfm/artists.json`.
    pub fn artists_path(&self) -> PathBuf {
        self.root.join("artists.json")
    }

    /// `<lib>/.sonagram/lastfm/tracks.json`.
    pub fn tracks_path(&self) -> PathBuf {
        self.root.join("tracks.json")
    }

    /// `<lib>/.sonagram/lastfm/albums.json`.
    pub fn albums_path(&self) -> PathBuf {
        self.root.join("albums.json")
    }

    /// Create the enrichment cache directory if absent.
    pub fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        Ok(())
    }

    /// Load `artists.json`, or an empty map if it does not exist.
    pub fn load_artists(&self) -> Result<BTreeMap<String, ArtistEnrich>> {
        load_map(&self.artists_path())
    }

    /// Load `tracks.json`, or an empty map if it does not exist.
    pub fn load_tracks(&self) -> Result<BTreeMap<String, TrackEnrich>> {
        load_map(&self.tracks_path())
    }

    /// Load `albums.json`, or an empty map if it does not exist.
    pub fn load_albums(&self) -> Result<BTreeMap<String, AlbumEnrich>> {
        load_map(&self.albums_path())
    }

    /// Atomically write `artists.json` (pretty JSON, sorted by construction).
    pub fn save_artists(&self, m: &BTreeMap<String, ArtistEnrich>) -> Result<()> {
        self.save_map(&self.artists_path(), m)
    }

    /// Atomically write `tracks.json`.
    pub fn save_tracks(&self, m: &BTreeMap<String, TrackEnrich>) -> Result<()> {
        self.save_map(&self.tracks_path(), m)
    }

    /// Atomically write `albums.json`.
    pub fn save_albums(&self, m: &BTreeMap<String, AlbumEnrich>) -> Result<()> {
        self.save_map(&self.albums_path(), m)
    }

    fn save_map<T: Serialize>(&self, path: &Path, m: &BTreeMap<String, T>) -> Result<()> {
        self.ensure_dir()?;
        // Serialize the borrowed map (no per-record clone) with the version stamp.
        let file = EnrichFileRef {
            enrich_version: ENRICH_VERSION,
            entries: m,
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| SonagramError::Enrich(format!("serialize {}: {e}", path.display())))?;
        atomic_write(path, json.as_bytes())
    }
}

/// A by-reference view of an [`EnrichFile`] for serialization without cloning
/// every record into an owned map.
#[derive(Serialize)]
struct EnrichFileRef<'a, T> {
    enrich_version: u32,
    entries: &'a BTreeMap<String, T>,
}

/// Load a versioned enrichment map from `path`, or an empty map when absent.
fn load_map<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<BTreeMap<String, T>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(path)?;
    let file: EnrichFile<T> = serde_json::from_str(&text)
        .map_err(|e| SonagramError::Enrich(format!("parse {}: {e}", path.display())))?;
    Ok(file.entries)
}

/// Write `bytes` to `path` atomically: unique temp sibling, then rename. Mirrors
/// the scan cache's atomic write so an interrupted run never leaves a partial
/// enrichment file.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| SonagramError::Enrich(format!("no parent dir for {}", path.display())))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| SonagramError::Enrich(format!("bad file name {}", path.display())))?;
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_lib(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sonagram-enrich-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_files_load_empty() {
        let lib = tmp_lib("missing");
        let store = EnrichStore::new(&lib);
        assert!(store.load_artists().unwrap().is_empty());
        assert!(store.load_tracks().unwrap().is_empty());
        assert!(store.load_albums().unwrap().is_empty());
    }

    #[test]
    fn artists_round_trip_and_are_sorted() {
        let lib = tmp_lib("artists");
        let store = EnrichStore::new(&lib);
        let mut m = BTreeMap::new();
        m.insert(
            "ZZ Top".to_string(),
            ArtistEnrich {
                queried_name: "ZZ Top".to_string(),
                playcount: Some(9),
                fetched: true,
                ..Default::default()
            },
        );
        m.insert(
            "ABBA".to_string(),
            ArtistEnrich {
                queried_name: "ABBA".to_string(),
                tags: vec!["pop".to_string()],
                fetched: true,
                ..Default::default()
            },
        );
        store.save_artists(&m).unwrap();
        assert_eq!(store.load_artists().unwrap(), m);
        // Serialized keys are sorted (BTreeMap): "ABBA" before "ZZ Top".
        let text = std::fs::read_to_string(store.artists_path()).unwrap();
        assert!(text.find("ABBA").unwrap() < text.find("ZZ Top").unwrap());
        // Version stamped.
        assert!(text.contains("\"enrich_version\""));
    }

    #[test]
    fn track_similar_preserves_match_weight() {
        let lib = tmp_lib("tracks");
        let store = EnrichStore::new(&lib);
        let mut m = BTreeMap::new();
        m.insert(
            "abc123".to_string(),
            TrackEnrich {
                similar: vec![SimilarTrack {
                    artist: "Bee Gees".to_string(),
                    title: "Jive Talkin'".to_string(),
                    match_weight: 0.87,
                }],
                fetched: true,
                ..Default::default()
            },
        );
        store.save_tracks(&m).unwrap();
        let back = store.load_tracks().unwrap();
        assert_eq!(back["abc123"].similar[0].match_weight, 0.87);
    }

    #[test]
    fn atomic_save_leaves_no_temp_files() {
        let lib = tmp_lib("atomic");
        let store = EnrichStore::new(&lib);
        store.save_albums(&BTreeMap::new()).unwrap();
        for entry in std::fs::read_dir(store.dir()).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(
                !name.to_string_lossy().contains(".tmp."),
                "temp left: {name:?}"
            );
        }
    }
}
