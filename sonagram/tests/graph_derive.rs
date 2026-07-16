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
fn styles_absent_on_diverse_fixtures_at_calibrated_threshold() {
    // P10b: the Style community threshold (0.85) is calibrated to the real
    // library's compressed similarity scores. The 15 committed fixtures are
    // maximally diverse — their top mutual-kNN pair-score is only ~0.78 — so no
    // component clears 0.85 and NO Style is created. This is the honest,
    // documented consequence of the subset calibration; end-to-end Style shape
    // coverage lives in the `derive` unit tests + the `style_table_dump` /
    // `style_threshold_tuning` maintainer diagnostics. Regression guard: if this
    // count ever becomes non-zero on the fixtures the threshold moved.
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    assert_eq!(node_count(&graph, "Style"), 0, "no style survives 0.85 over diverse fixtures");
    assert!(style_ids(&graph).is_empty(), "no Style ids");
    assert_eq!(edges_of(&graph, "IN_STYLE").len(), 0, "no IN_STYLE edges");
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

// ────────────────────── P10b: style community tuning ────────────────────────

/// Minimal union-find mirroring the production one in `derive.rs`, so the
/// diagnostic sweeps thresholds with the exact same component semantics.
struct Uf {
    parent: Vec<usize>,
    size: Vec<usize>,
}
impl Uf {
    fn new(n: usize) -> Self {
        Uf { parent: (0..n).collect(), size: vec![1; n] }
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        let (big, small) = if self.size[ra] >= self.size[rb] { (ra, rb) } else { (rb, ra) };
        self.parent[small] = big;
        self.size[big] += self.size[small];
    }
}

/// One sweep result: (n_styles, largest_component, coverage_tracks).
fn sweep(
    n: usize,
    index_of: &BTreeMap<String, usize>,
    edges: &[(usize, usize, f64)],
    threshold: f64,
    min_size: usize,
) -> (usize, usize, usize) {
    let _ = index_of;
    let mut uf = Uf::new(n);
    for &(a, b, s) in edges {
        if s >= threshold {
            uf.union(a, b);
        }
    }
    let mut groups: BTreeMap<usize, usize> = BTreeMap::new();
    for i in 0..n {
        *groups.entry(uf.find(i)).or_default() += 1;
    }
    let comps: Vec<usize> = groups.values().copied().filter(|&c| c >= min_size).collect();
    let n_styles = comps.len();
    let largest = comps.iter().copied().max().unwrap_or(0);
    let coverage: usize = comps.iter().sum();
    (n_styles, largest, coverage)
}

/// PERMANENT #[ignore] diagnostic (P10b). Loads the maintainer-only 456-track
/// subset (or `$SONAGRAM_SUBSET_DIR`), builds the real graph, reads the directed
/// top-k `SIMILAR_TO` fan-out back, then sweeps the Style community threshold for
/// both the **mutual-kNN** edge set (production) and the old **single-linkage**
/// set (any directed edge). Prints a threshold → (n_styles, largest%, coverage%)
/// table so the tuned `STYLE_SCORE_THRESHOLD` / `STYLE_MIN_SIZE` are auditable.
///
/// Skips cleanly when the subset dir is absent (it is never committed). Run:
///   cargo test -p sonagram --test graph_derive -- --ignored --nocapture style_threshold_tuning
#[test]
#[ignore = "maintainer-only 456-track subset diagnostic, not a gate"]
fn style_threshold_tuning() {
    let dir = std::env::var("SONAGRAM_SUBSET_DIR").unwrap_or_else(|_| {
        "/Volumes/EksternalHome/KristianEX/tmp-sonagram-p10/subset500/.sonagram/analysis".to_string()
    });
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        eprintln!("[skip] subset dir {} absent", dir.display());
        return;
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut records: Vec<AnalysisRecord> = paths
        .iter()
        .map(|p| AnalysisRecord::from_json(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect();
    records.sort_by(|a, b| a.source.content_hash.cmp(&b.source.content_hash));
    let n = records.len();
    let lib = LibraryInfo { root: "subset".to_string(), n_tracks: n };
    let graph = graph::build_graph(&records, &lib).unwrap();
    let n_tracks = node_count(&graph, "Track");

    // Dense index over content hashes (union-find domain).
    let mut hashes: Vec<String> = records.iter().map(|r| r.source.content_hash.clone()).collect();
    hashes.sort();
    let index_of: BTreeMap<String, usize> =
        hashes.iter().cloned().enumerate().map(|(i, h)| (h, i)).collect();

    // Read directed SIMILAR_TO (src, tgt, score) back from the built graph.
    let mut directed: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for (src, tgt, props) in edges_of(&graph, "SIMILAR_TO") {
        let (Some(&a), Some(&b)) = (index_of.get(&src), index_of.get(&tgt)) else { continue };
        let s = match props.get("score") {
            Some(Value::Float64(v)) => *v,
            _ => continue,
        };
        directed.insert((a, b), s);
    }

    // Single-linkage edge set: every directed edge, canonicalised (min,max).
    let mut single: Vec<(usize, usize, f64)> = Vec::new();
    for (&(a, b), &s) in &directed {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        single.push((lo, hi, s));
    }
    // Mutual-kNN edge set: keep only pairs present in BOTH directions.
    let mut mutual: Vec<(usize, usize, f64)> = Vec::new();
    for (&(a, b), &s) in &directed {
        if a < b && directed.contains_key(&(b, a)) {
            mutual.push((a, b, s));
        }
    }

    let pct = |x: usize| 100.0 * x as f64 / n_tracks as f64;
    let mut out = String::new();
    use std::fmt::Write as _;
    writeln!(out, "P10b style-community tuning — subset n_tracks={n_tracks}").unwrap();
    writeln!(out, "directed SIMILAR_TO edges={}, mutual pairs={}", directed.len(), mutual.len()).unwrap();
    for (label, edges) in [("single-linkage", &single), ("mutual-kNN", &mutual)] {
        writeln!(out, "\n[{label}]").unwrap();
        writeln!(out, "  thr  min  n_styles  largest(%)   coverage(%)").unwrap();
        for &min_size in &[2usize, 3usize] {
            for &thr in &[0.60f64, 0.70, 0.75, 0.80, 0.82, 0.84, 0.85, 0.86, 0.88] {
                let (ns, lg, cov) = sweep(n, &index_of, edges, thr, min_size);
                writeln!(
                    out,
                    "  {thr:.2}  {min_size}    {ns:>4}     {lg:>4} ({:>5.1})  {cov:>4} ({:>5.1})",
                    pct(lg),
                    pct(cov)
                )
                .unwrap();
            }
        }
    }
    eprint!("{out}");
    // Offload the table (scratchpad, not dev-docs).
    if let Ok(scratch) = std::env::var("SONAGRAM_SCRATCH") {
        let _ = std::fs::write(PathBuf::from(scratch).join("p10b-tuning.txt"), &out);
    }
}

/// PERMANENT #[ignore] diagnostic (P10b): build the graph from a library dir
/// ($SONAGRAM_SUBSET_DIR, else the maintainer subset) at the tuned production
/// threshold and dump the Style table (top styles by size: name, n, mean_bpm,
/// top_genres) so the tuned names/communities are auditable. Skips if absent.
///   cargo test -p sonagram --test graph_derive -- --ignored --nocapture style_table_dump
#[test]
#[ignore = "maintainer-only style-table diagnostic, not a gate"]
fn style_table_dump() {
    let dir = std::env::var("SONAGRAM_SUBSET_DIR").unwrap_or_else(|_| {
        "/Volumes/EksternalHome/KristianEX/tmp-sonagram-p10/subset500/.sonagram/analysis".to_string()
    });
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        eprintln!("[skip] subset dir {} absent", dir.display());
        return;
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut records: Vec<AnalysisRecord> = paths
        .iter()
        .map(|p| AnalysisRecord::from_json(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect();
    records.sort_by(|a, b| a.source.content_hash.cmp(&b.source.content_hash));
    let n = records.len();
    let lib = LibraryInfo { root: "subset".to_string(), n_tracks: n };
    let graph = graph::build_graph(&records, &lib).unwrap();

    let mut rows: Vec<(i64, String, f64, String)> = graph
        .type_indices
        .get("Style")
        .map(|r| r.to_vec())
        .unwrap_or_default()
        .into_iter()
        .map(|ni| {
            let node = graph.get_node(ni).unwrap();
            let name = match resolve_node_property(node, "name", &graph) {
                Value::String(s) => s,
                _ => "?".to_string(),
            };
            let nt = match resolve_node_property(node, "n_tracks", &graph) {
                Value::Int64(v) => v,
                _ => 0,
            };
            let bpm = match resolve_node_property(node, "mean_bpm", &graph) {
                Value::Float64(v) => v,
                _ => 0.0,
            };
            let genres = match resolve_node_property(node, "top_genres", &graph) {
                Value::List(v) => v
                    .iter()
                    .map(|g| match g {
                        Value::String(s) => s.clone(),
                        _ => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                _ => String::new(),
            };
            (nt, name, bpm, genres)
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let n_styles = rows.len();
    let coverage: i64 = rows.iter().map(|r| r.0).sum();
    let mut out = String::new();
    use std::fmt::Write as _;
    writeln!(out, "P10b Style table — n_tracks={n}, n_styles={n_styles}, coverage={coverage} ({:.1}%)",
        100.0 * coverage as f64 / n as f64).unwrap();
    writeln!(out, "  rank  n   mean_bpm  name  [top_genres]").unwrap();
    for (i, (nt, name, bpm, genres)) in rows.iter().take(15).enumerate() {
        writeln!(out, "  {:>4}  {nt:>2}  {bpm:>7.1}  {name}  [{genres}]", i + 1).unwrap();
    }
    eprint!("{out}");
    if let Ok(scratch) = std::env::var("SONAGRAM_SCRATCH") {
        let _ = std::fs::write(PathBuf::from(scratch).join("p10b-style-table.txt"), &out);
    }
}

/// Synthetic cluster derived from the committed fixtures (no maintainer dir):
/// force three records to share one embedding so they are mutual nearest
/// neighbours at similarity 1.0 and provably clear the 0.85 bar, forming exactly
/// one Style of three. Exercises the end-to-end Style profile shape + determinism
/// that the (now styleless) diverse fixtures can no longer cover on their own.
fn synthetic_style_records() -> Vec<AnalysisRecord> {
    let mut recs = load_records();
    recs.sort_by(|a, b| a.source.content_hash.cmp(&b.source.content_hash));
    // Three identical 48-dim embeddings ⇒ pairwise similarity 1.0 ⇒ one style.
    let shared = vec![0.5f32; 48];
    for r in recs.iter_mut().take(3) {
        r.analysis.embedding = Some(shared.clone());
    }
    recs
}

#[test]
fn style_profile_is_shaped_and_deterministic_on_synthetic_cluster() {
    let recs = synthetic_style_records();
    let a = graph::build_graph(&recs, &library()).unwrap();
    let b = graph::build_graph(&recs, &library()).unwrap();

    assert_eq!(node_count(&a, "Style"), 1, "the shared-embedding trio forms one style");
    // Id width = max(3, ndigits(n_styles)); one style ⇒ "style-000".
    let id = "style-000";

    let ni_a = a
        .lookup_by_id_readonly("Style", &Value::String(id.to_string()))
        .unwrap_or_else(|| panic!("style {id} present"));
    let node_a = a.get_node(ni_a).unwrap();

    // name: a non-empty template string (2 or 3 dash-segments — the middle
    // acoustic/electric term is omitted for mid-acousticness communities).
    let name = match resolve_node_property(node_a, "name", &a) {
        Value::String(s) => s,
        other => panic!("style name not String: {other:?}"),
    };
    let segs = name.split('-').count();
    assert!((2..=3).contains(&segs), "style name has 2–3 template segments: {name}");

    // exemplar_titles: non-empty, at most 5, deterministic across builds.
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

    // n_tracks == IN_STYLE member count.
    let n = match resolve_node_property(node_a, "n_tracks", &a) {
        Value::Int64(v) => v,
        other => panic!("n_tracks not Int64: {other:?}"),
    };
    let members = edges_of(&a, "IN_STYLE")
        .into_iter()
        .filter(|(_, tgt, _)| tgt == id)
        .count() as i64;
    assert_eq!(n, 3, "the trio's style has 3 tracks");
    assert_eq!(n, members, "n_tracks == IN_STYLE members for {id}");
}
