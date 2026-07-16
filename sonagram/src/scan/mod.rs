//! Library scan: walk, content-hash, cache, and incremental rescan.
//!
//! The scanner turns a directory of MP3s into a set of cached
//! [`AnalysisRecord`]s under `<library>/.sonagram/` (see [`cache`]). Its central
//! guarantee is **incremental cheapness**: a no-op rescan of an unchanged
//! library performs *zero* sonara analyses and *zero* re-hashes.
//!
//! Three tiers decide the cost of each file, cheapest first:
//!
//! 1. **Stat fast-path** — the `(size, mtime)` in `index.json` matches the file
//!    on disk (and its record still exists) ⇒ trust the cached hash, do nothing.
//! 2. **Hash reuse** — stats changed, but the ID3-stripped
//!    [`audio_content_hash`](hash::audio_content_hash) already has a record ⇒ the
//!    file was retagged or moved. Reuse the analysis; refresh `SourceInfo`
//!    (path/size) and the index. Still zero analyses.
//! 3. **Analyze** — an unseen hash ⇒ hand it to the [`Analyzer`]. New records are
//!    gathered and analyzed in **one batch call** so sonara parallelizes across
//!    files, with per-file failure isolation.
//!
//! Records store `source.path` **relative** to the library root, for
//! portability, privacy and determinism.

pub mod cache;
pub mod hash;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use sonara::analyze::{AnalysisConfig, AnalysisMode};
use walkdir::WalkDir;

use crate::record::{AnalysisRecord, SourceInfo};
use crate::Result;

use cache::{Cache, IndexEntry};
pub use hash::{audio_content_hash, hash_kind};

/// The default sonara feature set the scanner requests — every feature the music
/// graph schema consumes.
///
/// This is the single source of truth for the analysis feature list, shared with
/// the `capture_fixtures` bin (via [`default_analysis_config`]) so the frozen
/// fixtures and live scans request identical features. Playlist mode is the base;
/// an explicit `features` set overrides the mode, so the extended
/// perceptual/spectral names are listed alongside the opt-in-only groups.
pub const DEFAULT_FEATURES: &[&str] = &[
    // Spectral (extended)
    "bandwidth",
    "rolloff",
    "flatness",
    "contrast",
    "mfcc",
    "chroma",
    // Tonal
    "chords",
    "dissonance",
    // Perceptual
    "energy",
    "danceability",
    "key",
    "valence",
    "acousticness",
    // Rhythm analysis
    "tempo_curve",
    "time_signature",
    // Opt-in-only groups the graph needs
    "tags",
    "mood",
    "instrumentalness",
    "loudness",
    "structure",
    "beatgrid",
    "silence",
    "embedding",
    "vocalness",
    "key_candidates",
];

/// The default features as owned `String`s.
pub fn default_features() -> Vec<String> {
    DEFAULT_FEATURES.iter().map(|s| s.to_string()).collect()
}

/// The canonical [`AnalysisConfig`] the scanner and `capture_fixtures` use:
/// playlist mode with [`DEFAULT_FEATURES`] requested explicitly.
pub fn default_analysis_config() -> AnalysisConfig {
    analysis_config(&default_features())
}

/// Build a playlist-mode [`AnalysisConfig`] requesting exactly `features`.
fn analysis_config(features: &[String]) -> AnalysisConfig {
    AnalysisConfig {
        mode: AnalysisMode::Playlist,
        features: Some(features.iter().cloned().collect()),
        bpm_min: None,
        bpm_max: None,
    }
}

/// Coarse-grained scan progress, reported through [`ScanOptions::progress`].
///
/// Analysis is a single batched sonara call with no per-file callback in sonara
/// 0.2.2, so [`ScanStage::Analyze`] fires once at the start of the batch with the
/// batch total; the walk/hash stages report per-file counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanProgress {
    /// Which stage of the scan is reporting.
    pub stage: ScanStage,
    /// Items completed in this stage so far.
    pub done: usize,
    /// Total items in this stage.
    pub total: usize,
}

/// The stage a [`ScanProgress`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStage {
    /// Discovering `*.mp3` files under the library root.
    Walk,
    /// Content-hashing files whose stats changed.
    Hash,
    /// Handing new hashes to the analyzer.
    Analyze,
    /// Scan complete.
    Done,
}

/// Options controlling a scan.
pub struct ScanOptions {
    /// sonara features to request for new files. Defaults to [`default_features`].
    pub features: Vec<String>,
    /// Optional progress sink.
    pub progress: Option<Box<dyn Fn(ScanProgress) + Send + Sync>>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            features: default_features(),
            progress: None,
        }
    }
}

impl ScanOptions {
    fn report(&self, stage: ScanStage, done: usize, total: usize) {
        if let Some(p) = &self.progress {
            p(ScanProgress { stage, done, total });
        }
    }
}

/// The outcome of a scan.
#[derive(Debug, Clone)]
pub struct ScanReport {
    /// Total `*.mp3` files discovered under the library root.
    pub total_files: usize,
    /// Files that ran a fresh sonara analysis (unseen content hash).
    pub analyzed: usize,
    /// Files whose analysis was reused via a content-hash match (retag/move).
    pub reused_hash_match: usize,
    /// Files served entirely from the `(size, mtime)` stat fast-path.
    pub reused_stat_match: usize,
    /// Per-file failures `(path, message)`; a failure never aborts the scan.
    pub failed: Vec<(PathBuf, String)>,
    /// Wall-clock time for the whole scan.
    pub elapsed: Duration,
}

/// A single unit of work for an [`Analyzer`]: the file to analyze and the
/// `SourceInfo` identity the scanner already computed for it.
pub struct AnalyzeRequest {
    /// Absolute path to the audio file to analyze.
    pub abs_path: PathBuf,
    /// The identity (content hash, relative path, size, format) to stamp onto the
    /// resulting record. Implementations must use this as the record's `source`.
    pub source: SourceInfo,
}

/// The analysis backend, factored out so tests can substitute a call-counting
/// mock for the real (slow, audio-requiring) sonara path.
///
/// Returns one result per request, in the same order. A per-file `Err` is
/// isolated to that file — it must not abort the batch.
pub trait Analyzer: Send + Sync {
    /// Analyze every request, returning records stamped with each request's
    /// `source`.
    fn analyze(&self, requests: &[AnalyzeRequest]) -> Vec<Result<AnalysisRecord>>;
}

/// The production analyzer: one batched [`sonara::analyze::analyze_batch`] call.
pub struct SonaraAnalyzer {
    config: AnalysisConfig,
}

impl SonaraAnalyzer {
    /// Build an analyzer requesting `features` in playlist mode.
    pub fn new(features: &[String]) -> Self {
        SonaraAnalyzer {
            config: analysis_config(features),
        }
    }
}

impl Analyzer for SonaraAnalyzer {
    fn analyze(&self, requests: &[AnalyzeRequest]) -> Vec<Result<AnalysisRecord>> {
        let paths: Vec<&Path> = requests.iter().map(|r| r.abs_path.as_path()).collect();
        // sr = 0 → native sample rate. analyze_batch isolates per-file failures.
        let results = sonara::analyze::analyze_batch(&paths, 0, &self.config);
        results
            .into_iter()
            .zip(requests)
            .map(|(res, req)| {
                res.map(|ta| AnalysisRecord::from_analysis(ta, req.source.clone()))
                    .map_err(Into::into)
            })
            .collect()
    }
}

/// Scan `library_root` with the real sonara analyzer.
///
/// Walks for `*.mp3`, reuses cached analysis wherever the content hash is
/// unchanged, and analyzes only unseen hashes. See the module docs for the
/// three-tier cost model. A no-op rescan analyzes nothing.
pub fn scan_library(library_root: &Path, opts: &ScanOptions) -> Result<ScanReport> {
    let analyzer = SonaraAnalyzer::new(&opts.features);
    scan_library_with(library_root, opts, &analyzer)
}

/// Load every cached record for `library_root`, sorted by content hash.
///
/// This is the deterministic input order the graph phase (P4) consumes: records
/// come from `analysis/*.json` regardless of walk order or wall-clock timing.
/// Returns an empty vec if the cache does not exist.
pub fn load_records(library_root: &Path) -> Result<Vec<AnalysisRecord>> {
    let cache = Cache::new(library_root);
    let dir = cache.analysis_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut json_paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    json_paths.sort();

    let mut records = Vec::with_capacity(json_paths.len());
    for path in json_paths {
        let text = std::fs::read_to_string(&path)?;
        records.push(AnalysisRecord::from_json(&text)?);
    }
    records.sort_by(|a, b| a.source.content_hash.cmp(&b.source.content_hash));
    Ok(records)
}

/// Scan `library_root` using an injected [`Analyzer`] (the seam tests drive).
pub fn scan_library_with(
    library_root: &Path,
    opts: &ScanOptions,
    analyzer: &dyn Analyzer,
) -> Result<ScanReport> {
    let start = Instant::now();
    let cache = Cache::new(library_root);
    let old_index = cache.load_index()?;

    // -- Walk: discover *.mp3, deterministically ordered --
    let files = discover_mp3s(library_root, &cache);
    opts.report(ScanStage::Walk, files.len(), files.len());

    // The fresh index we will persist: only files that currently exist end up in
    // it, so deleted files are pruned automatically.
    let mut new_index = cache::Index::new();
    let mut reused_stat_match = 0usize;
    let mut reused_hash_match = 0usize;
    let mut failed: Vec<(PathBuf, String)> = Vec::new();

    // New work gathered for one batched analyze call. `pending_meta` runs
    // parallel to `requests` and carries the index bookkeeping for each.
    let mut requests: Vec<AnalyzeRequest> = Vec::new();
    let mut pending_meta: Vec<(String, IndexEntry)> = Vec::new();

    let mut hashed = 0usize;
    for abs_path in &files {
        let rel = match relative_path(library_root, abs_path) {
            Some(r) => r,
            None => {
                failed.push((abs_path.clone(), "path not under library root".to_string()));
                continue;
            }
        };

        let meta = match std::fs::metadata(abs_path) {
            Ok(m) => m,
            Err(e) => {
                failed.push((abs_path.clone(), format!("stat: {e}")));
                continue;
            }
        };
        let size = meta.len();
        let mtime = mtime_unix(&meta);

        // Tier 1: stat fast-path. Requires the record to still exist, so a
        // deleted record self-heals instead of leaving a hole in the graph.
        if let Some(old) = old_index.get(&rel) {
            if old.size == size && old.mtime_unix == mtime && cache.has_record(&old.content_hash) {
                new_index.insert(rel, old.clone());
                reused_stat_match += 1;
                continue;
            }
        }

        // Tier 2/3: hash the file.
        hashed += 1;
        opts.report(ScanStage::Hash, hashed, files.len());
        let content_hash = match audio_content_hash(abs_path) {
            Ok(h) => h,
            Err(e) => {
                failed.push((abs_path.clone(), format!("hash: {e}")));
                continue;
            }
        };
        let source = SourceInfo {
            content_hash: content_hash.clone(),
            hash_kind: hash_kind(abs_path).to_string(),
            path: rel.clone(),
            file_size: size,
            format: file_format(abs_path),
        };
        let entry = IndexEntry {
            size,
            mtime_unix: mtime,
            content_hash: content_hash.clone(),
        };

        // Tier 2: known hash → reuse the analysis (retag/move path). If the
        // record vanished between the stat and the load, fall through to
        // re-analyze.
        if let Some(mut rec) = cache.load_record(&content_hash)? {
            // Refresh mutable identity (path/size) and re-save if changed.
            if rec.source != source {
                rec.source = source;
                cache.save_record(&rec)?;
            }
            new_index.insert(rel, entry);
            reused_hash_match += 1;
            continue;
        }

        // Tier 3: unseen hash → queue for analysis.
        requests.push(AnalyzeRequest {
            abs_path: abs_path.clone(),
            source,
        });
        pending_meta.push((rel, entry));
    }

    // -- Analyze all new files in one batch --
    let mut analyzed = 0usize;
    if !requests.is_empty() {
        opts.report(ScanStage::Analyze, 0, requests.len());
        let results = analyzer.analyze(&requests);
        for ((res, req), (rel, entry)) in results.into_iter().zip(&requests).zip(pending_meta) {
            match res {
                Ok(record) => {
                    cache.save_record(&record)?;
                    new_index.insert(rel, entry);
                    analyzed += 1;
                }
                Err(e) => failed.push((req.abs_path.clone(), e.to_string())),
            }
        }
        opts.report(ScanStage::Analyze, requests.len(), requests.len());
    }

    cache.save_index(&new_index)?;
    let total_files = files.len();
    opts.report(ScanStage::Done, total_files, total_files);

    Ok(ScanReport {
        total_files,
        analyzed,
        reused_hash_match,
        reused_stat_match,
        failed,
        elapsed: start.elapsed(),
    })
}

/// Walk `library_root` for `*.mp3` (case-insensitive), skipping hidden
/// directories (including the `.sonagram/` cache). Results are sorted for
/// deterministic ordering.
fn discover_mp3s(library_root: &Path, _cache: &Cache) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = WalkDir::new(library_root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            // Prune hidden directories (names starting with '.'), which covers
            // `.sonagram/`. Never prune the root itself.
            if e.depth() == 0 {
                return true;
            }
            let hidden = e
                .file_name()
                .to_str()
                .map(|n| n.starts_with('.'))
                .unwrap_or(false);
            !hidden
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("mp3"))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

/// The path of `abs_path` relative to `library_root`, as a `/`-separated string.
fn relative_path(library_root: &Path, abs_path: &Path) -> Option<String> {
    let rel = abs_path.strip_prefix(library_root).ok()?;
    // Join components with '/' for a portable, deterministic key.
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Lowercased file extension, e.g. `"mp3"`.
fn file_format(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

/// File mtime as whole seconds since the Unix epoch (signed).
fn mtime_unix(meta: &std::fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64),
        },
        Err(_) => 0,
    }
}
