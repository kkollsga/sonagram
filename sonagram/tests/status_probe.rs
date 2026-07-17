//! Classification tests for the read-only `probe_freshness` status probe.
//!
//! The probe never hashes files or runs analysis — it only stats `*.mp3` files
//! and compares them to the cached `index.json` + records. So the "library" here
//! is a handful of **dummy** `.mp3` files (arbitrary bytes; the probe never reads
//! their audio), a hand-built index, and fixture-derived records. This isolates
//! the fresh / stale / missing / deleted classification with no sonara.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sonagram::record::AnalysisRecord;
use sonagram::scan::cache::{Cache, IndexEntry};
use sonagram::scan::probe_freshness;

/// Load one committed fixture record to use as a record payload.
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

fn tmp_lib(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sonagram-status-{}-{}-{}",
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

/// Write a dummy `.mp3` (arbitrary bytes) and return its `(size, mtime_unix)`.
fn write_mp3(lib: &Path, rel: &str, bytes: &[u8]) -> (u64, i64) {
    let path = lib.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, bytes).unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    (meta.len(), mtime_unix(&meta))
}

fn mtime_unix(meta: &std::fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64),
        },
        Err(_) => 0,
    }
}

/// A fixture record stamped with `hash`/`rel` and an optional schema override
/// (to force staleness).
fn record(hash: &str, rel: &str, schema_override: Option<u32>) -> AnalysisRecord {
    let mut rec = load_a_fixture();
    rec.source.content_hash = hash.to_string();
    rec.source.path = rel.to_string();
    if let Some(v) = schema_override {
        rec.analysis.provenance.schema_version = v;
    }
    rec
}

#[test]
fn no_cache_reports_absent() {
    let lib = tmp_lib("nocache");
    write_mp3(&lib, "a.mp3", b"aaaa");
    let r = probe_freshness(&lib).unwrap();
    assert!(!r.has_cache, "no index.json ⇒ no cache");
    assert_eq!(r.total_files, 1);
    assert_eq!(r.missing_from_index, 1);
    assert_eq!(r.fresh, 0);
}

#[test]
fn classifies_fresh_stale_missing_deleted() {
    let lib = tmp_lib("mixed");
    let cache = Cache::new(&lib);
    let mut index: BTreeMap<String, IndexEntry> = BTreeMap::new();

    // fresh: file present, stats match the index, record present + current-schema.
    let (size, mtime) = write_mp3(&lib, "fresh.mp3", b"fresh-bytes");
    cache.save_record(&record("h_fresh", "fresh.mp3", None)).unwrap();
    index.insert(
        "fresh.mp3".to_string(),
        IndexEntry { size, mtime_unix: mtime, content_hash: "h_fresh".to_string() },
    );

    // stale (stats changed): mtime in the index no longer matches the file.
    let (size2, mtime2) = write_mp3(&lib, "stale_stat.mp3", b"stale-stat");
    cache.save_record(&record("h_stat", "stale_stat.mp3", None)).unwrap();
    index.insert(
        "stale_stat.mp3".to_string(),
        IndexEntry { size: size2, mtime_unix: mtime2 + 5000, content_hash: "h_stat".to_string() },
    );

    // stale (record stale): stats match but the record is an older schema.
    let (size3, mtime3) = write_mp3(&lib, "stale_rec.mp3", b"stale-rec");
    cache.save_record(&record("h_rec", "stale_rec.mp3", Some(999))).unwrap();
    index.insert(
        "stale_rec.mp3".to_string(),
        IndexEntry { size: size3, mtime_unix: mtime3, content_hash: "h_rec".to_string() },
    );

    // missing_from_index: on disk, no index entry.
    write_mp3(&lib, "new.mp3", b"brand-new");

    // deleted_in_index: index entry with no file on disk.
    index.insert(
        "gone.mp3".to_string(),
        IndexEntry { size: 1, mtime_unix: 1, content_hash: "h_gone".to_string() },
    );

    cache.save_index(&index).unwrap();

    let r = probe_freshness(&lib).unwrap();
    assert!(r.has_cache);
    assert_eq!(r.total_files, 4, "four *.mp3 on disk");
    assert_eq!(r.fresh, 1);
    assert_eq!(r.stale, 2, "stat-changed + record-stale");
    assert_eq!(r.missing_from_index, 1);
    assert_eq!(r.deleted_in_index, 1);
}

#[test]
fn all_fresh_after_matching_index() {
    let lib = tmp_lib("allfresh");
    let cache = Cache::new(&lib);
    let mut index: BTreeMap<String, IndexEntry> = BTreeMap::new();
    for (i, rel) in ["a.mp3", "sub/b.mp3"].iter().enumerate() {
        let (size, mtime) = write_mp3(&lib, rel, format!("bytes-{i}").as_bytes());
        let hash = format!("h{i}");
        cache.save_record(&record(&hash, rel, None)).unwrap();
        index.insert(
            rel.to_string(),
            IndexEntry { size, mtime_unix: mtime, content_hash: hash },
        );
    }
    cache.save_index(&index).unwrap();

    let r = probe_freshness(&lib).unwrap();
    assert!(r.has_cache);
    assert_eq!(r.fresh, 2);
    assert_eq!(r.stale, 0);
    assert_eq!(r.missing_from_index, 0);
    assert_eq!(r.deleted_in_index, 0);
}
