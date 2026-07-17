//! P7 playlist-export integration test.
//!
//! Builds the music graph from the 15 frozen fixtures, then exercises the
//! playlist writer end-to-end: `entries_from_graph` (explicit id order),
//! `write_m3u8` → re-read, and `entries_from_cypher` (both the content-hash and
//! the Track-node result shapes). Asserts order is preserved verbatim, paths are
//! absolute + joined onto the library root, the CJK fixture's UTF-8 title
//! survives intact, and a missing id lists *all* missing ids.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sonagram::graph::{self, LibraryInfo};
use sonagram::playlist::{self, PlaylistEntry};
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

// A platform-appropriate absolute root: a bare "/music/library" is NOT
// absolute on Windows (no drive letter), which failed these tests in CI.
#[cfg(windows)]
const LIB_ROOT: &str = r"C:\music\library";
#[cfg(not(windows))]
const LIB_ROOT: &str = "/music/library";

// P17: build the fixture graph with the source root = LIB_ROOT, so each Track's
// stamped `source_root` resolves playlist paths under LIB_ROOT (the same root the
// tests pass as the fallback).
fn library() -> LibraryInfo {
    LibraryInfo {
        root: LIB_ROOT.to_string(),
        n_tracks: 15,
    }
}

/// title → content hash, for picking fixtures by their (readable) title.
fn hash_by_title(records: &[AnalysisRecord]) -> HashMap<String, String> {
    records
        .iter()
        .map(|r| {
            let title = r
                .tags
                .as_ref()
                .and_then(|t| t.title.clone())
                .unwrap_or_default();
            (title, r.source.content_hash.clone())
        })
        .collect()
}

fn unique_temp(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("sonagram-p7-{tag}-{}-{stamp}", std::process::id()))
}

#[test]
fn entries_from_graph_preserve_order_paths_and_cjk() {
    let records = load_records();
    let g = graph::build_graph(&records, &library()).unwrap();
    let by_title = hash_by_title(&records);

    // Five fixtures in a DELIBERATE, non-sorted order (not by hash, not by bpm),
    // including the CJK fixture. Playlist order must come out exactly like this.
    let titles = [
        "Marry You",
        "薔薇と雨",
        "On and on and on",
        "2pac::brenda's got a baby",
        "Jive Talkin'",
    ];
    let ids: Vec<String> = titles.iter().map(|t| by_title[*t].clone()).collect();

    let root = Path::new(LIB_ROOT);
    let entries = playlist::entries_from_graph(g.as_ref(), root, &ids).unwrap();

    assert_eq!(entries.len(), 5, "one entry per input id");
    // Order matches the input order verbatim.
    for (entry, title) in entries.iter().zip(titles.iter()) {
        assert_eq!(entry.title.as_deref(), Some(*title), "order preserved");
        // Paths are absolute and joined onto the library root.
        assert!(entry.abs_path.is_absolute(), "abs path: {:?}", entry.abs_path);
        assert!(
            entry.abs_path.starts_with(root),
            "path joined onto library root: {:?}",
            entry.abs_path
        );
    }

    // The CJK fixture: exact UTF-8 artist + title survive.
    let cjk = &entries[1];
    assert_eq!(cjk.title.as_deref(), Some("薔薇と雨"));
    assert_eq!(cjk.artist.as_deref(), Some("布袋寅泰"));

    // Write → re-read the .m3u8 and assert its structure.
    let dir = unique_temp("graph");
    let out = dir.join("set.m3u8");
    playlist::write_m3u8(&entries, &out).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(!text.starts_with('\u{feff}'), "no BOM");
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(lines[0], "#EXTM3U");
    // 5 EXTINF + 5 path lines after the header.
    let extinf: Vec<&str> = lines.iter().filter(|l| l.starts_with("#EXTINF:")).copied().collect();
    assert_eq!(extinf.len(), 5, "five EXTINF lines");
    assert_eq!(extinf[0], "#EXTINF:230,Bruno Mars - Marry You");
    // CJK title intact inside the EXTINF label (duration 212.2 → 212).
    assert_eq!(extinf[1], "#EXTINF:212,布袋寅泰 - 薔薇と雨");
    // The path line following the CJK entry is absolute + under the root.
    let cjk_line = lines.iter().position(|l| l.contains("薔薇と雨")).unwrap();
    let cjk_path = lines[cjk_line + 1];
    assert!(cjk_path.starts_with(LIB_ROOT), "cjk path under root: {cjk_path}");
    assert!(cjk_path.ends_with("08 薔薇と雨.mp3"), "cjk rel path joined: {cjk_path}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entries_from_cypher_string_column_order_matches_bpm() {
    let records = load_records();
    let g = graph::build_graph(&records, &library()).unwrap();

    // Expected: tracks with bpm > 130, ordered ascending by bpm — computed
    // independently from the raw records.
    let mut expected: Vec<(&str, f32)> = records
        .iter()
        .filter(|r| r.analysis.bpm > 130.0)
        .map(|r| {
            let title = r.tags.as_ref().and_then(|t| t.title.as_deref()).unwrap_or("");
            (title, r.analysis.bpm)
        })
        .collect();
    expected.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let expected_titles: Vec<&str> = expected.iter().map(|(t, _)| *t).collect();
    assert_eq!(
        expected_titles,
        vec![
            "Toxic - Armand Van Helden Remix Edit",
            "2 unlimited::let the beat control your body",
            "Marry You",
        ],
        "sanity: fixture set for bpm > 130"
    );

    let query = "MATCH (t:Track) WHERE t.bpm > 130 RETURN t.content_hash ORDER BY t.bpm";
    let entries = playlist::entries_from_cypher(g.as_ref(), Path::new(LIB_ROOT), query).unwrap();

    let got: Vec<&str> = entries.iter().map(|e| e.title.as_deref().unwrap_or("")).collect();
    assert_eq!(got, expected_titles, "cypher row order preserved");
    for e in &entries {
        assert!(e.abs_path.is_absolute());
        assert!(e.abs_path.starts_with(LIB_ROOT));
    }
}

#[test]
fn entries_from_cypher_node_column_shape() {
    let records = load_records();
    let g = graph::build_graph(&records, &library()).unwrap();

    // Returning the Track NODE (not its hash) resolves to the same set/order.
    let query = "MATCH (t:Track) WHERE t.bpm > 130 RETURN t ORDER BY t.bpm";
    let entries = playlist::entries_from_cypher(g.as_ref(), Path::new(LIB_ROOT), query).unwrap();
    let got: Vec<&str> = entries.iter().map(|e| e.title.as_deref().unwrap_or("")).collect();
    assert_eq!(
        got,
        vec![
            "Toxic - Armand Van Helden Remix Edit",
            "2 unlimited::let the beat control your body",
            "Marry You",
        ],
        "node-column result resolves like the hash-column one"
    );
}

#[test]
fn missing_ids_error_lists_all() {
    let records = load_records();
    let g = graph::build_graph(&records, &library()).unwrap();
    let by_title = hash_by_title(&records);

    let real = by_title["Marry You"].clone();
    let ids = vec![real, "deadbeefmissing1".to_string(), "c0ffeemissing2".to_string()];
    let err = playlist::entries_from_graph(g.as_ref(), Path::new(LIB_ROOT), &ids).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("deadbeefmissing1"), "lists first missing id: {msg}");
    assert!(msg.contains("c0ffeemissing2"), "lists second missing id: {msg}");
}

/// A Track entry read from the graph carries the joined absolute path.
#[test]
fn abs_path_is_root_join_relative() {
    let records = load_records();
    let g = graph::build_graph(&records, &library()).unwrap();
    let by_title = hash_by_title(&records);
    let id = by_title["Marry You"].clone();

    let root = Path::new(LIB_ROOT);
    let entries = playlist::entries_from_graph(g.as_ref(), root, std::slice::from_ref(&id)).unwrap();
    let expected: PlaylistEntry = PlaylistEntry {
        content_hash: id.clone(),
        abs_path: root.join("04 Marry You.mp3"),
        duration_sec: entries[0].duration_sec,
        artist: Some("Bruno Mars".to_string()),
        title: Some("Marry You".to_string()),
    };
    assert_eq!(entries[0], expected);
}
