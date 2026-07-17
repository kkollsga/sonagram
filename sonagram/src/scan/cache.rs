//! On-disk scan cache under `<library>/.sonagram/`.
//!
//! Two artifacts live here, both written deterministically:
//!
//! - `analysis/<content_hash>.json` — one [`AnalysisRecord`] per audio content
//!   hash (content-addressed, so a moved/retagged file reuses its record and an
//!   orphaned record is harmless).
//! - `index.json` — a `BTreeMap<relative_path, IndexEntry>` giving the
//!   `(size, mtime)` stat fast-path that lets a no-op rescan skip hashing and
//!   analysis entirely. `BTreeMap` (never `HashMap`) so the serialized bytes are
//!   sorted and reproducible.
//!
//! Both writes are **atomic** (write a temp sibling, then rename), so an
//! interrupted scan never leaves a half-written index or record. The cache dir
//! need not exist: loads treat "missing" as "empty", and saves create it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::record::AnalysisRecord;
use crate::{Result, SonagramError};

/// The index's per-file stat entry. Keyed in the index map by the file's path
/// relative to the library root.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// File size in bytes at scan time.
    pub size: u64,
    /// File mtime as whole seconds since the Unix epoch (signed: pre-epoch
    /// stamps are negative).
    pub mtime_unix: i64,
    /// The audio content hash the scanner computed for this file.
    pub content_hash: String,
}

/// The stat index: relative path → [`IndexEntry`]. A `BTreeMap` for
/// deterministic (sorted) serialization.
pub type Index = BTreeMap<String, IndexEntry>;

/// Handle to a library's `.sonagram/` cache directory.
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// The cache rooted at `<library_root>/.sonagram/`. Does not touch the disk.
    pub fn new(library_root: &Path) -> Self {
        Cache {
            root: library_root.join(".sonagram"),
        }
    }

    /// `<lib>/.sonagram/` itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<lib>/.sonagram/analysis/`.
    pub fn analysis_dir(&self) -> PathBuf {
        self.root.join("analysis")
    }

    /// `<lib>/.sonagram/index.json`.
    pub fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    /// Path of the record file for `content_hash`.
    pub fn record_path(&self, content_hash: &str) -> PathBuf {
        self.analysis_dir().join(format!("{content_hash}.json"))
    }

    /// Create the cache + analysis directories if absent.
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(self.analysis_dir())?;
        Ok(())
    }

    /// Load the index, or an empty map if it does not exist yet.
    pub fn load_index(&self) -> Result<Index> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(Index::new());
        }
        let text = std::fs::read_to_string(&path)?;
        serde_json::from_str(&text).map_err(|e| SonagramError::Cache(format!("index.json: {e}")))
    }

    /// Atomically write the index (pretty JSON, sorted by construction).
    pub fn save_index(&self, index: &Index) -> Result<()> {
        self.ensure_dirs()?;
        let json = serde_json::to_string_pretty(index)
            .map_err(|e| SonagramError::Cache(format!("serialize index: {e}")))?;
        atomic_write(&self.index_path(), json.as_bytes())
    }

    /// True if a record file exists for `content_hash` (cheap stat).
    pub fn has_record(&self, content_hash: &str) -> bool {
        self.record_path(content_hash).exists()
    }

    /// Load the record for `content_hash`, or `None` if there is no such file.
    pub fn load_record(&self, content_hash: &str) -> Result<Option<AnalysisRecord>> {
        let path = self.record_path(content_hash);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        AnalysisRecord::from_json(&text).map(Some)
    }

    /// Atomically write `record` to `analysis/<content_hash>.json`.
    pub fn save_record(&self, record: &AnalysisRecord) -> Result<()> {
        self.ensure_dirs()?;
        let json = record.to_json_pretty()?;
        atomic_write(&self.record_path(&record.source.content_hash), json.as_bytes())
    }
}

/// Write `bytes` to `path` atomically: write a uniquely-named temp sibling, then
/// rename over the target. Rename is atomic within a filesystem, so a reader
/// sees either the old file or the fully-written new one, never a partial write.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or_else(|| {
        SonagramError::Cache(format!("no parent dir for {}", path.display()))
    })?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| SonagramError::Cache(format!("bad file name {}", path.display())))?;
    // Unique temp name: pid keeps concurrent scans of the same library from
    // clobbering each other's temp file mid-write.
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{AnalysisDto, ProvenanceDto, SourceInfo};

    fn tmp_lib(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sonagram-cache-{}-{}-{}",
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

    fn sample_record(hash: &str, path: &str) -> AnalysisRecord {
        AnalysisRecord {
            record_version: crate::record::RECORD_VERSION,
            source: SourceInfo {
                content_hash: hash.to_string(),
                hash_kind: "mp3-audio-v1".to_string(),
                path: path.to_string(),
                file_size: 123,
                format: "mp3".to_string(),
            },
            tags: None,
            analysis: AnalysisDto {
                provenance: ProvenanceDto {
                    schema_version: 1,
                    sample_rate: 22050,
                    hop_length: 512,
                    mode: "playlist".to_string(),
                    requested_features: None,
                },
                duration_sec: 1.0,
                bpm: 0.0,
                bpm_raw: 0.0,
                bpm_candidates: vec![],
                beats: vec![],
                onset_frames: vec![],
                rms_mean: 0.0,
                rms_max: 0.0,
                loudness_lufs: 0.0,
                dynamic_range_db: 0.0,
                true_peak_db: None,
                replaygain_db: None,
                loudness_curve: None,
                loudness_momentary_max_db: None,
                loudness_range_lu: None,
                spectral_centroid_mean: 0.0,
                zero_crossing_rate: 0.0,
                onset_density: 0.0,
                spectral_bandwidth_mean: None,
                spectral_rolloff_mean: None,
                spectral_flatness_mean: None,
                spectral_contrast_mean: None,
                mfcc_mean: None,
                chroma_mean: None,
                tempo_curve: None,
                tempo_variability: None,
                time_signature: None,
                time_signature_confidence: None,
                chord_sequence: None,
                chord_events: None,
                chord_change_rate: None,
                predominant_chord: None,
                dissonance: None,
                energy: None,
                danceability: None,
                key: None,
                key_confidence: None,
                key_camelot: None,
                valence: None,
                acousticness: None,
                embedding: None,
                mood_happy: None,
                mood_aggressive: None,
                mood_relaxed: None,
                mood_sad: None,
                instrumentalness: None,
                genre: None,
                genre_confidence: None,
                grid_offset_sec: None,
                downbeats: None,
                grid_stability: None,
                energy_curve: None,
                energy_curve_hop_sec: None,
                segments: None,
                intro_end_sec: None,
                outro_start_sec: None,
                energy_level: None,
                leading_silence_sec: None,
                trailing_silence_sec: None,
                key_candidates: None,
                vocalness: None,
                fingerprint: None,
                embedding_version: None,
            },
        }
    }

    #[test]
    fn missing_cache_loads_empty() {
        let lib = tmp_lib("missing");
        let cache = Cache::new(&lib);
        assert!(cache.load_index().unwrap().is_empty());
        assert!(!cache.has_record("deadbeef"));
        assert!(cache.load_record("deadbeef").unwrap().is_none());
    }

    #[test]
    fn index_round_trips_and_is_sorted() {
        let lib = tmp_lib("index");
        let cache = Cache::new(&lib);
        let mut index = Index::new();
        index.insert(
            "z.mp3".to_string(),
            IndexEntry { size: 2, mtime_unix: 20, content_hash: "bb".to_string() },
        );
        index.insert(
            "a.mp3".to_string(),
            IndexEntry { size: 1, mtime_unix: 10, content_hash: "aa".to_string() },
        );
        cache.save_index(&index).unwrap();
        let back = cache.load_index().unwrap();
        assert_eq!(index, back);
        // Serialized order is sorted by key (BTreeMap): "a.mp3" before "z.mp3".
        let text = std::fs::read_to_string(cache.index_path()).unwrap();
        assert!(text.find("a.mp3").unwrap() < text.find("z.mp3").unwrap());
    }

    #[test]
    fn record_round_trips() {
        let lib = tmp_lib("record");
        let cache = Cache::new(&lib);
        let rec = sample_record("cafef00d", "song.mp3");
        assert!(!cache.has_record("cafef00d"));
        cache.save_record(&rec).unwrap();
        assert!(cache.has_record("cafef00d"));
        assert_eq!(cache.load_record("cafef00d").unwrap().unwrap(), rec);
    }

    #[test]
    fn atomic_save_leaves_no_temp_files() {
        let lib = tmp_lib("atomic");
        let cache = Cache::new(&lib);
        cache.save_record(&sample_record("aa", "a.mp3")).unwrap();
        cache.save_index(&Index::new()).unwrap();
        // No stray ".tmp" siblings remain after atomic writes.
        for entry in std::fs::read_dir(cache.analysis_dir()).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(!name.to_string_lossy().contains(".tmp."), "temp left: {name:?}");
        }
    }
}
