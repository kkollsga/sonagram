//! P4 graph-mapping gate over the 15 frozen fixtures.
//!
//! Builds the music graph from `tests/fixtures/analyses/*.json` and asserts the
//! full shape the schema promises: per-type node counts, zero-skip edges,
//! a spot-checked `Track` property row, a 15-vector / 48-dim similarity store,
//! and a `.kgl` save → load round-trip that preserves properties, embeddings,
//! and counts.
//!
//! Expected node/edge cardinalities were computed independently from the raw
//! fixture JSON (not from the builder), then hard-coded here.

use std::path::PathBuf;

use kglite::api::cypher::resolve_node_property;
use kglite::api::{DirGraph, Value};
use sonagram::graph::{self, LibraryInfo};
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

fn library() -> LibraryInfo {
    LibraryInfo {
        root: "fixtures".to_string(),
        n_tracks: 15,
    }
}

/// Bruno Mars — "Marry You" fixture, used for the property spot-check.
const BRUNO_HASH: &str = "a204cc4055d23bf27a3659c2b28da5aad9c0769e6a6c587fa749ce5ad18b4419";

fn node_count(graph: &DirGraph, node_type: &str) -> usize {
    graph.type_indices.get(node_type).map(|r| r.len()).unwrap_or(0)
}

fn f64_prop(graph: &DirGraph, node_type: &str, hash: &str, prop: &str) -> f64 {
    let ni = graph
        .lookup_by_id_readonly(node_type, &Value::String(hash.to_string()))
        .unwrap_or_else(|| panic!("no {node_type} node with id {hash}"));
    let node = graph.get_node(ni).unwrap();
    match resolve_node_property(node, prop, graph) {
        Value::Float64(v) => v,
        other => panic!("{prop} is not Float64: {other:?}"),
    }
}

fn str_prop(graph: &DirGraph, node_type: &str, hash: &str, prop: &str) -> String {
    let ni = graph
        .lookup_by_id_readonly(node_type, &Value::String(hash.to_string()))
        .unwrap_or_else(|| panic!("no {node_type} node with id {hash}"));
    let node = graph.get_node(ni).unwrap();
    match resolve_node_property(node, prop, graph) {
        Value::String(s) => s,
        other => panic!("{prop} is not String: {other:?}"),
    }
}

// Expected cardinalities, computed independently from the fixture JSON.
// `Style` (P10c): **2** communities over the 15 fixtures at the *adaptive*
// threshold this build chooses from the fixtures' own score distribution
// (measured chosen threshold ≈0.747, cap = style_cap(15) = 5). P10b's fixed 0.85
// produced zero fixture styles; P10c's per-build adaptive selection restores them
// (see the `graph_derive::style_threshold_tuning` diagnostic + GRAPH-GATE.md).
const EXPECT_NODES: &[(&str, usize)] = &[
    ("Library", 1),
    ("Track", 15),
    ("Artist", 15),
    ("Album", 15),
    ("Genre", 10),
    ("Key", 24),
    ("TempoBand", 7),
    ("EnergyLevel", 10),
    ("Decade", 4),
    ("Style", 2),
];

// BY_ARTIST carries both Track→Artist (15) and Album→Artist (15) = 30.
// P6/P10c derived edges: SIMILAR_TO = 15 tracks × min(10, 14) = 150 (dense, all
// 15 carry embeddings); CAMELOT_ADJACENT = 24 keys × 3 = 72; IN_STYLE = 4 + 4 = 8
// (the two adaptive-threshold communities' members).
const EXPECT_EDGES: &[(&str, usize)] = &[
    ("BY_ARTIST", 30),
    ("ON_ALBUM", 15),
    ("IN_GENRE", 14),
    ("IN_KEY", 15),
    ("IN_TEMPO_BAND", 15),
    ("AT_ENERGY", 15),
    ("FROM_DECADE", 15),
    ("SIMILAR_TO", 150),
    ("CAMELOT_ADJACENT", 72),
    ("IN_STYLE", 8),
];

#[test]
fn node_counts_match_expected() {
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    for (ty, n) in EXPECT_NODES {
        assert_eq!(node_count(&graph, ty), *n, "node count for {ty}");
    }
}

#[test]
fn edge_counts_match_and_no_endpoint_skipped() {
    // build_graph already errors on any skipped endpoint; the counts below
    // prove the same thing positively (a missing endpoint would shorten a
    // count).
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    let counts = graph.get_edge_type_counts();
    for (ty, n) in EXPECT_EDGES {
        assert_eq!(counts.get(*ty).copied().unwrap_or(0), *n, "edge count for {ty}");
    }
    let total: usize = counts.values().sum();
    let expected_total: usize = EXPECT_EDGES.iter().map(|(_, n)| n).sum();
    assert_eq!(total, expected_total, "no unexpected extra edge types");
}

#[test]
fn track_property_row_spot_check() {
    let graph = graph::build_graph(&load_records(), &library()).unwrap();

    let bpm = f64_prop(&graph, "Track", BRUNO_HASH, "bpm");
    assert!((bpm - 144.566).abs() < 0.1, "bpm ≈ 144.57, got {bpm}");

    // Mood heuristics present and in [0, 1].
    for m in ["mood_happy", "mood_aggressive", "mood_relaxed", "mood_sad"] {
        let v = f64_prop(&graph, "Track", BRUNO_HASH, m);
        assert!((0.0..=1.0).contains(&v), "{m} out of range: {v}");
    }
    assert!(f64_prop(&graph, "Track", BRUNO_HASH, "mood_happy") > 0.7);
    assert!(f64_prop(&graph, "Track", BRUNO_HASH, "instrumentalness") > 0.0);

    assert_eq!(str_prop(&graph, "Track", BRUNO_HASH, "title"), "Marry You");
    assert_eq!(str_prop(&graph, "Track", BRUNO_HASH, "artist_name"), "Bruno Mars");
    assert_eq!(str_prop(&graph, "Track", BRUNO_HASH, "key"), "F major");
    assert_eq!(str_prop(&graph, "Track", BRUNO_HASH, "camelot"), "7B");
}

#[test]
fn embedding_store_is_present_and_shaped() {
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    let store = graph
        .embeddings
        .get(&("Track".to_string(), "similarity".to_string()))
        .expect("similarity embedding store present");
    assert_eq!(store.dimension, 48, "48-dim vectors");
    assert_eq!(store.slot_to_node.len(), 15, "one vector per track");
    assert_eq!(store.model_id.as_deref(), Some("sonara-similarity-v1"));
    assert_eq!(store.metric.as_deref(), Some("euclidean"));
}

/// Not a gate — writes a human-readable graph summary to `dev-docs/temp/`.
/// Run with: `cargo test -p sonagram --test graph_build -- --ignored dump`.
#[test]
#[ignore = "diagnostic dump, not an assertion"]
fn dump_graph_stats() {
    use std::fmt::Write as _;
    let graph = graph::build_graph(&load_records(), &library()).unwrap();

    let mut out = String::new();
    writeln!(out, "sonagram P4 graph dump — 15 fixtures\n").unwrap();

    writeln!(out, "NODE COUNTS (by type):").unwrap();
    let mut node_total = 0usize;
    for (ty, _) in EXPECT_NODES {
        let n = node_count(&graph, ty);
        node_total += n;
        writeln!(out, "  {ty:<12} {n}").unwrap();
    }
    writeln!(out, "  {:<12} {node_total}\n", "TOTAL").unwrap();

    writeln!(out, "EDGE COUNTS (by type):").unwrap();
    let counts = graph.get_edge_type_counts();
    let mut edge_types: Vec<(&String, &usize)> = counts.iter().collect();
    edge_types.sort_by(|a, b| a.0.cmp(b.0));
    let mut edge_total = 0usize;
    for (ty, n) in &edge_types {
        edge_total += **n;
        writeln!(out, "  {ty:<14} {n}").unwrap();
    }
    writeln!(out, "  {:<14} {edge_total}\n", "TOTAL").unwrap();

    let track_props = graph
        .get_node_type_metadata("Track")
        .map(|m| m.len())
        .unwrap_or(0);
    writeln!(out, "Track declared properties: {track_props}").unwrap();

    let store = graph
        .embeddings
        .get(&("Track".to_string(), "similarity".to_string()))
        .unwrap();
    writeln!(
        out,
        "Embedding store: {} vectors, dim {}, model {:?}, metric {:?}",
        store.slot_to_node.len(),
        store.dimension,
        store.model_id,
        store.metric
    )
    .unwrap();

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dev-docs/temp");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p4-graph-dump.txt");
    std::fs::write(&path, &out).unwrap();
    eprintln!("wrote {}", path.display());
    eprint!("{out}");
}

#[test]
fn save_load_round_trip_preserves_properties_embeddings_counts() {
    let mut graph = graph::build_graph(&load_records(), &library()).unwrap();

    // Unique temp path (no extra deps).
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("sonagram-p4-{}-{stamp}.kgl", std::process::id()));

    graph::save(&mut graph, &path).unwrap();
    let loaded = kglite::api::io::load_file(path.to_str().unwrap()).expect("load .kgl");

    // Node counts survive.
    for (ty, n) in EXPECT_NODES {
        assert_eq!(node_count(&loaded, ty), *n, "post-load node count for {ty}");
    }
    // A property survives.
    assert_eq!(str_prop(&loaded, "Track", BRUNO_HASH, "title"), "Marry You");
    let bpm = f64_prop(&loaded, "Track", BRUNO_HASH, "bpm");
    assert!((bpm - 144.566).abs() < 0.1, "post-load bpm ≈ 144.57, got {bpm}");
    // Embeddings survive.
    let store = loaded
        .embeddings
        .get(&("Track".to_string(), "similarity".to_string()))
        .expect("post-load similarity store present");
    assert_eq!(store.dimension, 48);
    assert_eq!(store.slot_to_node.len(), 15);

    let _ = std::fs::remove_file(&path);
}
