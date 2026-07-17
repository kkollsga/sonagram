//! P19 cold-start bootstrap: end-to-end tests through the **built binary** plus
//! graph-level assertions on the `Source.scan_fingerprint` property.
//!
//! Covered:
//! - `sonagram skill show` prints the embedded skill (non-empty, self-named).
//! - `sonagram skill install --dir <tmp>` writes the file.
//! - `sonagram status` graph freshness transitions: build → fresh, rescan cache
//!   without rebuilding → graph STALE (exit 1 even with fresh caches), rebuild →
//!   fresh again.
//! - The scan_fingerprint is stamped on a `Source` node when a source carries one
//!   and is **absent** for a fixture-style build (so the golden is unchanged).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use kglite::api::cypher::resolve_node_property;
use kglite::api::{DirGraph, Value};
use sonagram::graph::{self, LibraryInfo, SourceInput};
use sonagram::record::AnalysisRecord;
use sonagram::scan::cache::{Cache, Index, IndexEntry};

// ─────────────────────────── shared helpers ─────────────────────────────────

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sonagram-p19-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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

fn stat(path: &Path) -> (u64, i64) {
    let meta = std::fs::metadata(path).unwrap();
    let mtime = match meta.modified().unwrap().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    (meta.len(), mtime)
}

/// Write `bytes` to `<lib>/a.mp3`, then (re)build the cache index + a fixture
/// record so a subsequent `sonagram build` finds one Track and the index carries
/// a fresh scan_fingerprint.
fn build_cache(lib: &Path, bytes: &[u8]) {
    let path = lib.join("a.mp3");
    std::fs::write(&path, bytes).unwrap();
    let (size, mtime) = stat(&path);

    let cache = Cache::new(lib);
    let mut rec = load_a_fixture();
    rec.source.content_hash = "h0".to_string();
    rec.source.path = "a.mp3".to_string();
    cache.save_record(&rec).unwrap();

    let mut index: Index = BTreeMap::new();
    index.insert(
        "a.mp3".to_string(),
        IndexEntry { size, mtime_unix: mtime, content_hash: "h0".to_string() },
    );
    cache.save_index(&index).unwrap();
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sonagram"))
}

// ───────────────────────── skill show / install ─────────────────────────────

#[test]
fn skill_show_prints_embedded_skill() {
    let out = bin().args(["skill", "show"]).output().expect("run binary");
    assert!(out.status.success(), "skill show exits 0");
    let text = String::from_utf8(out.stdout).expect("utf-8 skill");
    assert!(!text.is_empty(), "skill show is non-empty");
    assert!(text.contains("sonagram-playlist"), "names the skill");
    // The library-detection ladder (P19) rides along in the embedded copy.
    assert!(text.contains("Library detection"), "carries the detection ladder");
}

#[test]
fn skill_install_writes_file_to_dir() {
    let home = tmp_dir("skill-home");
    let skills = tmp_dir("skill-root");
    let out = bin()
        .env("SONAGRAM_HOME", &home)
        .args(["skill", "install", "--dir"])
        .arg(&skills)
        .output()
        .expect("run binary");
    assert!(out.status.success(), "skill install exits 0: {:?}", out);
    let file = skills.join("sonagram-playlist").join("SKILL.md");
    assert!(file.exists(), "SKILL.md written");
    let body = std::fs::read_to_string(&file).unwrap();
    assert!(body.contains("name: sonagram-playlist"));
    // Re-install without --force fails; with --force succeeds.
    let again = bin()
        .env("SONAGRAM_HOME", &home)
        .args(["skill", "install", "--dir"])
        .arg(&skills)
        .output()
        .unwrap();
    assert!(!again.status.success(), "refuses to overwrite without --force");
}

// ────────────────────────── status graph_stale ──────────────────────────────

fn status_json(home: &Path) -> (i32, serde_json::Value) {
    let out = bin()
        .env("SONAGRAM_HOME", home)
        .args(["status", "--format", "json"])
        .output()
        .expect("run status");
    let code = out.status.code().expect("exit code");
    let parsed = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("status stdout not JSON: {e}\nstdout={:?}\nstderr={:?}", out.stdout, out.stderr)
    });
    (code, parsed)
}

fn run_ok(home: &Path, args: &[&str]) {
    let out = bin().env("SONAGRAM_HOME", home).args(args).output().expect("run");
    assert!(out.status.success(), "{args:?} failed: {:?}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn status_reports_graph_stale_transitions() {
    let home = tmp_dir("home");
    let lib = tmp_dir("lib");
    build_cache(&lib, b"original-audio-bytes");

    // Register the source + build the graph (config-driven → SONAGRAM_HOME/music.kgl).
    run_ok(&home, &["sources", "add", &lib.to_string_lossy()]);
    run_ok(&home, &["build"]);

    // 1) Fresh right after build: caches fresh, graph current.
    let (code, j) = status_json(&home);
    eprintln!("[stage 1: after build] exit={code} graph_stale={} graph_current={}", j["graph_stale"], j["sources"][0]["graph_current"]);
    assert_eq!(j["graph_present"], true);
    assert_eq!(j["graph_stale"], false, "graph is current after build: {j}");
    assert_eq!(j["sources"][0]["graph_current"], true);
    assert_eq!(code, 0, "fresh + current ⇒ exit 0");

    // 2) Rescan the cache (file changed) WITHOUT rebuilding: caches stay fresh
    //    (index matches disk), but the graph's stamped fingerprint is now stale.
    build_cache(&lib, b"CHANGED-audio-bytes-longer-now");
    let (code, j) = status_json(&home);
    eprintln!("[stage 2: cache rescanned, NOT rebuilt] exit={code} needs_scan={} graph_stale={} status={}", j["needs_scan"], j["graph_stale"], j["status"]);
    assert_eq!(j["needs_scan"], false, "cache matches disk ⇒ no scan needed: {j}");
    assert_eq!(j["graph_stale"], true, "graph no longer reflects the cache: {j}");
    assert_eq!(j["sources"][0]["graph_current"], false);
    assert_eq!(j["status"], "needs_build");
    assert_eq!(code, 1, "stale graph is action-worthy even with fresh caches");

    // 3) Rebuild → current again.
    run_ok(&home, &["build"]);
    let (code, j) = status_json(&home);
    eprintln!("[stage 3: after rebuild] exit={code} graph_stale={} graph_current={}", j["graph_stale"], j["sources"][0]["graph_current"]);
    assert_eq!(j["graph_stale"], false, "rebuild refreshes the graph: {j}");
    assert_eq!(j["sources"][0]["graph_current"], true);
    assert_eq!(code, 0);

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&lib);
}

// ──────────────────── Source.scan_fingerprint stamping ───────────────────────

fn source_fingerprint(graph: &DirGraph, id: &str) -> Option<String> {
    let ni = graph.lookup_by_id_readonly("Source", &Value::String(id.to_string()))?;
    let node = graph.get_node(ni)?;
    match resolve_node_property(node, "scan_fingerprint", graph) {
        Value::String(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn load_records() -> Vec<AnalysisRecord> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/analyses");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| AnalysisRecord::from_json(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect()
}

#[test]
fn scan_fingerprint_stamped_when_present_absent_for_fixtures() {
    let records = load_records();

    // A source WITH a fingerprint → the Source node carries scan_fingerprint.
    let with_fp = [SourceInput {
        root: "src-a".to_string(),
        records: &records,
        scan_fingerprint: Some("deadbeef-fingerprint".to_string()),
    }];
    let lib = LibraryInfo { root: "src-a".to_string(), n_tracks: records.len() };
    let g = graph::build_graph_from_sources(&with_fp, None, &lib).unwrap();
    assert_eq!(
        source_fingerprint(&g, "src-a").as_deref(),
        Some("deadbeef-fingerprint"),
        "a source's fingerprint is stamped on its Source node"
    );

    // A fixture-style build (no fingerprint) → the property is ABSENT, so the
    // golden digest is byte-unchanged.
    let g2 = graph::build_graph(&records, &LibraryInfo { root: "fixtures".to_string(), n_tracks: records.len() }).unwrap();
    assert!(
        source_fingerprint(&g2, "fixtures").is_none(),
        "no fingerprint ⇒ no scan_fingerprint property (golden stays unchanged)"
    );
}
