//! End-to-end test of the `sonagram status` subcommand through the **built
//! binary** (`CARGO_BIN_EXE_sonagram`), exercising the freshness exit codes and
//! the `--format json` object a skill would parse.
//!
//! The synthetic mini-cache uses dummy `.mp3` files (the probe never reads their
//! audio) plus a hand-built index + fixture records, so no sonara is needed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use sonagram::record::AnalysisRecord;
use sonagram::scan::cache::{Cache, IndexEntry};

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
    // `scan::is_fresh` compares BOTH the vocalness model id (stamped above) and
    // the analysis schema version, each by exact equality. This helper's whole
    // contract is "a record the CURRENT build considers fresh", so it must
    // normalize both. Leaving the schema unstamped was latent: it only bit when
    // sonara moved ANALYSIS_SCHEMA_VERSION 4 -> 6 in 0.3.4, at which point every
    // fixture-derived record read as stale and 18 tests across scan_incremental,
    // p19_bootstrap, cli_status and status_probe failed at once. Tests that
    // deliberately want an OLD schema override this field explicitly after
    // calling the helper, so stamping here is safe.
    record.analysis.provenance.schema_version = sonara::analyze::ANALYSIS_SCHEMA_VERSION;
    record
}

fn tmp_lib(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sonagram-clistatus-{}-{}-{}",
        std::process::id(),
        name,
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_mp3(lib: &Path, rel: &str, bytes: &[u8]) -> (u64, i64) {
    let path = lib.join(rel);
    std::fs::write(&path, bytes).unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    let mtime = match meta.modified().unwrap().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    (meta.len(), mtime)
}

fn fresh_record(hash: &str, rel: &str) -> AnalysisRecord {
    let mut rec = load_a_fixture();
    rec.source.content_hash = hash.to_string();
    rec.source.path = rel.to_string();
    rec
}

/// Run `sonagram status <lib> --format json` and return `(exit_code, parsed)`.
fn run_status_json(lib: &Path) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_sonagram"))
        .args(["status", &lib.to_string_lossy(), "--format", "json"])
        .output()
        .expect("run sonagram binary");
    let code = out.status.code().expect("exit code");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("status stdout is valid JSON");
    (code, parsed)
}

#[test]
fn status_no_cache_exits_two() {
    let lib = tmp_lib("nocache");
    write_mp3(&lib, "a.mp3", b"aaaa");
    let (code, json) = run_status_json(&lib);
    assert_eq!(code, 2, "no cache ⇒ exit 2");
    assert_eq!(json["status"], "no_cache");
    assert_eq!(json["has_cache"], false);
    assert_eq!(json["exit_code"], 2);
}

#[test]
fn status_fresh_exits_zero() {
    let lib = tmp_lib("fresh");
    let cache = Cache::new(&lib);
    let (size, mtime) = write_mp3(&lib, "a.mp3", b"hello");
    cache.save_record(&fresh_record("h0", "a.mp3")).unwrap();
    let mut index: BTreeMap<String, IndexEntry> = BTreeMap::new();
    index.insert(
        "a.mp3".to_string(),
        IndexEntry { size, mtime_unix: mtime, content_hash: "h0".to_string() },
    );
    cache.save_index(&index).unwrap();

    let (code, json) = run_status_json(&lib);
    assert_eq!(code, 0, "all fresh ⇒ exit 0");
    assert_eq!(json["status"], "fresh");
    assert_eq!(json["fresh"], 1);
    assert_eq!(json["needs_scan"], false);
    // The current sonara versions are surfaced for transparency.
    assert!(json["schema_version"].is_number());
    assert!(json["similarity_version"].is_number());
}

#[test]
fn status_needs_scan_exits_one() {
    let lib = tmp_lib("needsscan");
    let cache = Cache::new(&lib);
    let (size, mtime) = write_mp3(&lib, "a.mp3", b"hello");
    cache.save_record(&fresh_record("h0", "a.mp3")).unwrap();
    let mut index: BTreeMap<String, IndexEntry> = BTreeMap::new();
    index.insert(
        "a.mp3".to_string(),
        IndexEntry { size, mtime_unix: mtime, content_hash: "h0".to_string() },
    );
    cache.save_index(&index).unwrap();
    // An unindexed file forces "needs scan".
    write_mp3(&lib, "b.mp3", b"new-file");

    let (code, json) = run_status_json(&lib);
    assert_eq!(code, 1, "unindexed file ⇒ exit 1");
    assert_eq!(json["status"], "needs_scan");
    assert_eq!(json["missing_from_index"], 1);
    assert_eq!(json["needs_scan"], true);
}
