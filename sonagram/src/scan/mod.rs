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
//! 3. **Analyze** — an unseen hash, **or a stale record** ⇒ hand one canonical
//!    path per content hash to the [`Analyzer`]. The worker pool parallelizes
//!    distinct hashes while same-hash paths share that single outcome.
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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sonara::analyze::{AnalysisConfig, AnalysisMode};
use walkdir::WalkDir;

use crate::progress::{load_progress, unix_now, ProgressWriter};
use crate::record::{AnalysisRecord, SourceInfo};
use crate::Result;

use cache::{Cache, IndexEntry};
pub use hash::{audio_content_hash, hash_kind};

/// How often the scan's on-disk progress snapshot is refreshed (unforced
/// writes; stage transitions always land).
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
/// How often the index is flushed mid-scan, making an interrupted cold scan
/// resumable at ~this granularity via the stat fast-path.
const INDEX_FLUSH_INTERVAL: Duration = Duration::from_secs(30);

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
        // Enabled deliberately only after the model-aware cache gate lands.
        vocalness_model: None,
    }
}

/// Coarse-grained scan progress, reported through [`ScanOptions::progress`].
///
/// Every stage reports per-item counts. Analysis rides sonara 0.2.3's
/// [`analyze_batch_with`](sonara::analyze::analyze_batch_with) core progress
/// hook, so [`ScanStage::Analyze`] fires once per file as it completes (in
/// completion order, on scan workers) — once per unique content hash, not once
/// per aliasing path.
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

/// The stable string form of a [`ScanStage`], used in progress files and the
/// Python callback contract: `"walk"`, `"hash"`, `"analyze"`, `"done"`.
pub fn stage_name(stage: ScanStage) -> &'static str {
    match stage {
        ScanStage::Walk => "walk",
        ScanStage::Hash => "hash",
        ScanStage::Analyze => "analyze",
        ScanStage::Done => "done",
    }
}

/// The on-disk scan progress snapshot (P20): `<lib>/.sonagram/scan_progress.json`.
///
/// Written atomically (throttled to [`PROGRESS_INTERVAL`]) by
/// [`scan_library_with`] itself, so every entry point — CLI, Python, tests —
/// produces the same observable progress. Read it with [`load_scan_progress`]
/// (or `sonagram progress` / `sonagram status`). `updated_unix` going stale
/// while `stage != "done"` means the scanning process died or was killed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanProgressSnapshot {
    /// PID of the scanning process (staleness diagnosis, not liveness proof).
    pub pid: u32,
    /// Current stage: `"walk"`, `"hash"` (deciding/hashing, analysis workers
    /// already running), `"analyze"` (hashing finished, draining the queue),
    /// `"done"`.
    pub stage: String,
    /// Files that have passed the decision loop (stat/hash/queue) so far —
    /// the primary "x of y files processed" scan counter.
    pub done: usize,
    /// Total files discovered by the walk.
    pub total: usize,
    /// Analyses completed so far (success or failure).
    pub analyze_done: usize,
    /// Unique content hashes queued so far. Grows while `stage == "hash"`
    /// (discovery is still running); final once the stage moves to `"analyze"`.
    pub analyze_total: usize,
    /// Unique content hashes analyzed (new records saved) so far this scan.
    pub analyzed: usize,
    /// Files served from the stat fast-path so far.
    pub reused_stat: usize,
    /// Files served from the hash-reuse path so far.
    pub reused_hash: usize,
    /// Per-file failures so far.
    pub failed: usize,
    /// When this scan started (unix seconds).
    pub started_unix: i64,
    /// When this snapshot was written (unix seconds).
    pub updated_unix: i64,
}

/// Path of a library's scan progress file.
pub fn scan_progress_path(library_root: &Path) -> PathBuf {
    library_root.join(".sonagram").join("scan_progress.json")
}

/// Load a library's scan progress snapshot, `None` when absent or unreadable.
pub fn load_scan_progress(library_root: &Path) -> Option<ScanProgressSnapshot> {
    load_progress(&scan_progress_path(library_root))
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
    /// Unique content hashes that ran a fresh sonara analysis.
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
/// **Streaming contract (P20):** the scanner owns all fan-out — it runs a
/// worker pool that pulls files off the hash loop's queue *while hashing is
/// still discovering more* and calls `analyze_one` from those workers
/// concurrently. Implementations therefore only analyze a single file, must be
/// `Send + Sync`, and must stamp the returned record with the request's
/// `source`. A per-file `Err` is isolated to that file.
pub trait Analyzer: Send + Sync {
    /// Analyze one file, returning the record stamped with `request.source`.
    fn analyze_one(&self, request: &AnalyzeRequest) -> Result<AnalysisRecord>;
}

/// The production analyzer: one [`sonara::analyze::analyze_file`] call per
/// request (the scanner's worker pool provides the cross-file parallelism that
/// `analyze_batch_with` used to supply internally).
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
    fn analyze_one(&self, request: &AnalyzeRequest) -> Result<AnalysisRecord> {
        // sr = 0 → native sample rate (matches every record in existing caches;
        // a change here would fork analysis semantics mid-library).
        sonara::analyze::analyze_file(&request.abs_path, 0, &self.config)
            .map(|ta| AnalysisRecord::from_analysis(ta, request.source.clone()))
            .map_err(Into::into)
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

/// Load the fresh, index-authoritative records for `library_root`, sorted by
/// content hash.
///
/// `index.json` is the authority for what belongs to the current source. Cached
/// JSON that is no longer referenced (for example after a file is deleted) is
/// deliberately retained on disk for content-addressed reuse, but must not
/// re-enter a graph. Multiple indexed paths with the same content hash yield
/// one record. A stale indexed record is omitted: it represents analysis that a
/// failed or interrupted rescan could not refresh and is therefore not valid
/// graph input. A missing indexed record is cache corruption and errors with
/// the first affected path so the caller never builds a silently incomplete
/// graph.
///
/// Returns an empty vec when there is no index (or the index is empty).
pub fn load_records(library_root: &Path) -> Result<Vec<AnalysisRecord>> {
    let cache = Cache::new(library_root);
    let index = cache.load_index()?;

    // hash -> first indexed relative path. Index iteration is path-sorted and
    // BTreeMap makes the record-load order hash-sorted, independent of scan or
    // index construction order.
    let mut indexed_hashes: BTreeMap<String, String> = BTreeMap::new();
    for (rel_path, entry) in index {
        indexed_hashes
            .entry(entry.content_hash)
            .or_insert(rel_path);
    }

    let mut records = Vec::with_capacity(indexed_hashes.len());
    for (content_hash, first_path) in indexed_hashes {
        let Some(record) = cache.load_record(&content_hash)? else {
            return Err(crate::SonagramError::Cache(format!(
                "index entry `{first_path}` references missing analysis record `{content_hash}`; run `sonagram scan` to repair the cache"
            )));
        };
        if record.source.content_hash != content_hash {
            return Err(crate::SonagramError::Cache(format!(
                "analysis record `{content_hash}` contains mismatched content hash `{}`; run `sonagram scan` to repair the cache",
                record.source.content_hash
            )));
        }
        if record_is_fresh(&record) {
            records.push(record);
        }
    }
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
///
/// **Streaming pipeline (P20).** The decision loop (stat fast-path / hash /
/// queue) runs on the calling thread while a pool of analysis workers drains
/// the queue concurrently — analysis starts the moment the first unseen file
/// is hashed, so a cold scan pays no separate hash-pass wall-clock and a
/// freshly-hashed file is often still in the page cache when analyzed.
///
/// **Persistence discipline:** every new record is saved the moment its
/// analysis completes, and the index is flushed (merged over the previous
/// index) every [`INDEX_FLUSH_INTERVAL`] — so a killed cold scan resumes from
/// roughly where it stopped instead of losing the whole batch. Live progress is
/// mirrored to `scan_progress.json` throughout (see [`ScanProgressSnapshot`]).
pub fn scan_library_with(
    library_root: &Path,
    opts: &ScanOptions,
    analyzer: &dyn Analyzer,
) -> Result<ScanReport> {
    let start = Instant::now();
    let started_unix = unix_now();
    let cache = Cache::new(library_root);
    let old_index = cache.load_index()?;
    let progress_file = ProgressWriter::new(scan_progress_path(library_root), PROGRESS_INTERVAL);

    // -- Walk: discover *.mp3, deterministically ordered --
    let files = discover_mp3s(library_root, &cache);
    let total_files = files.len();
    opts.report(ScanStage::Walk, total_files, total_files);

    // All mutable scan state, shared between the decision loop and the
    // analysis workers. Records are saved OUTSIDE this lock (they are distinct
    // files, written atomically, and the write is the expensive part); the
    // lock guards counters, the growing index, and flush/progress decisions.
    struct ScanState {
        /// The fresh index to persist: only files that currently exist end up
        /// in it, so deleted files are pruned automatically.
        new_index: cache::Index,
        failed: Vec<(PathBuf, String)>,
        reused_stat: usize,
        reused_hash: usize,
        /// Files through the decision loop so far.
        decided: usize,
        /// True once the decision loop has seen every file.
        deciding_complete: bool,
        analyze_done: usize,
        analyze_total: usize,
        analyzed: usize,
        /// Work claimed during this scan, keyed by content hash. Keeping the
        /// terminal outcome prevents a fast worker from racing a later alias
        /// into a second analysis (or from changing the canonical source path).
        hash_work: BTreeMap<String, HashWork>,
        last_flush: Instant,
    }
    #[derive(Clone)]
    struct HashFollower {
        abs_path: PathBuf,
        rel: String,
        entry: IndexEntry,
    }
    enum HashWork {
        Pending(Vec<HashFollower>),
        Succeeded,
        Failed(String),
    }
    let state = Mutex::new(ScanState {
        new_index: cache::Index::new(),
        failed: Vec::new(),
        reused_stat: 0,
        reused_hash: 0,
        decided: 0,
        deciding_complete: false,
        analyze_done: 0,
        analyze_total: 0,
        analyzed: 0,
        hash_work: BTreeMap::new(),
        last_flush: Instant::now(),
    });
    let poisoned = || crate::SonagramError::Cache("scan state poisoned".to_string());
    let snapshot = |s: &ScanState, stage: ScanStage| ScanProgressSnapshot {
        pid: std::process::id(),
        stage: stage_name(stage).to_string(),
        done: s.decided,
        total: total_files,
        analyze_done: s.analyze_done,
        analyze_total: s.analyze_total,
        analyzed: s.analyzed,
        reused_stat: s.reused_stat,
        reused_hash: s.reused_hash,
        failed: s.failed.len(),
        started_unix,
        updated_unix: unix_now(),
    };
    // The live stage under the streaming pipeline: `"hash"` while the decision
    // loop is still discovering work, `"analyze"` while the queue drains.
    let live_stage = |s: &ScanState| {
        if s.deciding_complete {
            ScanStage::Analyze
        } else {
            ScanStage::Hash
        }
    };
    // Merged mid-scan flush: decisions made so far survive a kill without
    // dropping not-yet-revisited old entries. Best-effort by design (the final
    // save below is the authoritative, pruning write).
    let flush_index_locked = |s: &mut ScanState| {
        if s.last_flush.elapsed() >= INDEX_FLUSH_INTERVAL {
            let mut merged = old_index.clone();
            merged.extend(s.new_index.iter().map(|(k, v)| (k.clone(), v.clone())));
            s.last_flush = Instant::now();
            let _ = cache.save_index(&merged);
        }
    };
    {
        let s = state.lock().map_err(|_| poisoned())?;
        progress_file.write(&snapshot(&s, ScanStage::Walk), true);
    }

    // -- Streaming pipeline: the decision loop feeds an analysis worker pool --
    let (tx, rx) = std::sync::mpsc::channel::<(AnalyzeRequest, String, IndexEntry)>();
    let rx = Mutex::new(rx);
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    std::thread::scope(|scope| -> Result<()> {
        for _ in 0..n_workers {
            scope.spawn(|| loop {
                // Take one queued file; a closed-and-drained queue ends the
                // worker. Holding the rx lock across the blocking recv is the
                // standard shared-receiver pattern: dequeue is serialized,
                // analysis is not.
                let msg = match rx.lock() {
                    Ok(g) => g.recv(),
                    Err(_) => break,
                };
                let Ok((req, rel, entry)) = msg else { break };
                let content_hash = req.source.content_hash.clone();
                // Analyze + persist first (no lock held): a record on disk is
                // the unit of resumability. A failed save demotes the result
                // to a per-file failure.
                let outcome = analyzer
                    .analyze_one(&req)
                    .and_then(|rec| cache.save_record(&rec));
                let Ok(mut s) = state.lock() else { break };
                let followers = match s.hash_work.remove(&content_hash) {
                    Some(HashWork::Pending(followers)) => followers,
                    _ => Vec::new(),
                };
                match outcome {
                    Ok(()) => {
                        s.new_index.insert(rel, entry);
                        s.analyzed += 1;
                        s.reused_hash += followers.len();
                        for follower in followers {
                            s.new_index.insert(follower.rel, follower.entry);
                        }
                        s.hash_work.insert(content_hash, HashWork::Succeeded);
                    }
                    Err(e) => {
                        let message = e.to_string();
                        s.failed.push((req.abs_path, message.clone()));
                        for follower in followers {
                            s.failed.push((follower.abs_path, message.clone()));
                        }
                        s.hash_work
                            .insert(content_hash, HashWork::Failed(message));
                    }
                }
                s.analyze_done += 1;
                opts.report(ScanStage::Analyze, s.analyze_done, s.analyze_total);
                let stage = live_stage(&s);
                progress_file.write(&snapshot(&s, stage), false);
                flush_index_locked(&mut s);
            });
        }

        // The decision loop (this thread): stat fast-path / hash reuse /
        // queue-for-analysis. Every arm ends in one short `tick` lock that
        // applies the outcome and advances the shared counters.
        let tick = |mutate: &dyn Fn(&mut ScanState)| -> Result<()> {
            let mut s = state.lock().map_err(|_| poisoned())?;
            mutate(&mut s);
            s.decided += 1;
            let stage = live_stage(&s);
            progress_file.write(&snapshot(&s, stage), false);
            flush_index_locked(&mut s);
            Ok(())
        };

        // Per-scan memo of record freshness by content hash (see
        // [`cached_record_is_fresh`]) — avoids reloading a record shared by
        // several index entries when checking the stat fast-path.
        let mut fresh_memo: HashMap<String, bool> = HashMap::new();
        let mut hashed = 0usize;
        for abs_path in &files {
            let rel = match relative_path(library_root, abs_path) {
                Some(r) => r,
                None => {
                    tick(&|s| {
                        s.failed
                            .push((abs_path.clone(), "path not under library root".to_string()));
                    })?;
                    continue;
                }
            };

            let meta = match std::fs::metadata(abs_path) {
                Ok(m) => m,
                Err(e) => {
                    let msg = format!("stat: {e}");
                    tick(&|s| s.failed.push((abs_path.clone(), msg.clone())))?;
                    continue;
                }
            };
            let size = meta.len();
            let mtime = mtime_unix(&meta);

            // Tier 1: stat fast-path. Requires the record to still exist AND be
            // fresh: a deleted record self-heals instead of leaving a hole in the
            // graph, and a stale record (older sonara schema/embedding) is
            // re-analyzed instead of silently trusted. The freshness check loads
            // the record's two version ints once per hash (memoized).
            if let Some(old) = old_index.get(&rel) {
                if old.size == size
                    && old.mtime_unix == mtime
                    && cached_record_is_fresh(&cache, &old.content_hash, &mut fresh_memo)?
                {
                    let entry = old.clone();
                    tick(&|s| {
                        s.new_index.insert(rel.clone(), entry.clone());
                        s.reused_stat += 1;
                    })?;
                    continue;
                }
            }

            // Tier 2/3: hash the file.
            hashed += 1;
            opts.report(ScanStage::Hash, hashed, total_files);
            let content_hash = match audio_content_hash(abs_path) {
                Ok(h) => h,
                Err(e) => {
                    let msg = format!("hash: {e}");
                    tick(&|s| s.failed.push((abs_path.clone(), msg.clone())))?;
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

            // A same-hash path already claimed during this scan follows that
            // canonical path's outcome. This check intentionally precedes the
            // cache load: a very fast successful worker may already have saved
            // the record, but the first sorted path must remain canonical.
            let already_claimed = {
                let s = state.lock().map_err(|_| poisoned())?;
                s.hash_work.contains_key(&content_hash)
            };
            if already_claimed {
                let follower = HashFollower {
                    abs_path: abs_path.clone(),
                    rel: rel.clone(),
                    entry: entry.clone(),
                };
                tick(&|s| match s.hash_work.get_mut(&content_hash) {
                    Some(HashWork::Pending(followers)) => followers.push(follower.clone()),
                    Some(HashWork::Succeeded) => {
                        s.new_index.insert(rel.clone(), entry.clone());
                        s.reused_hash += 1;
                    }
                    Some(HashWork::Failed(message)) => {
                        s.failed.push((abs_path.clone(), message.clone()));
                    }
                    None => unreachable!("claimed content hash must retain its outcome"),
                })?;
                continue;
            }

            // Tier 2: known, FRESH hash → reuse the analysis (retag/move path).
            // A record that vanished between the stat and the load (→ None) or
            // one that is stale falls through to re-analyze, overwriting the
            // stale record with current-schema output.
            if let Some(mut rec) = cache.load_record(&content_hash)? {
                if record_is_fresh(&rec) {
                    // Refresh mutable identity (path/size) and re-save if changed.
                    if rec.source != source {
                        rec.source = source;
                        cache.save_record(&rec)?;
                    }
                    tick(&|s| {
                        s.new_index.insert(rel.clone(), entry.clone());
                        s.reused_hash += 1;
                    })?;
                    continue;
                }
            }

            // Tier 3: unseen hash → queue for the analysis workers (the total
            // grows before the send so a worker's report never overtakes it).
            tick(&|s| {
                s.analyze_total += 1;
                s.hash_work
                    .insert(content_hash.clone(), HashWork::Pending(Vec::new()));
            })?;
            let request = AnalyzeRequest {
                abs_path: abs_path.clone(),
                source,
            };
            if tx.send((request, rel, entry)).is_err() {
                return Err(crate::SonagramError::Cache(
                    "analysis workers exited early".to_string(),
                ));
            }
        }

        // Close the queue: workers drain what remains, then exit (the scope
        // joins them). From here the live stage reads "analyze".
        drop(tx);
        {
            let mut s = state.lock().map_err(|_| poisoned())?;
            s.deciding_complete = true;
            let stage = live_stage(&s);
            progress_file.write(&snapshot(&s, stage), true);
        }
        Ok(())
    })?;

    // Workers are joined; the authoritative index prunes deleted files.
    let mut s = state.into_inner().map_err(|_| poisoned())?;
    s.failed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    cache.save_index(&s.new_index)?;
    opts.report(ScanStage::Done, total_files, total_files);
    progress_file.write(
        &ScanProgressSnapshot {
            pid: std::process::id(),
            stage: stage_name(ScanStage::Done).to_string(),
            done: s.decided,
            total: total_files,
            analyze_done: s.analyze_done,
            analyze_total: s.analyze_total,
            analyzed: s.analyzed,
            reused_stat: s.reused_stat,
            reused_hash: s.reused_hash,
            failed: s.failed.len(),
            started_unix,
            updated_unix: unix_now(),
        },
        true,
    );

    Ok(ScanReport {
        total_files,
        analyzed: s.analyzed,
        reused_hash_match: s.reused_hash,
        reused_stat_match: s.reused_stat,
        failed: s.failed,
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
