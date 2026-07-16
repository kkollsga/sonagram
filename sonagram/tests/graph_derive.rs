//! P6 derived-structure gate over the 15 frozen fixtures — the assertions for
//! `SIMILAR_TO`, `CAMELOT_ADJACENT`, and `Style` that the golden digest alone
//! can't make legible.
//!
//! These run against the same `build_graph` output the golden gate digests, so
//! they document *why* the P6 counts are what they are (and would fail loudly,
//! with a reason, if the derivation drifted). Expected cardinalities were
//! computed independently: `SIMILAR_TO` = 15 tracks × min(10, 14) = 150;
//! `CAMELOT_ADJACENT` = 24 keys × 3 = 72; `Style` = 2 communities at the tuned
//! threshold (measured — the 15 diverse fixtures single-linkage into 2 tight
//! components once merely-adjacent chains are cut).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use kglite::api::cypher::resolve_node_property;
use kglite::api::{DirGraph, Value};
use sonagram::graph::{self, LibraryInfo};
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

fn library() -> LibraryInfo {
    LibraryInfo {
        root: "fixtures".to_string(),
        n_tracks: 15,
    }
}

/// All edges of `edge_type` as `(source_id, target_id, props)`, read through the
/// stable-digraph view the golden gate uses.
fn edges_of(
    graph: &DirGraph,
    edge_type: &str,
) -> Vec<(String, String, HashMap<String, Value>)> {
    let sg = graph.graph.as_stable_digraph();
    let mut out = Vec::new();
    for e in sg.edge_indices() {
        let edge = sg.edge_weight(e).expect("edge weight");
        if edge.connection_type_str(&graph.interner) != edge_type {
            continue;
        }
        let (si, ti) = sg.edge_endpoints(e).expect("endpoints");
        let src = id_string(sg.node_weight(si).expect("src").id());
        let tgt = id_string(sg.node_weight(ti).expect("tgt").id());
        let props: HashMap<String, Value> = edge.properties_cloned(&graph.interner);
        out.push((src, tgt, props));
    }
    out
}

fn id_string(v: Cow<Value>) -> String {
    match v.into_owned() {
        Value::String(s) => s,
        other => panic!("expected String id, got {other:?}"),
    }
}

fn node_count(graph: &DirGraph, node_type: &str) -> usize {
    graph.type_indices.get(node_type).map(|r| r.len()).unwrap_or(0)
}

// ──────────────────────────────── SIMILAR_TO ────────────────────────────────

#[test]
fn similar_to_is_dense_directed_and_self_loop_free() {
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    let edges = edges_of(&graph, "SIMILAR_TO");

    // 15 tracks, each with 10 neighbours (all 15 carry an embedding).
    assert_eq!(edges.len(), 150, "SIMILAR_TO = 15 × min(10, 14)");

    // No self-loops; every score in (0, 1]; exactly 10 out-edges per source.
    let mut out_deg: BTreeMap<String, usize> = BTreeMap::new();
    for (src, tgt, props) in &edges {
        assert_ne!(src, tgt, "SIMILAR_TO must not self-loop");
        let score = match props.get("score") {
            Some(Value::Float64(v)) => *v,
            other => panic!("SIMILAR_TO score not Float64: {other:?}"),
        };
        assert!(
            score > 0.0 && score <= 1.0,
            "SIMILAR_TO score out of (0, 1]: {score}"
        );
        *out_deg.entry(src.clone()).or_insert(0) += 1;
    }
    assert_eq!(out_deg.len(), 15, "every track is a SIMILAR_TO source");
    for (src, deg) in &out_deg {
        assert_eq!(*deg, 10, "track {src} must have exactly 10 SIMILAR_TO edges");
    }
}

#[test]
fn similar_to_neighbours_are_distinct_per_source() {
    // kNN is directed and each neighbour appears once per source (no dup edges).
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    let mut seen: std::collections::BTreeSet<(String, String)> = Default::default();
    for (src, tgt, _) in edges_of(&graph, "SIMILAR_TO") {
        assert!(seen.insert((src.clone(), tgt.clone())), "dup SIMILAR_TO {src}->{tgt}");
    }
}

// ─────────────────────────────── CAMELOT wheel ──────────────────────────────

#[test]
fn camelot_adjacent_is_72_edges() {
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    let edges = edges_of(&graph, "CAMELOT_ADJACENT");
    assert_eq!(edges.len(), 72, "24 keys × 3 neighbours");

    // Exactly 24 of each transition kind.
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for (_, _, props) in &edges {
        let t = match props.get("transition") {
            Some(Value::String(s)) => s.clone(),
            other => panic!("transition not String: {other:?}"),
        };
        *kinds.entry(t).or_insert(0) += 1;
    }
    assert_eq!(kinds.get("energy_up").copied(), Some(24));
    assert_eq!(kinds.get("energy_down").copied(), Some(24));
    assert_eq!(kinds.get("mode_switch").copied(), Some(24));
}

#[test]
fn camelot_8a_neighbours_are_correct() {
    // "A minor" is Camelot 8A. Its three wheel neighbours:
    //   7A (D minor) = energy_down, 9A (E minor) = energy_up, 8B (C major) = mode_switch.
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    let neighbours: BTreeMap<String, String> = edges_of(&graph, "CAMELOT_ADJACENT")
        .into_iter()
        .filter(|(src, _, _)| src == "A minor")
        .map(|(_, tgt, props)| {
            let t = match props.get("transition") {
                Some(Value::String(s)) => s.clone(),
                other => panic!("transition not String: {other:?}"),
            };
            (tgt, t)
        })
        .collect();

    assert_eq!(neighbours.len(), 3, "A minor has 3 wheel neighbours");
    assert_eq!(neighbours.get("D minor").map(String::as_str), Some("energy_down"));
    assert_eq!(neighbours.get("E minor").map(String::as_str), Some("energy_up"));
    assert_eq!(neighbours.get("C major").map(String::as_str), Some("mode_switch"));
}

// ─────────────────────────────── Style nodes ────────────────────────────────

fn style_ids(graph: &DirGraph) -> Vec<String> {
    let mut ids: Vec<String> = graph
        .type_indices
        .get("Style")
        .map(|r| r.to_vec())
        .unwrap_or_default()
        .into_iter()
        .map(|ni| id_string(graph.get_node(ni).unwrap().id()))
        .collect();
    ids.sort();
    ids
}

#[test]
fn styles_are_two_with_stable_padded_ids() {
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    assert_eq!(node_count(&graph, "Style"), 2, "2 communities at the tuned threshold");
    assert_eq!(style_ids(&graph), vec!["style-000", "style-001"]);

    // IN_STYLE membership = 1.0 (v1 hard assignment); 7 members (4 + 3).
    let in_style = edges_of(&graph, "IN_STYLE");
    assert_eq!(in_style.len(), 7, "4 + 3 members carry IN_STYLE");
    for (_, _, props) in &in_style {
        assert_eq!(props.get("membership"), Some(&Value::Float64(1.0)));
    }
}

#[test]
fn style_ids_stable_across_input_reordering() {
    let recs = load_records();
    let a = graph::build_graph(&recs, &library()).unwrap();

    let mut reversed = recs.clone();
    reversed.reverse();
    let b = graph::build_graph(&reversed, &library()).unwrap();

    assert_eq!(style_ids(&a), style_ids(&b), "Style ids stable across reorder");
}

#[test]
fn style_profiles_are_shaped_and_deterministic() {
    let recs = load_records();
    let a = graph::build_graph(&recs, &library()).unwrap();
    let b = graph::build_graph(&recs, &library()).unwrap();

    for id in ["style-000", "style-001"] {
        let ni_a = a
            .lookup_by_id_readonly("Style", &Value::String(id.to_string()))
            .unwrap_or_else(|| panic!("style {id} present"));
        let node_a = a.get_node(ni_a).unwrap();

        // name matches the "<band>-<acoustic|electric>-<genre>" template shape.
        let name = match resolve_node_property(node_a, "name", &a) {
            Value::String(s) => s,
            other => panic!("style name not String: {other:?}"),
        };
        assert_eq!(name.split('-').count(), 3, "style name has 3 template segments: {name}");

        // exemplar_titles: a non-empty list, at most 5, deterministic across builds.
        let ex_a = resolve_node_property(node_a, "exemplar_titles", &a);
        let ex_len = match &ex_a {
            Value::List(v) => v.len(),
            other => panic!("exemplar_titles not List: {other:?}"),
        };
        assert!((1..=5).contains(&ex_len), "exemplar_titles len in [1,5]: {ex_len}");

        let ni_b = b
            .lookup_by_id_readonly("Style", &Value::String(id.to_string()))
            .unwrap();
        let ex_b = resolve_node_property(b.get_node(ni_b).unwrap(), "exemplar_titles", &b);
        assert_eq!(ex_a, ex_b, "exemplar_titles deterministic for {id}");

        // n_tracks matches the IN_STYLE member count for this style.
        let n = match resolve_node_property(node_a, "n_tracks", &a) {
            Value::Int64(v) => v,
            other => panic!("n_tracks not Int64: {other:?}"),
        };
        let members = edges_of(&a, "IN_STYLE")
            .into_iter()
            .filter(|(_, tgt, _)| tgt == id)
            .count() as i64;
        assert_eq!(n, members, "n_tracks == IN_STYLE members for {id}");
    }
}
