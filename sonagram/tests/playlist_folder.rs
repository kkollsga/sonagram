//! P13 portable-playlist-folder integration test.
//!
//! Builds the music graph from the 15 frozen fixtures over a FAKE library — tiny
//! dummy files created at each fixture's relative path (copy semantics don't care
//! about audio content) — then exercises `export_folder` end-to-end: copied file
//! names (position-prefixed, sanitized, CJK intact), the relative-path `.m3u8`
//! lines matching the copied files in order, the copied count and total bytes.
//!
//! Copies only: the test asserts the source files are left untouched.

use std::path::PathBuf;

use sonagram::graph::{self, LibraryInfo};
use sonagram::playlist;
use sonagram::record::AnalysisRecord;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/analyses")
}

fn load_records() -> Vec<AnalysisRecord> {
    let dir = fixtures_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read fixtures dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).unwrap();
            AnalysisRecord::from_json(&text).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
        })
        .collect()
}

/// P17: build the fixture graph with the source root = the fake library dir, so
/// each Track's stamped `source_root` resolves to the on-disk files this test
/// materializes (playlist export no longer needs a library_root argument).
fn build_with_root(records: &[AnalysisRecord], lib: &std::path::Path) -> std::sync::Arc<kglite::api::DirGraph> {
    let library = LibraryInfo {
        root: lib.to_string_lossy().into_owned(),
        n_tracks: records.len(),
    };
    graph::build_graph(records, &library).unwrap()
}

fn unique_temp(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("sonagram-p13-{tag}-{}-{stamp}", std::process::id()))
}

/// Materialize a fake library on disk: one tiny dummy file per record at its
/// `source.path`, content = the (unique) content hash bytes so sizes differ.
/// Returns the library root.
fn fake_library(records: &[AnalysisRecord], root: &std::path::Path) {
    for r in records {
        let path = root.join(&r.source.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, r.source.content_hash.as_bytes()).unwrap();
    }
}

#[test]
fn export_folder_copies_all_fixtures_with_relative_m3u8() {
    let records = load_records();
    let base = unique_temp("all");
    let lib = base.join("library");
    let dest = base.join("Road Trip");
    fake_library(&records, &lib);
    let g = build_with_root(&records, &lib);

    // Resolve ALL 15 tracks in fixture (sorted) order via their content hashes.
    let ids: Vec<String> = records.iter().map(|r| r.source.content_hash.clone()).collect();
    let entries = playlist::entries_from_graph(g.as_ref(), &lib, &ids).unwrap();
    assert_eq!(entries.len(), 15);

    let report = playlist::export_folder(&entries, &dest, "Road Trip").unwrap();

    // Count + bytes: one copy per track, bytes = sum of the dummy file sizes.
    assert_eq!(report.copied, 15);
    let expected_bytes: u64 = records
        .iter()
        .map(|r| r.source.content_hash.len() as u64)
        .sum();
    assert_eq!(report.bytes, expected_bytes);
    assert_eq!(report.playlist_path, dest.join("Road Trip.m3u8"));

    // Read the .m3u8: header + 15 EXTINF/path pairs, path lines are bare relative
    // filenames (no directory separators, no library root leaking in).
    let text = std::fs::read_to_string(&report.playlist_path).unwrap();
    assert!(!text.starts_with('\u{feff}'), "no BOM");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "#EXTM3U");

    let path_lines: Vec<&str> = lines[1..]
        .iter()
        .filter(|l| !l.starts_with('#'))
        .copied()
        .collect();
    assert_eq!(path_lines.len(), 15, "one path line per track");
    for (i, name) in path_lines.iter().enumerate() {
        // Relative: no separators, and the actual copied file exists.
        assert!(!name.contains('/') && !name.contains('\\'), "relative name: {name}");
        assert!(name.starts_with(&format!("{:02} - ", i + 1)), "position prefix: {name}");
        assert!(dest.join(name).exists(), "copied file present: {name}");
    }

    // The CJK fixture survives intact in both the copied filename and the m3u8.
    let cjk = path_lines
        .iter()
        .find(|n| n.contains("薔薇と雨"))
        .expect("CJK track present with intact title");
    assert!(cjk.contains("布袋寅泰"), "CJK artist intact: {cjk}");
    assert!(dest.join(cjk).exists());

    // The folder holds exactly the 15 copies + the one .m3u8.
    let n_entries = std::fs::read_dir(&dest).unwrap().count();
    assert_eq!(n_entries, 16, "15 audio copies + 1 .m3u8");

    // Copies only: every source file is untouched (still present, same bytes).
    for r in &records {
        let src = lib.join(&r.source.path);
        assert!(src.exists(), "source not moved: {}", src.display());
        assert_eq!(
            std::fs::read(&src).unwrap(),
            r.source.content_hash.as_bytes(),
            "source not modified: {}",
            src.display()
        );
    }

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn export_folder_preserves_selection_order() {
    let records = load_records();
    let base = unique_temp("order");
    let lib = base.join("library");
    let dest = base.join("out");
    fake_library(&records, &lib);
    let g = build_with_root(&records, &lib);

    // Reverse fixture order — the NN prefixes must follow THIS order, not sorted.
    let ids: Vec<String> = records
        .iter()
        .rev()
        .map(|r| r.source.content_hash.clone())
        .collect();
    let entries = playlist::entries_from_graph(g.as_ref(), &lib, &ids).unwrap();

    let report = playlist::export_folder(&entries, &dest, "set").unwrap();
    assert_eq!(report.copied, records.len());

    let text = std::fs::read_to_string(&report.playlist_path).unwrap();
    let path_lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect();
    // First path line is position 01 and corresponds to the LAST fixture record:
    // verify by byte identity of the copied file vs that record's source content.
    assert!(path_lines[0].starts_with("01 - "), "{}", path_lines[0]);
    let last_hash = records.last().unwrap().source.content_hash.as_bytes();
    assert_eq!(
        std::fs::read(dest.join(path_lines[0])).unwrap(),
        last_hash,
        "position 01 copies the last-selected record"
    );

    let _ = std::fs::remove_dir_all(&base);
}
