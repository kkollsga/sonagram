//! P17 multi-source build integration test.
//!
//! Two sources, each with fixture-derived records and **one shared content hash**
//! (the same recording present in both libraries). Asserts the P17 contract:
//! the shared recording collapses to a single `Track` (first source wins its
//! path), every `Track` carries a `FROM_SOURCE` edge to its winning `Source`
//! node, and playlist export resolves absolute paths off each `Track.source_root`
//! **without a `library_root` argument**.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use kglite::api::cypher::resolve_node_property;
use kglite::api::{DirGraph, Value};
use sonagram::graph::{self, LibraryInfo, SourceInput};
use sonagram::playlist;
use sonagram::record::AnalysisRecord;

fn load_records() -> Vec<AnalysisRecord> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/analyses");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read fixtures dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut records: Vec<AnalysisRecord> = paths
        .iter()
        .map(|p| AnalysisRecord::from_json(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect();
    records.sort_by(|a, b| a.source.content_hash.cmp(&b.source.content_hash));
    records
}

fn unique_temp(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("sonagram-p17-{tag}-{}-{stamp}", std::process::id()))
}

fn node_count(graph: &DirGraph, node_type: &str) -> usize {
    graph
        .type_indices
        .get(node_type)
        .map(|r| r.len())
        .unwrap_or(0)
}

fn str_prop(graph: &DirGraph, node_type: &str, id: &str, prop: &str) -> Option<String> {
    let ni = graph.lookup_by_id_readonly(node_type, &Value::String(id.to_string()))?;
    let node = graph.node_view(ni)?;
    match resolve_node_property(node, prop, graph) {
        Value::String(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// All FROM_SOURCE edges as `(track_hash, source_root)`.
fn from_source_edges(graph: &DirGraph) -> Vec<(String, String)> {
    let sg = graph.graph.as_stable_digraph();
    let mut out = Vec::new();
    for e in sg.edge_indices() {
        let edge = sg.edge_weight(e).unwrap();
        if edge.connection_type_str(&graph.interner) != "FROM_SOURCE" {
            continue;
        }
        let (si, ti) = sg.edge_endpoints(e).unwrap();
        let src = match graph.node_view(si).unwrap().id().into_owned() {
            Value::String(s) => s,
            other => panic!("track id not a string: {other:?}"),
        };
        let tgt = match graph.node_view(ti).unwrap().id().into_owned() {
            Value::String(s) => s,
            other => panic!("source id not a string: {other:?}"),
        };
        out.push((src, tgt));
    }
    out
}

#[test]
fn dedup_from_source_and_source_root_resolution() {
    let all = load_records();
    assert!(all.len() >= 6, "need enough fixtures");

    // Two source roots. The builder iterates sources SORTED by root, first wins a
    // shared content hash — so the winner is the lexicographically smaller root.
    let base = unique_temp("build");
    let dir_a = base.join("a-source");
    let dir_b = base.join("b-source");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    let root_a = dir_a.to_string_lossy().into_owned();
    let root_b = dir_b.to_string_lossy().into_owned();

    // Split the fixtures across the two sources.
    let mid = all.len() / 2;
    let records_a: Vec<AnalysisRecord> = all[..mid].to_vec();
    let mut records_b: Vec<AnalysisRecord> = all[mid..].to_vec();

    // The SHARED recording: clone source A's first record into source B under a
    // DIFFERENT path. Same content hash → one Track; first source keeps its path.
    let shared_hash = records_a[0].source.content_hash.clone();
    let path_a = records_a[0].source.path.clone();
    let mut dup = records_a[0].clone();
    dup.source.path = "dup-copy.mp3".to_string();
    records_b.push(dup);

    // Expected winner of the shared hash + its winning path.
    let (winner_root, winner_path) = if root_a < root_b {
        (root_a.clone(), path_a.clone())
    } else {
        (root_b.clone(), "dup-copy.mp3".to_string())
    };

    let sources = [
        SourceInput {
            root: root_a.clone(),
            records: &records_a,
            scan_fingerprint: None,
        },
        SourceInput {
            root: root_b.clone(),
            records: &records_b,
            scan_fingerprint: None,
        },
    ];
    let library = LibraryInfo {
        root: "multi-source".to_string(),
        n_tracks: 0,
    };
    let graph = graph::build_graph_from_sources(&sources, None, &library).unwrap();

    // Unique tracks = distinct content hashes across both sources.
    let unique: BTreeSet<&str> = all.iter().map(|r| r.source.content_hash.as_str()).collect();
    assert_eq!(
        node_count(&graph, "Track"),
        unique.len(),
        "one Track per hash"
    );
    assert_eq!(
        node_count(&graph, "Source"),
        2,
        "one Source node per source"
    );

    // Library node is labelled multi-source with an n_sources property.
    assert_eq!(
        str_prop(&graph, "Library", "multi-source", "path").as_deref(),
        Some("multi-source")
    );

    // Every Track has exactly one FROM_SOURCE edge; the shared one points at the
    // winning source, and its Source n_tracks sum to the unique total.
    let edges = from_source_edges(&graph);
    assert_eq!(edges.len(), unique.len(), "one FROM_SOURCE per Track");
    let shared_edge = edges
        .iter()
        .find(|(h, _)| *h == shared_hash)
        .expect("shared track has a FROM_SOURCE edge");
    assert_eq!(
        shared_edge.1, winner_root,
        "shared track wins the first source"
    );

    // The shared Track keeps the WINNING source's path + source_root.
    assert_eq!(
        str_prop(&graph, "Track", &shared_hash, "source_root").as_deref(),
        Some(winner_root.as_str())
    );
    assert_eq!(
        str_prop(&graph, "Track", &shared_hash, "path").as_deref(),
        Some(winner_path.as_str())
    );

    // ── Playlist export resolves absolute paths with NO library_root ─────────
    // Materialize each Track's file at source_root/path, then export by id with
    // an EMPTY library_root: source_root must carry the resolution.
    let ids: Vec<String> = unique.iter().map(|h| h.to_string()).collect();
    for id in &ids {
        let root = str_prop(&graph, "Track", id, "source_root").unwrap();
        let rel = str_prop(&graph, "Track", id, "path").unwrap();
        let path = Path::new(&root).join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, b"x").unwrap();
    }

    let entries = playlist::entries_from_graph(&graph, Path::new(""), &ids).unwrap();
    assert_eq!(entries.len(), ids.len());
    for e in &entries {
        assert!(
            e.abs_path.is_absolute(),
            "abs path off source_root: {:?}",
            e.abs_path
        );
        assert!(
            e.abs_path.exists(),
            "resolved file exists: {:?}",
            e.abs_path
        );
        assert!(
            e.abs_path.starts_with(&dir_a) || e.abs_path.starts_with(&dir_b),
            "path under a configured source: {:?}",
            e.abs_path
        );
    }
    // The shared track resolves under the winning source.
    let shared_entry = entries
        .iter()
        .find(|e| e.content_hash == shared_hash)
        .unwrap();
    assert!(shared_entry.abs_path.starts_with(&winner_root));

    let _ = std::fs::remove_dir_all(&base);
}
