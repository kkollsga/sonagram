//! Incremental-rescan integration tests.
//!
//! The core guarantee under test: **a no-op rescan performs zero analyses**, and
//! the identity model (content hash) survives retag / touch / move while a delete
//! prunes only the index.
//!
//! sonara cannot be run here (no committed audio), so the analyzer seam is
//! substituted with a [`CountingAnalyzer`] that (a) counts how many files it is
//! asked to analyze and (b) returns a canned record built from a committed
//! fixture, stamped with the `source` the scanner computed. The "library" is
//! built from tiny **synthetic** MP3s: a fabricated ID3v2 container + a few audio
//! bytes, so that editing the tag region is hash-invariant exactly as it is for
//! real files.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use sonagram::record::AnalysisRecord;
use sonagram::scan::cache::{Cache, Index, IndexEntry};
use sonagram::scan::{
    default_analysis_config, load_records, migrate_cached_record, record_is_fresh,
    record_is_fresh_for_features, scan_library, scan_library_with, AnalyzeRequest, Analyzer,
    ScanOptions, VOCALNESS_MODEL_ID,
};

/// A mock analyzer: counts analyses and returns a fixture-derived record stamped
/// with each request's `source` (so the saved record is keyed by the file hash).
struct CountingAnalyzer {
    template: AnalysisRecord,
    calls: AtomicUsize,
}

impl CountingAnalyzer {
    fn new() -> Self {
        CountingAnalyzer {
            template: load_a_fixture(),
            calls: AtomicUsize::new(0),
        }
    }
    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Analyzer for CountingAnalyzer {
    fn analyze_one(&self, request: &AnalyzeRequest) -> sonagram::Result<AnalysisRecord> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut rec = self.template.clone();
        rec.source = request.source.clone();
        Ok(rec)
    }
}

/// Keeps the canonical analysis in flight long enough for the decision loop to
/// encounter a same-hash follower, exercising the pending-work fan-out path.
struct SlowCountingAnalyzer(CountingAnalyzer);

impl SlowCountingAnalyzer {
    fn new() -> Self {
        Self(CountingAnalyzer::new())
    }

    fn count(&self) -> usize {
        self.0.count()
    }
}

impl Analyzer for SlowCountingAnalyzer {
    fn analyze_one(&self, request: &AnalyzeRequest) -> sonagram::Result<AnalysisRecord> {
        std::thread::sleep(std::time::Duration::from_millis(25));
        self.0.analyze_one(request)
    }
}

/// A mock analyzer that fails exactly one relative path and succeeds (with the
/// fixture template) everywhere else — the seam for interrupted/partial-scan
/// tests.
struct SelectiveAnalyzer {
    template: AnalysisRecord,
    fail_path: String,
    calls: AtomicUsize,
}

impl SelectiveAnalyzer {
    fn failing(rel_path: &str) -> Self {
        SelectiveAnalyzer {
            template: load_a_fixture(),
            fail_path: rel_path.to_string(),
            calls: AtomicUsize::new(0),
        }
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Analyzer for SelectiveAnalyzer {
    fn analyze_one(&self, request: &AnalyzeRequest) -> sonagram::Result<AnalysisRecord> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request.source.path == self.fail_path {
            return Err(sonagram::SonagramError::Cache("mock failure".to_string()));
        }
        let mut rec = self.template.clone();
        rec.source = request.source.clone();
        Ok(rec)
    }
}

/// Load one committed fixture record to use as the mock's analysis payload.
fn load_a_fixture() -> AnalysisRecord {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/analyses");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixtures dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let text = std::fs::read_to_string(&paths[0]).expect("read fixture");
    let mut record = AnalysisRecord::from_json(&text).expect("parse fixture");
    record.analysis.provenance.vocalness_model_id =
        Some(sonagram::scan::VOCALNESS_MODEL_ID.to_string());
    record
}

// ---- synthetic library helpers ----

/// Build a synthetic "mp3": fabricated ID3v2(tag_payload) + audio.
fn make_mp3(tag_payload: &[u8], audio: &[u8]) -> Vec<u8> {
    let body_len = tag_payload.len();
    let mut v = Vec::new();
    v.extend_from_slice(b"ID3");
    v.push(4); // major
    v.push(0); // minor
    v.push(0); // flags, no footer
    v.push(((body_len >> 21) & 0x7f) as u8);
    v.push(((body_len >> 14) & 0x7f) as u8);
    v.push(((body_len >> 7) & 0x7f) as u8);
    v.push((body_len & 0x7f) as u8);
    v.extend_from_slice(tag_payload);
    v.extend_from_slice(audio);
    v
}

fn tmp_library(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sonagram-scan-{}-{}-{}",
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

fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

fn opts() -> ScanOptions {
    ScanOptions::default()
}

/// Build a 3-track library with distinct audio payloads.
fn build_library(name: &str) -> PathBuf {
    let lib = tmp_library(name);
    write_file(&lib.join("a.mp3"), &make_mp3(b"tag-a", b"AUDIO-AAAA"));
    write_file(&lib.join("b.mp3"), &make_mp3(b"tag-b", b"AUDIO-BBBB"));
    write_file(&lib.join("sub/c.mp3"), &make_mp3(b"tag-c", b"AUDIO-CCCC"));
    lib
}

/// Build two differently tagged files whose audio payload (and therefore
/// content hash) is identical.
fn build_duplicate_library(name: &str) -> PathBuf {
    let lib = tmp_library(name);
    write_file(&lib.join("a.mp3"), &make_mp3(b"first-tag", b"SHARED-AUDIO"));
    write_file(&lib.join("b.mp3"), &make_mp3(b"second-longer-tag", b"SHARED-AUDIO"));
    lib
}

#[test]
fn bundled_vocalness_model_identity_drives_cache_freshness() {
    let config = default_analysis_config().expect("bundled model must validate");
    assert_eq!(
        config.vocalness_model.as_ref().map(|model| model.id()),
        Some(VOCALNESS_MODEL_ID)
    );

    let current = load_a_fixture();
    assert!(record_is_fresh(&current));

    let mut absent = current.clone();
    absent.analysis.provenance.vocalness_model_id = None;
    assert!(
        !record_is_fresh(&absent),
        "pre-model records must invalidate"
    );

    let mut changed = current;
    changed.analysis.provenance.vocalness_model_id = Some("future-model".to_string());
    assert!(
        !record_is_fresh(&changed),
        "a changed model id must invalidate"
    );
}

#[test]
fn requested_feature_identity_drives_cache_freshness() {
    let current = load_a_fixture();
    let mut reordered = ScanOptions::default().features;
    reordered.reverse();
    reordered.push("embedding".to_string());
    assert!(
        record_is_fresh_for_features(&current, &reordered),
        "feature order and duplicates are not analysis semantics"
    );

    let mut subset = ScanOptions::default().features;
    subset.retain(|feature| feature != "loudness");
    assert!(!record_is_fresh_for_features(&current, &subset));

    let mut missing = current;
    missing.analysis.provenance.requested_features = None;
    assert!(!record_is_fresh(&missing));

    let mut foreign_genre = load_a_fixture();
    foreign_genre.analysis.provenance.genre_model_id = Some("genre-v1".to_string());
    foreign_genre.analysis.genre = Some("rock".to_string());
    foreign_genre.analysis.genre_confidence = Some(0.9);
    assert!(!record_is_fresh(&foreign_genre));
}

#[test]
fn changed_requested_features_bypass_both_cache_reuse_tiers() {
    let lib = build_library("feature-change");
    let initial = CountingAnalyzer::new();
    scan_library_with(&lib, &ScanOptions::default(), &initial).unwrap();

    let mut subset_options = ScanOptions::default();
    subset_options
        .features
        .retain(|feature| feature != "loudness");
    let changed = CountingAnalyzer::new();
    let report = scan_library_with(&lib, &subset_options, &changed).unwrap();
    assert_eq!(report.reused_stat_match, 0);
    assert_eq!(report.reused_hash_match, 0);
    assert_eq!(report.analyzed, 3);
    assert_eq!(changed.count(), 3);
}

#[test]
fn schema_three_cache_migrates_exactly_from_stored_features() {
    let expected = load_a_fixture();
    let mut legacy = expected.clone();
    legacy.analysis.provenance.schema_version = 3;
    legacy.analysis.provenance.vocalness_model_id = None;
    legacy.analysis.vocalness = Some(0.0);
    legacy.analysis.instrumentalness = Some(1.0);
    legacy.analysis.chord_change_rate = Some(-1.0);
    legacy.analysis.predominant_chord = Some("G#m".to_string());

    let model = sonara::vocal_model::bundled().unwrap();
    let features = ScanOptions::default().features;
    assert!(migrate_cached_record(&mut legacy, &features, &model));
    assert_eq!(legacy.analysis.provenance, expected.analysis.provenance);
    assert_eq!(legacy.analysis.vocalness, expected.analysis.vocalness);
    assert_eq!(
        legacy.analysis.instrumentalness,
        expected.analysis.instrumentalness
    );
    assert_eq!(
        legacy.analysis.chord_change_rate,
        expected.analysis.chord_change_rate
    );
    assert_eq!(
        legacy.analysis.predominant_chord,
        expected.analysis.predominant_chord
    );
    assert!(record_is_fresh(&legacy));
    assert!(
        !migrate_cached_record(&mut legacy, &features, &model),
        "current records are an idempotent no-op"
    );
}

#[test]
fn schema_four_cache_without_model_provenance_is_migrated() {
    let expected = load_a_fixture();
    let mut unstamped = expected.clone();
    unstamped.analysis.provenance.vocalness_model_id = None;
    unstamped.analysis.vocalness = Some(0.0);
    unstamped.analysis.instrumentalness = Some(1.0);

    let model = sonara::vocal_model::bundled().unwrap();
    let features = ScanOptions::default().features;
    assert!(migrate_cached_record(&mut unstamped, &features, &model));
    assert_eq!(unstamped.analysis.vocalness, expected.analysis.vocalness);
    assert_eq!(
        unstamped.analysis.instrumentalness,
        expected.analysis.instrumentalness
    );
    assert!(record_is_fresh(&unstamped));
}

#[test]
fn schema_four_v1_cache_migrates_to_v2_without_audio() {
    let expected = load_a_fixture();
    let mut v1 = expected.clone();
    v1.analysis.provenance.vocalness_model_id =
        Some("sonara-vocalness-v1".to_string());
    v1.analysis.vocalness = Some(0.0);
    v1.analysis.instrumentalness = Some(1.0);

    let model = sonara::vocal_model::bundled().unwrap();
    let features = ScanOptions::default().features;
    assert!(migrate_cached_record(&mut v1, &features, &model));
    assert_eq!(v1.analysis.provenance, expected.analysis.provenance);
    assert_eq!(v1.analysis.vocalness, expected.analysis.vocalness);
    assert_eq!(
        v1.analysis.instrumentalness,
        expected.analysis.instrumentalness
    );
    assert!(record_is_fresh(&v1));
}

#[test]
fn cache_migration_rejects_incomplete_or_foreign_inputs() {
    let model = sonara::vocal_model::bundled().unwrap();
    let features = ScanOptions::default().features;
    let mut legacy = load_a_fixture();
    legacy.analysis.provenance.schema_version = 3;
    legacy.analysis.provenance.vocalness_model_id = None;

    let mut missing_embedding = legacy.clone();
    missing_embedding.analysis.embedding = None;
    assert!(!migrate_cached_record(
        &mut missing_embedding,
        &features,
        &model
    ));

    let mut foreign_model = legacy.clone();
    foreign_model.analysis.provenance.vocalness_model_id = Some("other-model".to_string());
    assert!(!migrate_cached_record(
        &mut foreign_model,
        &features,
        &model
    ));

    let mut hidden_genre = legacy;
    hidden_genre.analysis.genre = Some("rock".to_string());
    hidden_genre.analysis.genre_confidence = Some(0.9);
    assert!(!migrate_cached_record(
        &mut hidden_genre,
        &features,
        &model
    ));
}

#[test]
fn production_scan_migrates_eligible_cache_without_decoding_audio() {
    let lib = tmp_library("cached-migration");
    let path = lib.join("a.mp3");
    write_file(&path, &make_mp3(b"tag", b"not-decodable-audio"));
    let metadata = std::fs::metadata(&path).unwrap();
    let mtime_unix = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut legacy = load_a_fixture();
    legacy.source.content_hash = "cached-hash".to_string();
    legacy.source.path = "a.mp3".to_string();
    legacy.source.file_size = metadata.len();
    legacy.analysis.provenance.schema_version = 3;
    legacy.analysis.provenance.vocalness_model_id = None;
    legacy.analysis.vocalness = Some(0.0);
    legacy.analysis.instrumentalness = Some(1.0);
    legacy.analysis.predominant_chord = Some("G#m".to_string());

    let cache = Cache::new(&lib);
    cache.save_record(&legacy).unwrap();
    let mut index = Index::new();
    index.insert(
        "a.mp3".to_string(),
        IndexEntry {
            size: metadata.len(),
            mtime_unix,
            content_hash: "cached-hash".to_string(),
        },
    );
    cache.save_index(&index).unwrap();

    let report = scan_library(&lib, &ScanOptions::default()).unwrap();
    assert_eq!(report.migrated_analysis, 1);
    assert_eq!(report.analyzed, 0, "synthetic audio must never be decoded");
    assert_eq!(report.reused_stat_match, 1);
    assert!(report.failed.is_empty());
    let migrated = cache.load_record("cached-hash").unwrap().unwrap();
    assert!(record_is_fresh(&migrated));
}

#[test]
fn duplicate_hash_is_analyzed_once_and_fanned_out_deterministically() {
    let lib = build_duplicate_library("duplicate-hash");
    let analyzer = CountingAnalyzer::new();

    let first = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(first.total_files, 2);
    assert_eq!(first.analyzed, 1, "analysis work is counted per unique hash");
    assert_eq!(first.reused_hash_match, 1, "the alias follows the canonical result");
    assert_eq!(analyzer.count(), 1, "same-hash paths must never race into two analyses");
    assert!(first.failed.is_empty());

    let cache = Cache::new(&lib);
    let index = cache.load_index().unwrap();
    assert_eq!(index.len(), 2, "both paths are indexed");
    assert_eq!(index["a.mp3"].content_hash, index["b.mp3"].content_hash);
    let records = load_records(&lib).unwrap();
    assert_eq!(records.len(), 1, "one content hash persists one record");
    assert_eq!(records[0].source.path, "a.mp3", "first sorted path is canonical");

    let progress = sonagram::scan::load_scan_progress(&lib).unwrap();
    assert_eq!(progress.analyze_total, 1);
    assert_eq!(progress.analyze_done, 1);
    assert_eq!(progress.analyzed, 1);
    assert_eq!(progress.reused_hash, 1);

    let second = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(second.analyzed, 0);
    assert_eq!(second.reused_stat_match, 2);
    assert_eq!(analyzer.count(), 1, "the follow-up scan is a true no-op");
}

#[test]
fn in_flight_duplicate_hash_is_not_queued_to_a_second_worker() {
    let lib = build_duplicate_library("duplicate-in-flight");
    let analyzer = SlowCountingAnalyzer::new();

    let report = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(analyzer.count(), 1);
    assert_eq!(report.analyzed, 1);
    assert_eq!(report.reused_hash_match, 1);
    assert_eq!(Cache::new(&lib).load_index().unwrap().len(), 2);
}

#[test]
fn duplicate_hash_failure_fans_out_and_retries_once() {
    let lib = build_duplicate_library("duplicate-failure");
    let failing = SelectiveAnalyzer::failing("a.mp3");

    let first = scan_library_with(&lib, &opts(), &failing).unwrap();
    assert_eq!(failing.count(), 1, "the canonical failure is shared");
    assert_eq!(first.analyzed, 0);
    assert_eq!(first.failed.len(), 2);
    let failed_paths: Vec<&str> = first
        .failed
        .iter()
        .map(|(path, _)| path.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(failed_paths, ["a.mp3", "b.mp3"], "failures stay path-sorted");
    assert!(first.failed.iter().all(|(_, message)| message == "cache error: mock failure"));
    assert!(Cache::new(&lib).load_index().unwrap().is_empty());

    let recovering = CountingAnalyzer::new();
    let second = scan_library_with(&lib, &opts(), &recovering).unwrap();
    assert_eq!(recovering.count(), 1, "retry still analyzes the unique hash once");
    assert_eq!(second.analyzed, 1);
    assert_eq!(second.reused_hash_match, 1);
    assert!(second.failed.is_empty());
    assert_eq!(Cache::new(&lib).load_index().unwrap().len(), 2);
}

#[test]
fn first_scan_analyzes_all_then_noop_analyzes_zero() {
    let lib = build_library("noop");
    let analyzer = CountingAnalyzer::new();

    let r1 = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r1.total_files, 3);
    assert_eq!(r1.analyzed, 3, "first scan analyzes every file");
    assert_eq!(analyzer.count(), 3);
    assert!(r1.failed.is_empty(), "unexpected failures: {:?}", r1.failed);

    // Immediate rescan: stat fast-path serves everything, zero analyses.
    let r2 = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r2.analyzed, 0, "no-op rescan must analyze nothing");
    assert_eq!(r2.reused_stat_match, 3);
    assert_eq!(r2.reused_hash_match, 0);
    assert_eq!(analyzer.count(), 3, "analyzer call count unchanged on no-op");
}

#[test]
fn touch_mtime_rehashes_but_zero_analyses() {
    let lib = build_library("touch");
    let analyzer = CountingAnalyzer::new();
    scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(analyzer.count(), 3);

    // Bump mtime without changing bytes: stat fast-path misses → re-hash → hash
    // still matches an existing record → reused, zero analyses.
    let a = lib.join("a.mp3");
    let future = filetime::FileTime::from_unix_time(2_000_000_000, 0);
    filetime::set_file_mtime(&a, future).unwrap();

    let r = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r.analyzed, 0);
    assert_eq!(r.reused_hash_match, 1, "the touched file re-hashed and reused");
    assert_eq!(r.reused_stat_match, 2, "the other two hit the stat fast-path");
    assert_eq!(analyzer.count(), 3, "no new analyses from a touch");
}

#[test]
fn tag_edit_is_hash_invariant_zero_analyses() {
    let lib = build_library("retag");
    let analyzer = CountingAnalyzer::new();
    scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(analyzer.count(), 3);

    // Rewrite a.mp3 with a *different, longer* tag payload but identical audio.
    // Size and mtime change (fast-path misses) but the ID3-stripped hash is equal.
    write_file(&lib.join("a.mp3"), &make_mp3(b"a-much-longer-tag-payload-xyz", b"AUDIO-AAAA"));

    let r = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r.analyzed, 0, "retag must not trigger analysis");
    assert_eq!(r.reused_hash_match, 1);
    assert_eq!(analyzer.count(), 3);

    // The record's stored size reflects the new (larger) file.
    let records = load_records(&lib).unwrap();
    let a_rec = records
        .iter()
        .find(|r| r.source.path == "a.mp3")
        .expect("a.mp3 record present");
    assert_eq!(a_rec.source.file_size as usize, make_mp3(b"a-much-longer-tag-payload-xyz", b"AUDIO-AAAA").len());
}

#[test]
fn changed_audio_triggers_one_analysis() {
    let lib = build_library("changed-audio");
    let analyzer = CountingAnalyzer::new();
    scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(analyzer.count(), 3);

    // Change the audio bytes of b.mp3 → new hash → one fresh analysis.
    write_file(&lib.join("b.mp3"), &make_mp3(b"tag-b", b"AUDIO-DIFFERENT"));
    let r = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r.analyzed, 1);
    assert_eq!(analyzer.count(), 4);
}

#[test]
fn rename_reuses_analysis_and_updates_path() {
    let lib = build_library("rename");
    let analyzer = CountingAnalyzer::new();
    scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(analyzer.count(), 3);

    // Move a.mp3 → renamed.mp3 (same bytes). New path, unchanged hash.
    std::fs::rename(lib.join("a.mp3"), lib.join("renamed.mp3")).unwrap();
    let r = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r.analyzed, 0, "a rename must not re-analyze");
    assert_eq!(r.reused_hash_match, 1);
    assert_eq!(analyzer.count(), 3);

    // The record's path now reflects the new name.
    let records = load_records(&lib).unwrap();
    assert!(records.iter().any(|r| r.source.path == "renamed.mp3"));
    assert!(!records.iter().any(|r| r.source.path == "a.mp3"));
}

#[test]
fn delete_prunes_index_but_keeps_record() {
    let lib = build_library("delete");
    let analyzer = CountingAnalyzer::new();
    scan_library_with(&lib, &opts(), &analyzer).unwrap();

    // Record count before delete.
    let before = load_records(&lib).unwrap();
    assert_eq!(before.len(), 3);
    let deleted_hash = before
        .iter()
        .find(|record| record.source.path == "b.mp3")
        .unwrap()
        .source
        .content_hash
        .clone();

    std::fs::remove_file(lib.join("b.mp3")).unwrap();
    let r = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r.total_files, 2, "deleted file no longer discovered");
    assert_eq!(r.analyzed, 0);

    // The content-addressed JSON remains reusable on disk, but the authoritative
    // index no longer references it, so graph inputs omit the orphan.
    let after = load_records(&lib).unwrap().len();
    assert_eq!(after, 2, "deleted orphan must not re-enter graph inputs");
    assert!(
        Cache::new(&lib).has_record(&deleted_hash),
        "orphan remains cached on disk"
    );
}

#[test]
fn failed_stale_reanalysis_does_not_reenter_graph_inputs() {
    let lib = build_library("stale-failed");
    scan_library_with(&lib, &opts(), &CountingAnalyzer::new()).unwrap();

    mutate_record(&lib, "b.mp3", |record| {
        record.analysis.provenance.schema_version = 0
    });
    let report = scan_library_with(&lib, &opts(), &SelectiveAnalyzer::failing("b.mp3")).unwrap();
    assert_eq!(
        report.failed.len(),
        1,
        "stale record refresh fails explicitly"
    );

    let records = load_records(&lib).unwrap();
    assert_eq!(records.len(), 2);
    assert!(!records.iter().any(|record| record.source.path == "b.mp3"));
}

#[test]
fn missing_indexed_record_is_a_clear_cache_error() {
    let lib = build_library("missing-indexed-record");
    scan_library_with(&lib, &opts(), &CountingAnalyzer::new()).unwrap();
    let cache = Cache::new(&lib);
    let index = cache.load_index().unwrap();
    let missing_hash = index["b.mp3"].content_hash.clone();
    std::fs::remove_file(cache.record_path(&missing_hash)).unwrap();

    let message = load_records(&lib).unwrap_err().to_string();
    assert!(
        message.contains("b.mp3"),
        "affected indexed path is named: {message}"
    );
    assert!(
        message.contains(&missing_hash),
        "missing content hash is named: {message}"
    );
    assert!(
        message.contains("sonagram scan"),
        "repair action is named: {message}"
    );
}

fn save_permuted_cache(lib: &Path, order: &[usize]) {
    let cache = Cache::new(lib);
    let hashes = ["cc", "aa", "bb"];
    let paths = ["c.mp3", "a.mp3", "b.mp3"];
    let template = load_a_fixture();
    let mut index = Index::new();
    for &i in order {
        let mut record = template.clone();
        record.source.content_hash = hashes[i].to_string();
        record.source.path = paths[i].to_string();
        cache.save_record(&record).unwrap();
        index.insert(
            paths[i].to_string(),
            IndexEntry {
                size: i as u64 + 1,
                mtime_unix: i as i64 + 10,
                content_hash: hashes[i].to_string(),
            },
        );
    }
    // A second path for `aa` verifies that indexed aliases still load one row.
    index.insert(
        "alias-a.mp3".to_string(),
        IndexEntry {
            size: 99,
            mtime_unix: 99,
            content_hash: "aa".to_string(),
        },
    );
    cache.save_index(&index).unwrap();
}

fn records_digest(records: &[AnalysisRecord]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    for record in records {
        hasher.update(record.to_json_pretty().unwrap().as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize()
}

#[test]
fn indexed_record_order_and_digest_ignore_input_permutation() {
    let first = tmp_library("index-order-a");
    let second = tmp_library("index-order-b");
    save_permuted_cache(&first, &[0, 1, 2]);
    save_permuted_cache(&second, &[2, 0, 1]);

    let a = load_records(&first).unwrap();
    let b = load_records(&second).unwrap();
    let hashes: Vec<&str> = a
        .iter()
        .map(|record| record.source.content_hash.as_str())
        .collect();
    assert_eq!(hashes, ["aa", "bb", "cc"]);
    assert_eq!(a.len(), 3, "duplicate indexed hash loads once");
    assert_eq!(records_digest(&a), records_digest(&b));
}

/// Rewrite the on-disk record whose `source.path` is `rel_path`, applying `f`
/// (e.g. to age its schema/embedding version), simulating a record left behind
/// by an older sonara build.
fn mutate_record(lib: &Path, rel_path: &str, f: impl Fn(&mut AnalysisRecord)) {
    let dir = lib.join(".sonagram/analysis");
    for entry in std::fs::read_dir(&dir).expect("analysis dir") {
        let p = entry.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let mut rec = AnalysisRecord::from_json(&std::fs::read_to_string(&p).unwrap()).unwrap();
        if rec.source.path == rel_path {
            f(&mut rec);
            std::fs::write(&p, rec.to_json_pretty().unwrap()).unwrap();
            return;
        }
    }
    panic!("no record found for {rel_path}");
}

/// A record produced by an OLDER sonara (mismatched analysis-schema or embedding
/// version) is stale: it must be re-analyzed, not trusted — while fresh records
/// stay on the zero-cost stat fast-path. This is the gap the sonara 0.2.3 chroma
/// bump exposed (old records carried v1 semantics the graph must not keep).
#[test]
fn stale_records_reanalyzed_fresh_untouched() {
    let lib = build_library("stale");
    let analyzer = CountingAnalyzer::new();
    scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(analyzer.count(), 3, "first scan analyzes all three");

    // The mock's template is a committed fixture recaptured at the CURRENT sonara
    // versions, so the three saved records are all fresh. Age two of them via the
    // two independent staleness triggers the scanner checks.
    mutate_record(&lib, "a.mp3", |r| r.analysis.provenance.schema_version = 0);
    mutate_record(&lib, "sub/c.mp3", |r| {
        r.analysis.embedding_version = Some(999)
    });

    // Rescan: both stale files' stat fast-paths are rejected on freshness and
    // they are re-analyzed; the one fresh file stays on the stat fast-path.
    let r = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r.analyzed, 2, "both stale files are re-analyzed");
    assert_eq!(r.reused_stat_match, 1, "the one fresh file is untouched");
    assert_eq!(analyzer.count(), 5, "two new analyses total");

    // Re-analysis rewrote current-schema records → a further rescan is a true
    // no-op: all fresh, zero analyses.
    let r2 = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r2.analyzed, 0, "all records fresh again → no-op analyzes nothing");
    assert_eq!(r2.reused_stat_match, 3);
    assert_eq!(analyzer.count(), 5, "no further analyses on the no-op rescan");
}

/// P20 streaming persistence: results that completed before a failure are on
/// disk and survive — a follow-up scan re-analyzes ONLY what is missing.
#[test]
fn partial_scan_resumes_only_missing_work() {
    let lib = build_library("resume");
    let analyzer = SelectiveAnalyzer::failing("b.mp3");

    let r1 = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r1.analyzed, 2, "the two passing files are analyzed");
    assert_eq!(r1.failed.len(), 1, "the failing file is isolated");
    assert_eq!(
        load_records(&lib).unwrap().len(),
        2,
        "completed records are persisted individually"
    );

    // Next scan: the two persisted files ride the stat fast-path; only the
    // missing one is analyzed.
    let analyzer2 = CountingAnalyzer::new();
    let r2 = scan_library_with(&lib, &opts(), &analyzer2).unwrap();
    assert_eq!(r2.analyzed, 1, "only the previously failed file re-analyzes");
    assert_eq!(r2.reused_stat_match, 2);
    assert_eq!(analyzer2.count(), 1);
    assert_eq!(load_records(&lib).unwrap().len(), 3);
}

/// P20 observable progress: every scan (whatever the entry point) leaves a
/// final `scan_progress.json` snapshot with stage `"done"` and true counts.
#[test]
fn progress_snapshot_written_and_finalized() {
    let lib = build_library("progress");
    let analyzer = CountingAnalyzer::new();
    scan_library_with(&lib, &opts(), &analyzer).unwrap();

    let p = sonagram::scan::load_scan_progress(&lib).expect("progress snapshot exists");
    assert_eq!(p.stage, "done");
    assert_eq!(p.total, 3);
    assert_eq!(p.done, 3, "every file passed the decision loop");
    assert_eq!(p.analyzed, 3);
    assert_eq!(p.analyze_done, 3);
    assert_eq!(p.analyze_total, 3);
    assert_eq!(p.failed, 0);
    assert!(p.updated_unix >= p.started_unix);
    assert_eq!(p.pid, std::process::id());
}

#[test]
fn scan_tolerates_missing_cache_dir() {
    // A fresh library with no .sonagram/ yet must scan cleanly.
    let lib = build_library("fresh");
    let analyzer = CountingAnalyzer::new();
    let r = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r.analyzed, 3);
    assert!(lib.join(".sonagram/index.json").exists());
}
