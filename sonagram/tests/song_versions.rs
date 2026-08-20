//! P21 Stage C integration gate: the Song version layer over a small synthetic
//! record set.
//!
//! Three recordings of one song ("Yesterday" by The Beatles — a studio master, a
//! live take, and a demo) plus a singleton ("Let It Be") are built into a graph.
//! Asserts the Stage-C contract end to end: one `Song` node for the ≥2-version
//! group (none for the singleton), a `VERSION_OF` edge from every member,
//! `n_versions` / `canonical_hash` on the `Song`, and `is_canonical` stamped
//! `true` on the master + the singleton and `false` on the inferior takes. The
//! master carries the strongest recording-quality inputs (widest loudness spread,
//! lowest dissonance, highest bpm confidence) so the quality composite — not the
//! content hash — selects it as canonical.

use kglite::api::cypher::resolve_node_property;
use kglite::api::{DirGraph, Value};
use sonagram::enrich::{EnrichmentData, TrackEnrich};
use sonagram::graph::{self, LibraryInfo};
use sonagram::record::{AnalysisDto, AnalysisRecord, ProvenanceDto, SourceInfo, TagsDto};

/// Master recording of "Yesterday" — the intended canonical take. Its hash is
/// deliberately NOT the smallest, so passing the test proves quality (not the
/// content-hash tie-break) drove the canonical selection.
const MASTER_HASH: &str = "zzzz_master";
const LIVE_HASH: &str = "aaaa_live";
const DEMO_HASH: &str = "mmmm_demo";
const SINGLETON_HASH: &str = "bbbb_letitbe";

/// A record whose recording-quality inputs are controlled by `q` (higher = better
/// master signal): loudness-curve spread (`macro_dynamics`), `dissonance`
/// (inverted in the composite), and `bpm_confidence`.
fn rec(hash: &str, artist: &str, title: &str, q: f32) -> AnalysisRecord {
    // Wider loudness spread → higher macro_dynamics; lower dissonance and higher
    // bpm_confidence → higher recording_quality. All three move with `q`.
    let spread = 2.0 + 12.0 * q;
    let loudness_curve = vec![-12.0 - spread, -12.0 + spread];
    let dissonance = 0.7 - 0.6 * q;
    let bpm_confidence = 0.3 + 0.6 * q;

    let mut a = minimal_analysis();
    a.loudness_curve = Some(loudness_curve);
    a.dissonance = Some(dissonance);
    a.bpm_confidence = bpm_confidence;

    AnalysisRecord {
        record_version: 1,
        source: SourceInfo {
            content_hash: hash.to_string(),
            hash_kind: "whole-file-v0".to_string(),
            path: format!("{hash}.mp3"),
            file_size: 1,
            format: "mp3".to_string(),
        },
        tags: Some(TagsDto {
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            album: None,
            genre: None,
            year: None,
            original_year: None,
            track_no: None,
        }),
        analysis: a,
    }
}

fn records() -> Vec<AnalysisRecord> {
    vec![
        rec(MASTER_HASH, "The Beatles", "Yesterday", 1.0),
        rec(
            LIVE_HASH,
            "The Beatles",
            "Yesterday - Live at Shea Stadium",
            0.5,
        ),
        rec(DEMO_HASH, "The Beatles", "Yesterday (Demo)", 0.0),
        rec(SINGLETON_HASH, "The Beatles", "Let It Be", 0.4),
    ]
}

fn library() -> LibraryInfo {
    LibraryInfo {
        root: "synthetic".to_string(),
        n_tracks: 4,
    }
}

fn node_count(graph: &DirGraph, node_type: &str) -> usize {
    graph
        .type_indices
        .get(node_type)
        .map(|r| r.len())
        .unwrap_or(0)
}

fn str_prop(graph: &DirGraph, node_type: &str, id: &str, prop: &str) -> String {
    let ni = graph
        .lookup_by_id_readonly(node_type, &Value::String(id.to_string()))
        .unwrap_or_else(|| panic!("no {node_type} node with id {id}"));
    let node = graph.node_view(ni).unwrap();
    match resolve_node_property(node, prop, graph) {
        Value::String(s) => s,
        other => panic!("{prop} is not String: {other:?}"),
    }
}

fn int_prop(graph: &DirGraph, node_type: &str, id: &str, prop: &str) -> i64 {
    let ni = graph
        .lookup_by_id_readonly(node_type, &Value::String(id.to_string()))
        .unwrap_or_else(|| panic!("no {node_type} node with id {id}"));
    let node = graph.node_view(ni).unwrap();
    match resolve_node_property(node, prop, graph) {
        Value::Int64(v) => v,
        other => panic!("{prop} is not Int64: {other:?}"),
    }
}

fn bool_prop(graph: &DirGraph, node_type: &str, id: &str, prop: &str) -> bool {
    let ni = graph
        .lookup_by_id_readonly(node_type, &Value::String(id.to_string()))
        .unwrap_or_else(|| panic!("no {node_type} node with id {id}"));
    let node = graph.node_view(ni).unwrap();
    match resolve_node_property(node, prop, graph) {
        Value::Boolean(v) => v,
        other => panic!("{prop} is not Boolean: {other:?}"),
    }
}

fn float_prop(graph: &DirGraph, node_type: &str, id: &str, prop: &str) -> f64 {
    let ni = graph
        .lookup_by_id_readonly(node_type, &Value::String(id.to_string()))
        .unwrap_or_else(|| panic!("no {node_type} node with id {id}"));
    let node = graph.node_view(ni).unwrap();
    match resolve_node_property(node, prop, graph) {
        Value::Float64(v) => v,
        other => panic!("{prop} is not Float64: {other:?}"),
    }
}

/// All VERSION_OF edges as `(track_hash, song_id)`.
fn version_of_edges(graph: &DirGraph) -> Vec<(String, String)> {
    let sg = graph.graph.as_stable_digraph();
    let mut out = Vec::new();
    for e in sg.edge_indices() {
        let edge = sg.edge_weight(e).unwrap();
        if edge.connection_type_str(&graph.interner) != "VERSION_OF" {
            continue;
        }
        let (si, ti) = sg.edge_endpoints(e).unwrap();
        let src = match graph.node_view(si).unwrap().id().into_owned() {
            Value::String(s) => s,
            other => panic!("track id not a string: {other:?}"),
        };
        let tgt = match graph.node_view(ti).unwrap().id().into_owned() {
            Value::String(s) => s,
            other => panic!("song id not a string: {other:?}"),
        };
        out.push((src, tgt));
    }
    out.sort();
    out
}

#[test]
fn song_layer_groups_versions_and_flags_canonical() {
    let graph = graph::build_graph(&records(), &library()).unwrap();

    // Exactly one Song node — the 3-version group; the singleton has none.
    assert_eq!(
        node_count(&graph, "Song"),
        1,
        "one Song for the ≥2-version group"
    );

    let song_id = "The Beatles|yesterday";
    assert_eq!(str_prop(&graph, "Song", song_id, "title"), "yesterday");
    assert_eq!(str_prop(&graph, "Song", song_id, "artist"), "The Beatles");
    assert_eq!(int_prop(&graph, "Song", song_id, "n_versions"), 3);
    assert_eq!(
        str_prop(&graph, "Song", song_id, "canonical_hash"),
        MASTER_HASH,
        "highest recording_quality member is canonical, despite its larger hash"
    );

    // One VERSION_OF edge per member, all pointing at the one Song.
    let edges = version_of_edges(&graph);
    assert_eq!(
        edges,
        vec![
            (LIVE_HASH.to_string(), song_id.to_string()),
            (DEMO_HASH.to_string(), song_id.to_string()),
            (MASTER_HASH.to_string(), song_id.to_string()),
        ]
    );

    // is_canonical: master + singleton true; inferior takes false.
    assert!(bool_prop(&graph, "Track", MASTER_HASH, "is_canonical"));
    assert!(bool_prop(&graph, "Track", SINGLETON_HASH, "is_canonical"));
    assert!(!bool_prop(&graph, "Track", LIVE_HASH, "is_canonical"));
    assert!(!bool_prop(&graph, "Track", DEMO_HASH, "is_canonical"));
}

#[test]
fn song_layer_is_order_independent() {
    let base = records();
    let mut reversed = base.clone();
    reversed.reverse();

    let g0 = graph::build_graph(&base, &library()).unwrap();
    let g1 = graph::build_graph(&reversed, &library()).unwrap();

    for g in [&g0, &g1] {
        assert_eq!(node_count(g, "Song"), 1);
        assert_eq!(
            str_prop(g, "Song", "The Beatles|yesterday", "canonical_hash"),
            MASTER_HASH
        );
    }
    assert_eq!(version_of_edges(&g0), version_of_edges(&g1));
}

#[test]
fn recognized_release_beats_higher_quality_unmatched_take() {
    let records = records();
    let mut enrichment = EnrichmentData::default();
    enrichment.tracks.insert(
        LIVE_HASH.to_string(),
        TrackEnrich {
            listeners: Some(1_000),
            playcount: Some(5_000),
            fetched: true,
            ..TrackEnrich::default()
        },
    );
    let graph =
        graph::build_graph_with_enrichment(&records, Some(&enrichment), &library()).unwrap();
    assert_eq!(
        str_prop(&graph, "Song", "The Beatles|yesterday", "canonical_hash"),
        LIVE_HASH
    );
    assert!(bool_prop(&graph, "Track", LIVE_HASH, "has_lastfm_match"));
    assert!(!bool_prop(&graph, "Track", MASTER_HASH, "has_lastfm_match"));
    assert!(bool_prop(&graph, "Track", LIVE_HASH, "is_canonical"));
    assert!(!bool_prop(&graph, "Track", MASTER_HASH, "is_canonical"));
}

#[test]
fn audio_refinement_preserves_track_properties_and_round_trips() {
    let mut records = vec![
        rec("known_a", "Known Artist", "Focus", 0.6),
        rec("known_b", "Known Artist", "Focus - Live", 0.2),
        rec("junk", "Unknown Artist", "Focus", 1.0),
        rec("cover", "Cover Artist", "Focus", 0.9),
    ];
    for record in &mut records {
        record.analysis.embedding = Some(vec![0.25; 48]);
        record.analysis.embedding_version = Some(1);
    }
    let mut graph = graph::build_graph(&records, &library()).unwrap();
    let song_id = "Known Artist|focus";
    assert_eq!(int_prop(&graph, "Song", song_id, "n_versions"), 3);
    assert_eq!(str_prop(&graph, "Song", song_id, "canonical_hash"), "junk");
    assert_eq!(
        version_of_edges(&graph),
        vec![
            ("junk".to_string(), song_id.to_string()),
            ("known_a".to_string(), song_id.to_string()),
            ("known_b".to_string(), song_id.to_string()),
        ],
        "junk singleton attaches but the known-artist cover does not"
    );
    assert_eq!(str_prop(&graph, "Track", "junk", "title"), "Focus");
    assert_eq!(
        str_prop(&graph, "Track", "junk", "artist_name"),
        "Unknown Artist"
    );
    assert_eq!(float_prop(&graph, "Track", "junk", "bpm"), 120.0);
    assert!(bool_prop(&graph, "Track", "junk", "is_canonical"));
    assert!(bool_prop(&graph, "Track", "cover", "is_canonical"));

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "sonagram-song-refine-{}-{stamp}.kgl",
        std::process::id()
    ));
    graph::save(&mut graph, &path).unwrap();
    let loaded = kglite::api::io::load_file(path.to_str().unwrap()).unwrap();
    assert_eq!(int_prop(&loaded, "Song", song_id, "n_versions"), 3);
    assert_eq!(str_prop(&loaded, "Track", "junk", "title"), "Focus");
    assert_eq!(float_prop(&loaded, "Track", "junk", "bpm"), 120.0);
    assert!(bool_prop(&loaded, "Track", "junk", "is_canonical"));
    let _ = std::fs::remove_file(path);
}

/// A record with every optional analysis field left None/empty (the identity +
/// quality-input fields are overridden by [`rec`]).
fn minimal_analysis() -> AnalysisDto {
    AnalysisDto {
        provenance: ProvenanceDto {
            schema_version: 3,
            sample_rate: 22050,
            hop_length: 512,
            mode: "playlist".to_string(),
            requested_features: None,
            genre_model_id: None,
            vocalness_model_id: None,
            aggression_model_id: None,
        },
        duration_sec: 180.0,
        bpm: 120.0,
        bpm_raw: 120.0,
        bpm_confidence: 0.0,
        bpm_candidates: vec![],
        beats: vec![],
        onset_frames: vec![],
        rms_mean: 0.0,
        rms_max: 0.0,
        loudness_lufs: 0.0,
        dynamic_range_db: 0.0,
        true_peak_db: None,
        replaygain_db: None,
        loudness_curve: None,
        loudness_momentary_max_db: None,
        loudness_range_lu: None,
        spectral_centroid_mean: 0.0,
        zero_crossing_rate: 0.0,
        onset_density: 0.0,
        spectral_bandwidth_mean: None,
        spectral_rolloff_mean: None,
        spectral_flatness_mean: None,
        spectral_contrast_mean: None,
        mfcc_mean: None,
        chroma_mean: None,
        tempo_curve: None,
        tempo_variability: None,
        time_signature: None,
        time_signature_confidence: None,
        chord_sequence: None,
        chord_events: None,
        chord_change_rate: None,
        predominant_chord: None,
        dissonance: None,
        energy: None,
        danceability: None,
        key: None,
        key_confidence: None,
        key_camelot: None,
        valence: None,
        acousticness: None,
        embedding: None,
        aggression_score: None,
        aggression_confidence: None,
        aggression_forcefulness: None,
        aggression_harshness: None,
        aggression_tension: None,
        aggression_rhythm: None,
        mood_happy: None,
        mood_aggressive: None,
        mood_relaxed: None,
        mood_sad: None,
        instrumentalness: None,
        genre: None,
        genre_confidence: None,
        grid_offset_sec: None,
        downbeats: None,
        grid_stability: None,
        energy_curve: None,
        energy_curve_hop_sec: None,
        segments: None,
        intro_end_sec: None,
        outro_start_sec: None,
        energy_level: None,
        leading_silence_sec: None,
        trailing_silence_sec: None,
        key_candidates: None,
        vocalness: None,
        fingerprint: None,
        embedding_version: None,
    }
}
