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

/// The on-disk shape of `index.json` (P19). The per-file entries are stored
/// **flattened** at the top level — exactly the old bare-`BTreeMap` layout — so
/// a pre-P19 `index.json` (no `scan_fingerprint` key) still deserializes: the
/// missing field defaults to `None` and every remaining key flows into
/// `entries`. New writes prepend a `scan_fingerprint`; nothing is version-bumped.
#[derive(Serialize, Deserialize, Default)]
struct IndexFile {
    /// The scan-state fingerprint (see [`scan_fingerprint`]) as of the last save.
    /// Absent in pre-P19 caches (→ `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scan_fingerprint: Option<String>,
    /// The stat entries, flattened to the top level (backward-compatible shape).
    #[serde(flatten)]
    entries: Index,
}

/// Deterministic blake3 fingerprint of a scan's on-disk state: one
/// `rel_path|size|mtime` line per indexed file, in sorted rel-path order (the
/// `Index` `BTreeMap` iterates sorted). Stamped into `index.json` at save time
/// and onto each `Source` node at build time, so `sonagram status` can tell
/// whether the graph reflects the current disk state — without rebuilding.
///
/// Only `(rel_path, size, mtime)` feed the hash; `content_hash` is deliberately
/// excluded so a stat-only recompute from disk ([`crate::scan::compute_scan_fingerprint`])
/// reproduces the identical fingerprint without hashing a single file.
pub fn scan_fingerprint(index: &Index) -> String {
    let mut hasher = blake3::Hasher::new();
    for (rel, entry) in index {
        hasher.update(rel.as_bytes());
        hasher.update(b"|");
        hasher.update(entry.size.to_string().as_bytes());
        hasher.update(b"|");
        hasher.update(entry.mtime_unix.to_string().as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

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

    /// Load the parsed `index.json` (entries + fingerprint), or an empty default
    /// if it does not exist yet.
    fn load_index_file(&self) -> Result<IndexFile> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(IndexFile::default());
        }
        let text = std::fs::read_to_string(&path)?;
        serde_json::from_str(&text).map_err(|e| SonagramError::Cache(format!("index.json: {e}")))
    }

    /// Load the index entries, or an empty map if it does not exist yet.
    pub fn load_index(&self) -> Result<Index> {
        Ok(self.load_index_file()?.entries)
    }

    /// Load the saved scan-state fingerprint (P19), or `None` when the cache is
    /// absent or was written before fingerprints existed.
    pub fn load_scan_fingerprint(&self) -> Result<Option<String>> {
        Ok(self.load_index_file()?.scan_fingerprint)
    }

    /// Atomically write the index (pretty JSON, sorted by construction), stamping
    /// the current [`scan_fingerprint`] over the entries.
    pub fn save_index(&self, index: &Index) -> Result<()> {
        self.ensure_dirs()?;
        let file = IndexFile {
            scan_fingerprint: Some(scan_fingerprint(index)),
            entries: index.clone(),
        };
        let json = serde_json::to_string_pretty(&file)
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
                    genre_model_id: None,
                    vocalness_model_id: None,
                },
                duration_sec: 1.0,
                bpm: 0.0,
                bpm_raw: 0.0,
                bpm_confidence: 0.0,
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

    fn entry(size: u64, mtime: i64, hash: &str) -> IndexEntry {
        IndexEntry { size, mtime_unix: mtime, content_hash: hash.to_string() }
    }

    #[test]
    fn scan_fingerprint_is_deterministic_and_change_sensitive() {
        let mut a = Index::new();
        a.insert("x.mp3".to_string(), entry(10, 100, "h1"));
        a.insert("y.mp3".to_string(), entry(20, 200, "h2"));
        // A second, identically-built index → identical fingerprint.
        let mut b = Index::new();
        b.insert("y.mp3".to_string(), entry(20, 200, "hZ")); // content_hash is NOT part of it
        b.insert("x.mp3".to_string(), entry(10, 100, "hZ"));
        assert_eq!(scan_fingerprint(&a), scan_fingerprint(&b), "order + content_hash irrelevant");

        // Changing a size moves the fingerprint.
        let mut c = a.clone();
        c.insert("x.mp3".to_string(), entry(11, 100, "h1"));
        assert_ne!(scan_fingerprint(&a), scan_fingerprint(&c), "size change ⇒ new fingerprint");

        // Changing an mtime moves the fingerprint.
        let mut d = a.clone();
        d.insert("x.mp3".to_string(), entry(10, 101, "h1"));
        assert_ne!(scan_fingerprint(&a), scan_fingerprint(&d), "mtime change ⇒ new fingerprint");

        // Adding a file moves the fingerprint.
        let mut e = a.clone();
        e.insert("z.mp3".to_string(), entry(1, 1, "h3"));
        assert_ne!(scan_fingerprint(&a), scan_fingerprint(&e), "added file ⇒ new fingerprint");
    }

    #[test]
    fn save_index_stamps_and_reloads_fingerprint() {
        let lib = tmp_lib("fingerprint");
        let cache = Cache::new(&lib);
        let mut index = Index::new();
        index.insert("a.mp3".to_string(), entry(1, 10, "aa"));
        cache.save_index(&index).unwrap();
        // The reload carries the fingerprint the save computed.
        assert_eq!(
            cache.load_scan_fingerprint().unwrap().as_deref(),
            Some(scan_fingerprint(&index).as_str())
        );
        // And entries still round-trip unchanged.
        assert_eq!(cache.load_index().unwrap(), index);
    }

    #[test]
    fn pre_p19_bare_map_index_still_loads() {
        // A cache written before P19 is a bare `{ "a.mp3": {..} }` map with no
        // scan_fingerprint key — it must still load (fingerprint → None).
        let lib = tmp_lib("legacy");
        let cache = Cache::new(&lib);
        cache.ensure_dirs().unwrap();
        let legacy = r#"{
  "a.mp3": { "size": 5, "mtime_unix": 42, "content_hash": "cafe" }
}"#;
        std::fs::write(cache.index_path(), legacy).unwrap();
        let idx = cache.load_index().unwrap();
        assert_eq!(idx.len(), 1);
        assert_eq!(idx["a.mp3"], entry(5, 42, "cafe"));
        assert!(cache.load_scan_fingerprint().unwrap().is_none(), "no fingerprint in a legacy cache");
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
