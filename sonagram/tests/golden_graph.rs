//! THE regression net for sonagram's graph mapping — the gate every future
//! phase runs via `cargo test -p sonagram --test golden_graph`.
//!
//! sonagram's output is a graph, so the net is **digest fidelity +
//! determinism**, not audio fidelity (CLAUDE.md, "The graph gate"). Three
//! standing parts:
//!
//! - [`golden_graph`] — build the graph from the 15 frozen `TrackAnalysis`
//!   fixtures, render it to a deterministic canonical string, SHA-256 it, and
//!   assert the digest equals the committed golden at `tests/goldens/`.
//! - [`determinism`] — the same records built twice, in reversed order, and in
//!   a deterministically shuffled order all yield an identical digest, and node
//!   identity is stable across input reordering. Catches iteration-order and
//!   identity leaks a golden alone would miss.
//! - [`contract_*`] — compiles and asserts against the **real** `sonara` +
//!   `kglite` APIs, so an upstream bump that breaks the mapping fails loudly
//!   here instead of drifting silently.
//!
//! The `#[ignore]`d [`capture_goldens`] regenerates the committed goldens; it
//! is the deliberate regen path — see `GRAPH-GATE.md` for THE RULE.
//!
//! ## The canonical digest
//! [`canonical_graph_string`] is a full, deterministic render of the graph read
//! through the same public kglite accessors the builder and P4 gate use
//! (`type_indices`, `get_node`, `NodeData::{id, properties_cloned}`,
//! `get_edge_type_counts`, the stable-digraph edge view, and `graph.embeddings`).
//! Every input is sorted (`BTreeMap` / sorted `Vec`) and every value is rendered
//! via `Value`'s stable `Debug`, so the render is reproducible across runs and
//! machines. Embedding vectors are rendered as exact f32 bits (`to_bits()` hex)
//! to defeat float-formatting drift.

use std::collections::BTreeMap;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use sonagram::enrich::EnrichmentData;
use sonagram::graph::{self, LibraryInfo, GRAPH_SCHEMA_VERSION};
use sonagram::record::AnalysisRecord;

// ─────────────────────────── fixture loading ────────────────────────────────

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/analyses")
}

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens")
}

/// Load the 15 frozen fixture records. Mirrors the `scan::load_records`
/// contract the builder consumes: read every `*.json` and sort by
/// `content_hash` so input order is fixed regardless of directory-walk order.
fn load_records() -> Vec<AnalysisRecord> {
    let dir = fixtures_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read fixtures dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut records: Vec<AnalysisRecord> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).unwrap();
            AnalysisRecord::from_json(&text)
                .unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
        })
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

/// The frozen Last.fm enrichment fixtures (P12) — hand-crafted, deterministic
/// enrichment for a subset of the 15 tracks (4 artists / 4 tracks / 4 albums),
/// including 2 owned CROWD_SIMILAR track pairs, similar entries pointing at
/// non-owned tracks/artists (which must be dropped), and folksonomy tags that
/// extend the Genre dimension.
fn load_enrichment() -> EnrichmentData {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lastfm");
    EnrichmentData::load_from_dir(&dir)
        .unwrap_or_else(|e| panic!("load enrichment fixtures {}: {e}", dir.display()))
}

// ───────────────────────── canonical rendering ──────────────────────────────

/// Stable canonical form of any value. `Value`'s `Debug` is an enum form (not a
/// map), so it is deterministic across runs; `Cow<Value>` forwards to it.
fn canon<T: std::fmt::Debug>(v: &T) -> String {
    format!("{v:?}")
}

/// A deterministic, exhaustive canonical rendering of the built graph. Two
/// graphs render to the same string iff they are equivalent along every
/// dimension the gate protects: node-type counts, edge-type counts, the sorted
/// (node_type, id) identity set, the full per-node property sweep, the full
/// per-edge property sweep, and every embedding store (dimension / model / metric
/// / per-node vectors in exact f32 bits).
fn canonical_graph_string(g: &kglite::api::DirGraph) -> String {
    let mut s = String::new();
    s.push_str(&format!("## schema_version\n{GRAPH_SCHEMA_VERSION}\n"));

    // ── Node sweep (via type_indices → get_node) ────────────────────────────
    // Grouped so we build counts, identities, the property sweep, and the
    // node-index → id map (used to render embeddings by node identity) in one
    // pass.
    let mut type_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut nodes: Vec<(String, String, BTreeMap<String, String>)> = Vec::new();
    let mut id_by_index: BTreeMap<usize, String> = BTreeMap::new();

    for (ty, refs) in g.type_indices.iter() {
        let tname = ty.to_string();
        type_counts.insert(tname.clone(), refs.len());
        for ni in refs.to_vec() {
            let node = g
                .node_view(ni)
                .unwrap_or_else(|| panic!("type_indices points at missing node in {tname}"));
            let id = canon(&node.id());
            id_by_index.insert(ni.index(), id.clone());
            let props: BTreeMap<String, String> = node
                .properties_cloned(&g.interner)
                .into_iter()
                .map(|(k, v)| (k, canon(&v)))
                .collect();
            nodes.push((tname.clone(), id, props));
        }
    }
    nodes.sort();

    s.push_str("## node_type_counts\n");
    for (ty, n) in &type_counts {
        s.push_str(&format!("{ty}\t{n}\n"));
    }

    // ── Edge-type counts (via get_edge_type_counts) ─────────────────────────
    let edge_counts: BTreeMap<String, usize> = g
        .get_edge_type_counts()
        .iter()
        .map(|(ty, n)| (ty.clone(), *n))
        .collect();
    s.push_str("## edge_type_counts\n");
    for (ty, n) in &edge_counts {
        s.push_str(&format!("{ty}\t{n}\n"));
    }

    s.push_str("## node_identities\n");
    for (ty, id, _) in &nodes {
        s.push_str(&format!("{ty}\t{id}\n"));
    }

    s.push_str("## node_props\n");
    for (ty, id, props) in &nodes {
        s.push_str(&format!("{ty}\t{id}\n"));
        for (k, v) in props {
            s.push_str(&format!("\t{k}={v}\n"));
        }
    }

    // ── Edge sweep (endpoints + props, via the stable-digraph view) ─────────
    let sg = g.graph.as_stable_digraph();
    let mut edges: Vec<(String, String, String, BTreeMap<String, String>)> = Vec::new();
    for e in sg.edge_indices() {
        let edge = sg.edge_weight(e).expect("edge weight");
        let (si, ti) = sg.edge_endpoints(e).expect("edge endpoints");
        let sn = g.node_view(si).expect("edge source node");
        let tn = g.node_view(ti).expect("edge target node");
        let props: BTreeMap<String, String> = edge
            .properties_cloned(&g.interner)
            .into_iter()
            .map(|(k, v)| (k, canon(&v)))
            .collect();
        edges.push((
            edge.connection_type_str(&g.interner).to_string(),
            canon(&sn.id()),
            canon(&tn.id()),
            props,
        ));
    }
    edges.sort();

    s.push_str("## edge_props\n");
    for (conn, src, tgt, props) in &edges {
        s.push_str(&format!("{conn}\t{src}\t{tgt}\n"));
        for (k, v) in props {
            s.push_str(&format!("\t{k}={v}\n"));
        }
    }

    // ── Embedding stores (dimension / model / metric / per-node vectors) ────
    s.push_str("## embeddings\n");
    let mut emb_keys: Vec<&(String, String)> = g.embeddings.keys().collect();
    emb_keys.sort();
    for key in emb_keys {
        let store = &g.embeddings[key];
        let dim = store.dimension;
        s.push_str(&format!(
            "{}::{}\tdim={dim}\tmodel={:?}\tmetric={:?}\n",
            key.0, key.1, store.model_id, store.metric
        ));
        // Render one row per stored vector, keyed by the owning node's id so
        // the order is stable independent of slot assignment. f32 as exact bits
        // (to_bits hex) — no decimal formatting to drift across platforms.
        let mut rows: Vec<(String, String)> = Vec::with_capacity(store.slot_to_node.len());
        for (slot, &node_idx) in store.slot_to_node.iter().enumerate() {
            let id = id_by_index
                .get(&node_idx)
                .cloned()
                .unwrap_or_else(|| format!("idx:{node_idx}"));
            let start = slot * dim;
            let hex: Vec<String> = store.data[start..start + dim]
                .iter()
                .map(|f| format!("{:08x}", f.to_bits()))
                .collect();
            rows.push((id, hex.join(",")));
        }
        rows.sort();
        for (id, hex) in rows {
            s.push_str(&format!("\t{id}\t{hex}\n"));
        }
    }

    s
}

/// SHA-256 (lowercase hex) of the canonical graph rendering — the golden digest.
fn graph_digest(g: &kglite::api::DirGraph) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_graph_string(g).as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read_golden_file(name: &str) -> String {
    let path = goldens_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "missing golden {} — capture with `cargo test -p sonagram --test golden_graph -- --ignored capture_goldens`: {e}",
                path.display()
            )
        })
        .trim()
        .to_string()
}

fn read_golden() -> String {
    read_golden_file("library.sha256")
}

// ──────────────────────────── part 1: golden ────────────────────────────────

/// Build the graph from the 15 frozen fixtures, digest it, and assert the
/// digest matches the committed golden. On mismatch, print both digests and the
/// first line where the live canonical string diverges from the committed
/// snapshot, so a digest diff is explainable rather than opaque.
#[test]
fn golden_graph() {
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    let got = graph_digest(&graph);
    let want = read_golden();

    if got != want {
        let live = canonical_graph_string(&graph);
        let snap_path = goldens_dir().join("library.canonical.txt");
        let diff = match std::fs::read_to_string(&snap_path) {
            Ok(committed) => first_diff(&committed, &live),
            Err(e) => format!("(could not read {}: {e})", snap_path.display()),
        };
        panic!(
            "golden digest mismatch\n  golden (committed) = {want}\n  live build         = {got}\n\
             first canonical difference:\n{diff}\n\n\
             If this graph change is INTENTIONAL, regenerate the goldens in the SAME commit and\n\
             explain why in the commit body (GRAPH-GATE.md, THE RULE):\n\
             `cargo test -p sonagram --test golden_graph -- --ignored capture_goldens`"
        );
    }
}

/// First differing line between the committed canonical snapshot and the live
/// render, with line numbers. Returns a note if they match line-for-line but
/// differ in length.
fn first_diff(committed: &str, live: &str) -> String {
    for (i, (a, b)) in committed.lines().zip(live.lines()).enumerate() {
        if a != b {
            return format!("  line {}:\n    committed: {a}\n    live:      {b}", i + 1);
        }
    }
    let (nc, nl) = (committed.lines().count(), live.lines().count());
    if nc != nl {
        format!("  line counts differ: committed={nc} live={nl}")
    } else {
        "  (no line-level difference found — check trailing bytes)".to_string()
    }
}

// ───────────────────── part 1b: enriched golden (P12) ───────────────────────

/// Build the graph from the 15 fixtures **plus** the frozen Last.fm enrichment,
/// digest it, and assert it matches the committed enriched golden. This is a
/// SECOND golden (distinct from the plain one): the enrichment adds popularity /
/// MBID / original-album props, folksonomy `IN_GENRE` edges, and `CROWD_SIMILAR`
/// edges. The plain `golden_graph` above proves the un-enriched path is
/// unchanged; this proves the enrichment mapping is stable.
#[test]
fn golden_graph_enriched() {
    let enr = load_enrichment();
    let graph =
        graph::build_graph_with_enrichment(&load_records(), Some(&enr), &library()).unwrap();
    let got = graph_digest(&graph);
    let want = read_golden_file("library-enriched.sha256");

    if got != want {
        let live = canonical_graph_string(&graph);
        let snap_path = goldens_dir().join("library-enriched.canonical.txt");
        let diff = match std::fs::read_to_string(&snap_path) {
            Ok(committed) => first_diff(&committed, &live),
            Err(e) => format!("(could not read {}: {e})", snap_path.display()),
        };
        panic!(
            "enriched golden digest mismatch\n  golden (committed) = {want}\n  live build         = {got}\n\
             first canonical difference:\n{diff}\n\n\
             If this enrichment mapping change is INTENTIONAL, regenerate the goldens in the SAME\n\
             commit and explain why (GRAPH-GATE.md, THE RULE):\n\
             `cargo test -p sonagram --test golden_graph -- --ignored capture_goldens`"
        );
    }
}

/// The enriched build is also deterministic across input reordering.
#[test]
fn determinism_enriched() {
    let enr = load_enrichment();
    let records = load_records();
    let d0 = graph_digest(
        &graph::build_graph_with_enrichment(&records, Some(&enr), &library()).unwrap(),
    );
    let mut reversed = records.clone();
    reversed.reverse();
    let dr = graph_digest(
        &graph::build_graph_with_enrichment(&reversed, Some(&enr), &library()).unwrap(),
    );
    assert_eq!(d0, dr, "enriched build must be order-independent");
}

// ─────────────────────────── part 2: determinism ────────────────────────────

/// The same records built twice, in reversed order, and in a deterministically
/// shuffled order must all yield the identical digest — `build_graph` sorts
/// internally, so input order must not leak into node identity or ordering.
#[test]
fn determinism() {
    let records = load_records();

    let d0 = graph_digest(&graph::build_graph(&records, &library()).unwrap());

    // Same input, second build.
    let d1 = graph_digest(&graph::build_graph(&records, &library()).unwrap());
    assert_eq!(
        d0, d1,
        "two builds of the same records must be byte-identical"
    );

    // Reversed input order.
    let mut reversed = records.clone();
    reversed.reverse();
    let dr = graph_digest(&graph::build_graph(&reversed, &library()).unwrap());
    assert_eq!(d0, dr, "reversed input order must yield the same digest");

    // Deterministic shuffle: a fixed swap pattern (no rand dependency).
    let mut shuffled = records.clone();
    let n = shuffled.len();
    for i in 0..n {
        // A fixed, seed-free permutation via modular strides.
        let j = (i * 7 + 3) % n;
        shuffled.swap(i, j);
    }
    let ds = graph_digest(&graph::build_graph(&shuffled, &library()).unwrap());
    assert_eq!(
        d0, ds,
        "deterministically shuffled input must yield the same digest"
    );
}

// ──────────────────────────── part 3: contract ──────────────────────────────
//
// Compile-time + runtime assertions against the REAL upstream APIs. A version
// bump that renames/removes anything sonagram maps fails HERE, loudly, instead
// of drifting into a silent mapping bug. Each assert says what breaks downstream.

/// Compile-time proof that every `TrackAnalysis` field sonagram maps still
/// exists with the same name. Never called — it exists only so the compiler
/// rejects an upstream rename/removal/addition (no `..`, so an added field is a
/// non-exhaustive-pattern error and forces us to decide whether to map it).
///
/// WHY: `AnalysisRecord::from_analysis` destructures the same fields into the
/// cache/fixture DTO. If sonara reshapes `TrackAnalysis`, our mapping silently
/// drops or misreads data unless this breaks the build first.
#[allow(dead_code)]
fn _track_analysis_fields_present(a: sonara::analyze::TrackAnalysis) {
    let sonara::analyze::TrackAnalysis {
        provenance: _,
        duration_sec: _,
        bpm: _,
        bpm_raw: _,
        bpm_confidence: _,
        bpm_candidates: _,
        beats: _,
        onset_frames: _,
        rms_mean: _,
        rms_max: _,
        loudness_lufs: _,
        dynamic_range_db: _,
        true_peak_db: _,
        replaygain_db: _,
        loudness_curve: _,
        loudness_momentary_max_db: _,
        loudness_range_lu: _,
        spectral_centroid_mean: _,
        zero_crossing_rate: _,
        onset_density: _,
        spectral_bandwidth_mean: _,
        spectral_rolloff_mean: _,
        spectral_flatness_mean: _,
        spectral_contrast_mean: _,
        mfcc_mean: _,
        chroma_mean: _,
        tempo_curve: _,
        tempo_variability: _,
        time_signature: _,
        time_signature_confidence: _,
        chord_sequence: _,
        chord_events: _,
        chord_change_rate: _,
        predominant_chord: _,
        dissonance: _,
        energy: _,
        danceability: _,
        key: _,
        key_confidence: _,
        key_camelot: _,
        valence: _,
        acousticness: _,
        embedding: _,
        aggression_score: _,
        aggression_confidence: _,
        aggression_forcefulness: _,
        aggression_harshness: _,
        aggression_tension: _,
        aggression_rhythm: _,
        mood_happy: _,
        mood_aggressive: _,
        mood_relaxed: _,
        mood_sad: _,
        instrumentalness: _,
        genre: _,
        genre_confidence: _,
        grid_offset_sec: _,
        downbeats: _,
        grid_stability: _,
        energy_curve: _,
        energy_curve_hop_sec: _,
        segments: _,
        intro_end_sec: _,
        outro_start_sec: _,
        energy_level: _,
        leading_silence_sec: _,
        trailing_silence_sec: _,
        key_candidates: _,
        vocalness: _,
        fingerprint: _,
        embedding_version: _,
        tags: _,
    } = a;
}

/// Compile-time proof that every `TrackTags` field sonagram maps still exists with
/// the same name (no `..`, so an upstream rename/removal/addition breaks the
/// build). WHY: `AnalysisRecord::from_analysis` mirrors these into `TagsDto`
/// (title/artist/album/genre/year/original_year/track_no). `original_year` is the
/// sonara-0.2.4 era field the graph now prefers for `Decade`/`FROM_DECADE`; if
/// sonara drops or renames it, era reasoning silently regresses unless this fails
/// first.
#[allow(dead_code)]
fn _track_tags_fields_present(t: sonara::analyze::TrackTags) {
    let sonara::analyze::TrackTags {
        title: _,
        artist: _,
        album: _,
        genre: _,
        year: _,
        original_year: _,
        track_no: _,
    } = t;
}

/// Compile-time proof that every `AnalysisProvenance` field mirrored by
/// `ProvenanceDto` still exists. The exhaustive pattern also forces a conscious
/// mapping decision whenever sonara adds provenance that affects persisted
/// analysis identity.
#[allow(dead_code)]
fn _analysis_provenance_fields_present(p: sonara::analyze::AnalysisProvenance) {
    let sonara::analyze::AnalysisProvenance {
        schema_version: _,
        sample_rate: _,
        hop_length: _,
        mode: _,
        requested_features: _,
        // sonara 0.3.6 added these additively: the octave-folding tempo range in
        // effect at analysis time. We do not persist them yet (a change in the
        // configured range invalidates stored `bpm`, which future freshness
        // logic will want) — but the exhaustive pattern forces that to stay a
        // conscious decision rather than an oversight.
        bpm_min: _,
        bpm_max: _,
        genre_model_id: _,
        vocalness_model_id: _,
        aggression_model_id: _,
    } = p;
}

#[test]
fn contract_sonara() {
    use sonara::analyze::{AnalysisConfig, AnalysisMode, ANALYSIS_SCHEMA_VERSION};
    use sonara::similarity::{EMBEDDING_DIM, SIMILARITY_VERSION, WEIGHTS};

    // AnalysisConfig / AnalysisMode construction: the scanner builds these to
    // request features. WHY: a field/variant rename breaks `scan::analysis_config`.
    // Keep an exhaustive literal so upstream field changes fail at compile
    // time. Sonagram supplies no genre model; it does opt into Sonara's own
    // validated bundled vocalness model in `scan::default_analysis_config`.
    let _cfg = AnalysisConfig {
        mode: AnalysisMode::Playlist,
        features: None,
        bpm_min: None,
        bpm_max: None,
        genre_model: None,
        vocalness_model: None,
    };
    let configured = sonagram::scan::default_analysis_config()
        .expect("Sonara's embedded vocalness model must validate");
    let vocalness_model = configured
        .vocalness_model
        .expect("Sonagram deliberately enables the bundled vocalness model");
    assert_eq!(vocalness_model.id(), sonagram::scan::VOCALNESS_MODEL_ID);
    assert_eq!(vocalness_model.embedding_version(), SIMILARITY_VERSION);
    assert!(configured
        .features
        .as_ref()
        .is_some_and(|features| features.contains("aggression")));
    assert_eq!(
        sonagram::scan::AGGRESSION_MODEL_ID,
        sonara::aggression::AGGRESSION_MODEL_ID
    );
    assert_eq!(sonara::aggression::AGGRESSION_MODEL_VERSION, 3);
    assert_eq!(sonara::aggression::AGGRESSION_SAMPLE_RATE, 22_050);
    assert_eq!(sonara::aggression::AGGRESSION_FEATURE_COUNT, 39);

    // WHY: each Track carries `analysis_schema_version`. If sonara bumps its
    // schema, previously captured fixtures no longer describe the same analysis
    // semantics — the goldens must be recaptured, not silently trusted. Pinned to
    // 6 as of sonara 0.3.3 (sample-rate-stable aggression rank v3); a future
    // bump must recapture the 15 fixtures + regen the golden in the same commit.
    assert_eq!(
        ANALYSIS_SCHEMA_VERSION, 6,
        "sonara ANALYSIS_SCHEMA_VERSION changed — recapture fixtures + goldens"
    );

    // WHY: the embedding store is built at exactly this width; a mismatch means
    // `preweight` and `EmbeddingStore::with_metric(EMBEDDING_DIM, ..)` would
    // store vectors of the wrong dimension and vector search would be corrupt.
    assert_eq!(
        EMBEDDING_DIM, 48,
        "sonara EMBEDDING_DIM changed — remap the embedding store width"
    );
    assert_eq!(
        WEIGHTS.len(),
        48,
        "sonara WEIGHTS length changed — `preweight` zips weights against the 48-dim vector"
    );

    // WHY: `graph::embedding_model_id()` derives the store's model_id from this
    // (format "sonara-similarity-v{N}"); a bump re-tags every stored vector and
    // moves the golden digest. Pinned to 2 as of sonara 0.2.3 — a change means
    // stored embeddings must be regenerated (the model_id + golden move with it).
    assert_eq!(
        SIMILARITY_VERSION, 2,
        "sonara SIMILARITY_VERSION changed — embedding_model_id + goldens move; recapture"
    );

    // ── sonara 0.3.6: the per-feature augment lane (pinned, not yet consumed) ──
    //
    // WHY pin an API we do not call: the incremental-rescan work plans cache
    // freshness on exactly this surface — "can this one feature be recomputed
    // from the record we already hold, without decoding the audio, and if not
    // why not". Sonagram consumes it in a later plan; until then a rename or a
    // reshape upstream would only surface when that plan starts. These
    // ascriptions make it a compile error in the gate instead.
    let _: fn(&sonara::analyze::TrackAnalysis, &str) -> Option<sonara::analyze::AugmentBlocker> =
        sonara::analyze::augment_blocker;
    let _: fn(&sonara::analyze::TrackAnalysis, &str) -> bool = sonara::analyze::can_augment;
    let _: fn(
        &sonara::analyze::TrackAnalysis,
        &[&str],
        Option<&std::path::Path>,
        &AnalysisConfig,
    ) -> sonara::Result<sonara::analyze::TrackAnalysis> = sonara::analyze::augment_analysis;

    // The blocker enum is the *reason* half of that contract — a scan planner
    // branches on these variants (re-analyze vs. decode vs. skip), so a variant
    // rename or a payload reshape must fail here too.
    let _blockers = [
        sonara::analyze::AugmentBlocker::UnknownFeature,
        sonara::analyze::AugmentBlocker::NeedsAudio(sonara::analyze::DependencyClass::Audio),
        sonara::analyze::AugmentBlocker::SchemaVersionMismatch {
            record: 5,
            current: 6,
        },
        sonara::analyze::AugmentBlocker::EmbeddingVersionMismatch {
            record: 1,
            current: 2,
        },
        sonara::analyze::AugmentBlocker::MissingEvidence(vec!["chroma_mean"]),
    ];

    // The dependency map itself: an exhaustive destructure (a new row field is a
    // conscious decision) plus the two rows whose class decides whether a
    // rescan can stay decode-free.
    let deps: BTreeMap<&'static str, sonara::analyze::FeatureDependency> =
        sonara::analyze::feature_dependencies()
            .map(|dep| (dep.name, dep))
            .collect();
    assert!(
        !deps.is_empty(),
        "sonara feature_dependencies() is empty — the dependency map vanished"
    );
    for dep in deps.values() {
        let sonara::analyze::FeatureDependency {
            name: _,
            class: _,
            required_evidence: _,
            needs_extended: _,
            opt_in_only: _,
            full_only: _,
        } = dep;
    }
    let embedding = deps
        .get("embedding")
        .expect("sonara must declare an `embedding` feature — the store is built from it");
    assert_eq!(
        embedding.class,
        sonara::analyze::DependencyClass::Embedding,
        "`embedding` left the Embedding class — decode-free re-embedding is what \
         an incremental rescan is built on"
    );
    assert!(
        !embedding.required_evidence.is_empty(),
        "`embedding` declares no required evidence — a decode-free recompute \
         cannot be planned against an empty evidence list"
    );
    let aggression = deps
        .get("aggression")
        .expect("the `aggression` cargo feature is enabled, so its row must exist");
    assert_eq!(
        aggression.class,
        sonara::analyze::DependencyClass::Audio,
        "`aggression` is Audio-class: it can never be augmented decode-free, and \
         a rescan planner that believes otherwise would silently skip a decode"
    );

    // ── sonara 0.3.6: versioned similarity profiles ─────────────────────────
    //
    // The Rust core exposes the selectable set as `SimilarityProfile::ALL` +
    // `name()`/`version()`; sonara-python publishes the same pairs as the
    // `SIMILARITY_PROFILES` dict. Profiles are applied at distance time and
    // never change the stored vector, so adding one must not move a digest —
    // this probe pins the set, the versions, and (crucially) that `default`
    // still selects the historical `WEIGHTS` table our goldens were built with.
    let profiles: BTreeMap<&'static str, u32> = sonara::similarity::SimilarityProfile::ALL
        .iter()
        .map(|p| (p.name(), p.version()))
        .collect();
    assert_eq!(
        profiles.get("default").copied(),
        Some(2),
        "sonara similarity profile `default` is not version 2 (profiles: {profiles:?}) — \
         the default table is what every stored embedding is compared under"
    );
    assert_eq!(
        profiles.get("timbre").copied(),
        Some(1),
        "sonara similarity profile `timbre` is not version 1 (profiles: {profiles:?})"
    );
    assert_eq!(
        sonara::similarity::SimilarityProfile::default(),
        sonara::similarity::SimilarityProfile::Default,
        "the default `SimilarityProfile` is no longer `Default` — every distance \
         we compute without naming a profile would silently change metric"
    );
    assert_eq!(
        sonara::similarity::SimilarityProfile::Default.weights(),
        &WEIGHTS,
        "the `default` profile no longer selects the historical WEIGHTS table — \
         distances (and any digest derived from them) move"
    );
}

#[test]
fn contract_kglite() {
    use kglite::api::mutation::{add_nodes, EdgeSpec};
    use kglite::api::mutation::{ColumnData, ColumnType, DataFrame};
    use kglite::api::storage::{EmbeddingStore, StorageMode};
    use kglite::api::{DirGraph, NodeView, Value};
    use std::collections::HashMap;
    use std::sync::Arc;

    // EdgeSpec construction: the builder emits these for every graph edge.
    // WHY: a field rename breaks `graph::edge` / `add_edges_from_specs`.
    let _spec = EdgeSpec {
        source_type: "Track".to_string(),
        source_id: Value::String("x".to_string()),
        target_type: "Artist".to_string(),
        target_id: Value::String("y".to_string()),
        edge_type: "BY_ARTIST".to_string(),
        properties: HashMap::new(),
    };

    // DataFrame::new + EmbeddingStore::new: the builder stages every node table
    // and the similarity store through these. WHY: a signature change breaks
    // `graph::build_df` / the embedding-store construction.
    let _df = DataFrame::new(Vec::new());
    assert_eq!(
        EmbeddingStore::new(48).dimension,
        48,
        "kglite EmbeddingStore no longer honors its constructor dimension"
    );

    // WHY: the `.kgl` open path (P8 handoff) selects a storage mode; `Memory`
    // must remain a variant or that path won't compile.
    let _mode = StorageMode::Memory;

    // ── the 0.16.0 identity/read/save contract (three probes) ───────────────
    //
    // kglite 0.16.0 moved node identity into the per-type column store: on a
    // memory-backed graph the raw `NodeData::id()`/`.title()` fields hold a
    // `Value::Null` *sentinel* and only a resolving read (`NodeView::id`)
    // answers what the caller meant. That shape is silent — a raw `.id()` read
    // still compiles and just returns Null — and during the 0.16.2 migration it
    // dropped every curation relation before it was caught. These three probes
    // are the mechanical detectors for that class of engine change.

    // (a) `resolve_node_property` takes the *view*, not `&NodeData`. WHY: this
    // is the one property read the whole curation projection is built on
    // (`curation/project.rs`, `playlist.rs`, `cli.rs`); pinning the signature
    // makes a receiver change a compile error here instead of a silent Null.
    let _: fn(NodeView<'_>, &str, &DirGraph) -> Value = kglite::api::cypher::resolve_node_property;

    // (b) THE tripwire: a freshly built, memory-backed node must answer its
    // *inserted* id through `node_view`, not the Null sentinel. WHY: node
    // identity read back from the graph is what every edge sweep keys on
    // (`project_tracks` resolves each edge endpoint to an id). If an engine bump
    // moves identity out from under this read again, this assertion is the only
    // thing between it and a silently empty relation set — the goldens render
    // the graph, not the projection, so they would stay green.
    let mut probe = DirGraph::new();
    let mut probe_df = DataFrame::new(Vec::new());
    probe_df
        .add_column(
            "id".to_string(),
            ColumnType::String,
            ColumnData::String(vec![Some("probe-id".to_string())]),
        )
        .expect("add_column(id)");
    probe_df
        .add_column(
            "name".to_string(),
            ColumnType::String,
            ColumnData::String(vec![Some("Probe".to_string())]),
        )
        .expect("add_column(name)");
    add_nodes(
        &mut probe,
        probe_df,
        "Probe".to_string(),
        "id".to_string(),
        Some("name".to_string()),
        None,
    )
    .expect("kglite add_nodes rejected a one-row batch");
    let probe_idx = probe
        .type_indices
        .get("Probe")
        .and_then(|nodes| nodes.to_vec().into_iter().next())
        .expect("kglite add_nodes registered no Probe index");
    let probe_id = probe
        .node_view(probe_idx)
        .expect("no NodeView for a just-added node")
        .id();
    assert_ne!(
        *probe_id,
        Value::Null,
        "kglite node identity no longer resolves through NodeView::id — a raw \
         read now answers the Null sentinel; re-audit every `.id()` acquisition \
         (this is the 0.16.0 class of change)"
    );
    assert_eq!(
        *probe_id,
        Value::String("probe-id".to_string()),
        "kglite NodeView::id returned an id sonagram never inserted"
    );

    // (c) `save_graph` reports a typed `SaveError`, not a `String`. WHY:
    // `graph::save` is the single save choke point and maps this into
    // `SonagramError::Graph` via `e.to_string()`; a change in the error shape
    // (or in the `&mut Arc<DirGraph>` receiver) must fail here, loudly.
    let _: fn(&mut Arc<DirGraph>, &str) -> Result<(), kglite::api::io::SaveError> =
        kglite::api::io::save_graph;
}

// ─────────────────────── part 4: golden regeneration ────────────────────────

/// Regenerate `tests/goldens/library.sha256` + `library.canonical.txt` from the
/// current code. `#[ignore]` so it never runs in the normal suite — the
/// deliberate regen path (GRAPH-GATE.md, THE RULE). Run explicitly:
///
///   cargo test -p sonagram --test golden_graph -- --ignored capture_goldens
#[test]
#[ignore = "regeneration path; run explicitly with --ignored capture_goldens"]
fn capture_goldens() {
    let graph = graph::build_graph(&load_records(), &library()).unwrap();
    let canonical = canonical_graph_string(&graph);
    let digest = graph_digest(&graph);

    let dir = goldens_dir();
    std::fs::create_dir_all(&dir).expect("create goldens dir");
    std::fs::write(dir.join("library.sha256"), format!("{digest}\n")).expect("write digest golden");
    std::fs::write(dir.join("library.canonical.txt"), &canonical).expect("write canonical golden");

    let bytes = canonical.len();
    eprintln!("captured library golden -> {digest}");
    eprintln!(
        "canonical snapshot: {bytes} bytes ({} lines)",
        canonical.lines().count()
    );
    assert!(
        bytes < 2 * 1024 * 1024,
        "canonical snapshot exceeded 2MB ({bytes} bytes) — store counts+ids only instead"
    );

    // P12: the enriched golden (built WITH the frozen Last.fm enrichment).
    let enr = load_enrichment();
    let egraph =
        graph::build_graph_with_enrichment(&load_records(), Some(&enr), &library()).unwrap();
    let ecanonical = canonical_graph_string(&egraph);
    let edigest = graph_digest(&egraph);
    std::fs::write(dir.join("library-enriched.sha256"), format!("{edigest}\n"))
        .expect("write enriched digest golden");
    std::fs::write(dir.join("library-enriched.canonical.txt"), &ecanonical)
        .expect("write enriched canonical golden");
    eprintln!("captured enriched golden -> {edigest}");
}
