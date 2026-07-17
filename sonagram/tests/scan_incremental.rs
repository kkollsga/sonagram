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
use sonagram::scan::{
    load_records, scan_library_with, AnalyzeRequest, Analyzer, ScanOptions,
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
    fn analyze(
        &self,
        requests: &[AnalyzeRequest],
        on_done: &(dyn Fn(usize, usize) + Sync),
    ) -> Vec<sonagram::Result<AnalysisRecord>> {
        self.calls.fetch_add(requests.len(), Ordering::SeqCst);
        let total = requests.len();
        requests
            .iter()
            .enumerate()
            .map(|(i, req)| {
                let mut rec = self.template.clone();
                rec.source = req.source.clone();
                on_done(i + 1, total);
                Ok(rec)
            })
            .collect()
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
    AnalysisRecord::from_json(&text).expect("parse fixture")
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
    let before = load_records(&lib).unwrap().len();
    assert_eq!(before, 3);

    std::fs::remove_file(lib.join("b.mp3")).unwrap();
    let r = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r.total_files, 2, "deleted file no longer discovered");
    assert_eq!(r.analyzed, 0);

    // Orphaned analysis/*.json records are content-addressed and intentionally
    // kept — the index no longer references the deleted file, but its record
    // remains on disk.
    let after = load_records(&lib).unwrap().len();
    assert_eq!(after, 3, "orphaned record is kept (content-addressed)");
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

#[test]
fn scan_tolerates_missing_cache_dir() {
    // A fresh library with no .sonagram/ yet must scan cleanly.
    let lib = build_library("fresh");
    let analyzer = CountingAnalyzer::new();
    let r = scan_library_with(&lib, &opts(), &analyzer).unwrap();
    assert_eq!(r.analyzed, 3);
    assert!(lib.join(".sonagram/index.json").exists());
}
