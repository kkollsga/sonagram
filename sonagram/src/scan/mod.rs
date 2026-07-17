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
//!    on disk **and** its record still exists **and** is *fresh* (see below) ⇒
//!    trust the cached hash, do nothing.
//! 2. **Hash reuse** — stats changed, but the ID3-stripped
//!    [`audio_content_hash`](hash::audio_content_hash) already has a *fresh*
//!    record ⇒ the file was retagged or moved. Reuse the analysis; refresh
//!    `SourceInfo` (path/size) and the index. Still zero analyses.
//! 3. **Analyze** — an unseen hash, **or a stale record** ⇒ hand it to the
//!    [`Analyzer`]. New records are gathered and analyzed in **one batch call**
//!    so sonara parallelizes across files, with per-file failure isolation.
//!
//! **Freshness / staleness.** A cached record is *stale* when it was produced by
//! a different sonara build than the one we now link: its
//! `analysis.provenance.schema_version` ≠
//! [`ANALYSIS_SCHEMA_VERSION`](sonara::analyze::ANALYSIS_SCHEMA_VERSION), or its
//! `embedding_version` (when present) ≠
//! [`SIMILARITY_VERSION`](sonara::similarity::SIMILARITY_VERSION). A stale record is treated exactly
//! like a missing one — re-analyzed — so an upstream schema/embedding bump
//! (e.g. sonara 0.2.3's chroma fix) self-heals on the next rescan instead of
//! silently poisoning the graph with old-semantics data. Both reuse tiers verify
//! this; the check is memoized per content hash for the duration of a scan.
//!
//! Records store `source.path` **relative** to the library root, for
//! portability, privacy and determinism.

pub mod cache;
pub mod hash;

use std::collections::{HashMap, HashSet};
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
        // sonagram stays a pure mapper: genre inference belongs upstream, and
        // sonara ships no model — so we never supply one.
        genre_model: None,
    }
}

/// Coarse-grained scan progress, reported through [`ScanOptions::progress`].
///
/// Every stage reports per-item counts. Analysis rides sonara 0.2.3's
/// [`analyze_batch_with`](sonara::analyze::analyze_batch_with) core progress
/// hook, so [`ScanStage::Analyze`] fires once per file as it completes (in
/// completion order, on rayon workers) — not just once for the whole batch.
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
/// isolated to that file — it must not abort the batch. `on_done(done, total)`
/// is invoked once per completed file (completion order, possibly on worker
/// threads — keep it cheap and non-blocking), so the scanner can report real
/// per-file [`ScanStage::Analyze`] progress.
pub trait Analyzer: Send + Sync {
    /// Analyze every request, returning records stamped with each request's
    /// `source`, calling `on_done(done, total)` after each file completes.
    fn analyze(
        &self,
        requests: &[AnalyzeRequest],
        on_done: &(dyn Fn(usize, usize) + Sync),
    ) -> Vec<Result<AnalysisRecord>>;
}

/// The production analyzer: one batched
/// [`sonara::analyze::analyze_batch_with`] call.
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
    fn analyze(
        &self,
        requests: &[AnalyzeRequest],
        on_done: &(dyn Fn(usize, usize) + Sync),
    ) -> Vec<Result<AnalysisRecord>> {
        let paths: Vec<&Path> = requests.iter().map(|r| r.abs_path.as_path()).collect();
        // sr = 0 → native sample rate. analyze_batch_with isolates per-file
        // failures and drives `on_done` per completed file for real progress.
        let results = sonara::analyze::analyze_batch_with(&paths, 0, &self.config, |done, total| {
            on_done(done, total)
        });
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

/// Load the saved scan-state fingerprint for `library_root` (P19), or `None`
/// when there is no cache (or it predates fingerprints). Cheap: reads only the
/// `index.json` meta, no walk. The graph build stamps this onto the `Source`
/// node so `sonagram status` can compare graph freshness.
pub fn load_scan_fingerprint(library_root: &Path) -> Result<Option<String>> {
    Cache::new(library_root).load_scan_fingerprint()
}

/// Recompute the deterministic scan-state fingerprint of `library_root` directly
/// from disk — a **stat walk only** (no hashing, no analysis), cheap enough for
/// `sonagram status` to run on every probe.
///
/// Discovers the same `*.mp3` set a scan would, stats each for `(size, mtime)`,
/// and hashes the sorted `rel_path|size|mtime` lines with the identical
/// construction [`cache::scan_fingerprint`] applies to a saved index — so an
/// unchanged, fully-scanned library reproduces the fingerprint stamped on its
/// `Source` node at build time.
pub fn compute_scan_fingerprint(library_root: &Path) -> Result<String> {
    let cache = Cache::new(library_root);
    let files = discover_mp3s(library_root, &cache);
    let mut index = cache::Index::new();
    for abs_path in &files {
        let Some(rel) = relative_path(library_root, abs_path) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(abs_path) else {
            continue;
        };
        index.insert(
            rel,
            IndexEntry {
                size: meta.len(),
                mtime_unix: mtime_unix(&meta),
                // Not part of the fingerprint; a stat walk never hashes bytes.
                content_hash: String::new(),
            },
        );
    }
    Ok(cache::scan_fingerprint(&index))
}

/// True iff a cached record was produced by the sonara build we now link.
///
/// A record is **fresh** when its analysis schema version matches sonara's
/// [`ANALYSIS_SCHEMA_VERSION`](sonara::analyze::ANALYSIS_SCHEMA_VERSION) *and*,
/// if it carries an `embedding_version`, that matches
/// [`SIMILARITY_VERSION`](sonara::similarity::SIMILARITY_VERSION). A record with
/// no embedding_version imposes no embedding constraint. Anything else is
/// **stale** and must be re-analyzed, not trusted.
pub fn record_is_fresh(rec: &AnalysisRecord) -> bool {
    rec.analysis.provenance.schema_version == sonara::analyze::ANALYSIS_SCHEMA_VERSION
        && rec
            .analysis
            .embedding_version
            .is_none_or(|v| v == sonara::similarity::SIMILARITY_VERSION)
}

/// Freshness of the on-disk record for `content_hash`, memoized per scan.
///
/// Returns `false` for a **missing** record (so the stat fast-path self-heals a
/// deleted record) and for a **stale** one (so an upstream schema/embedding bump
/// forces re-analysis). Loading is cheap and cached by hash, so a hash shared by
/// several index entries is read at most once.
fn cached_record_is_fresh(
    cache: &Cache,
    content_hash: &str,
    memo: &mut HashMap<String, bool>,
) -> Result<bool> {
    if let Some(&fresh) = memo.get(content_hash) {
        return Ok(fresh);
    }
    let fresh = match cache.load_record(content_hash)? {
        Some(rec) => record_is_fresh(&rec),
        None => false,
    };
    memo.insert(content_hash.to_string(), fresh);
    Ok(fresh)
}

/// The outcome of a read-only freshness probe ([`probe_freshness`]).
///
/// Every count is derived without hashing a single file or running any analysis:
/// it compares the on-disk `*.mp3` set and their `(size, mtime)` stats against
/// the cached `index.json`, and checks each referenced record's schema/embedding
/// freshness against the sonara build we now link. The four disjoint file counts
/// (`fresh` + `stale` + `missing_from_index`) sum to `total_files`;
/// `deleted_in_index` counts index entries with no file on disk (not part of
/// `total_files`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshnessReport {
    /// Total `*.mp3` files discovered on disk under the library root.
    pub total_files: usize,
    /// Files present in the index whose `(size, mtime)` still match **and** whose
    /// cached record exists and is fresh (current sonara schema/embedding). These
    /// would be served from the scan's stat fast-path — zero work on a rescan.
    pub fresh: usize,
    /// Files in the index whose stats changed, or whose cached record is missing
    /// or stale (older sonara schema/embedding). A rescan would re-hash and, if
    /// needed, re-analyze these.
    pub stale: usize,
    /// Files on disk with no index entry at all (never scanned).
    pub missing_from_index: usize,
    /// Index entries whose file no longer exists on disk (deleted since the last
    /// scan). A rescan would prune these from the graph.
    pub deleted_in_index: usize,
    /// Whether a scan cache (`.sonagram/index.json`) exists at all.
    pub has_cache: bool,
}

/// Read-only freshness probe of `library_root` against its `.sonagram/` scan
/// cache. **Mutates nothing** — no index write, no record write, no analysis.
///
/// Walks for `*.mp3` (reusing the scanner's own discovery, so the file set is
/// identical to what a scan would see), then classifies each file against the
/// cached `index.json`:
///
/// - **fresh** — indexed, `(size, mtime)` unchanged, and the referenced record
///   is present and passes [`record_is_fresh`];
/// - **stale** — indexed but stats changed, or the record is missing/stale;
/// - **missing_from_index** — on disk, not in the index.
///
/// Index entries with no file on disk are counted as `deleted_in_index`.
///
/// Record freshness is checked through the same `(schema_version,
/// embedding_version)` comparison the scanner uses, memoized per content hash so
/// each unique cached record is read at most once (no sampling needed — a full,
/// exact check that stays cheap because records are content-addressed and
/// deduplicated).
pub fn probe_freshness(library_root: &Path) -> Result<FreshnessReport> {
    let cache = Cache::new(library_root);
    let has_cache = cache.index_path().exists();
    let index = cache.load_index()?;
    let files = discover_mp3s(library_root, &cache);

    let mut fresh = 0usize;
    let mut stale = 0usize;
    let mut missing_from_index = 0usize;
    let mut fresh_memo: HashMap<String, bool> = HashMap::new();
    let mut present: HashSet<String> = HashSet::with_capacity(files.len());

    for abs_path in &files {
        let rel = match relative_path(library_root, abs_path) {
            Some(r) => r,
            None => continue,
        };
        present.insert(rel.clone());

        match index.get(&rel) {
            None => missing_from_index += 1,
            Some(entry) => {
                let stats_match = match std::fs::metadata(abs_path) {
                    Ok(m) => entry.size == m.len() && entry.mtime_unix == mtime_unix(&m),
                    Err(_) => false,
                };
                if stats_match
                    && cached_record_is_fresh(&cache, &entry.content_hash, &mut fresh_memo)?
                {
                    fresh += 1;
                } else {
                    stale += 1;
                }
            }
        }
    }

    let deleted_in_index = index.keys().filter(|k| !present.contains(*k)).count();

    Ok(FreshnessReport {
        total_files: files.len(),
        fresh,
        stale,
        missing_from_index,
        deleted_in_index,
        has_cache,
    })
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

    // Per-scan memo of record freshness by content hash (see
    // [`cached_record_is_fresh`]) — avoids reloading a record shared by several
    // index entries when checking the stat fast-path.
    let mut fresh_memo: HashMap<String, bool> = HashMap::new();

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

        // Tier 1: stat fast-path. Requires the record to still exist AND be
        // fresh: a deleted record self-heals instead of leaving a hole in the
        // graph, and a stale record (older sonara schema/embedding) is
        // re-analyzed instead of silently trusted. The freshness check loads the
        // record's two version ints once per hash (memoized).
        if let Some(old) = old_index.get(&rel) {
            if old.size == size
                && old.mtime_unix == mtime
                && cached_record_is_fresh(&cache, &old.content_hash, &mut fresh_memo)?
            {
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

        // Tier 2: known, FRESH hash → reuse the analysis (retag/move path). A
        // record that vanished between the stat and the load (→ None) or one that
        // is stale falls through to re-analyze, overwriting the stale record with
        // current-schema output.
        if let Some(mut rec) = cache.load_record(&content_hash)? {
            if record_is_fresh(&rec) {
                // Refresh mutable identity (path/size) and re-save if changed.
                if rec.source != source {
                    rec.source = source;
                    cache.save_record(&rec)?;
                }
                new_index.insert(rel, entry);
                reused_hash_match += 1;
                continue;
            }
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
        let total = requests.len();
        opts.report(ScanStage::Analyze, 0, total);
        // Real per-file progress: sonara's analyze_batch_with drives this once
        // per completed file (completion order, on worker threads).
        let results = analyzer.analyze(&requests, &|done, n| {
            opts.report(ScanStage::Analyze, done, n)
        });
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
        opts.report(ScanStage::Analyze, total, total);
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
