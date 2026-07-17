//! P12 Last.fm enrichment ingestion gate.
//!
//! Builds the graph from the 15 frozen analysis fixtures **with** the frozen
//! Last.fm enrichment (`tests/fixtures/lastfm/*.json`) and asserts the exact
//! new shape — popularity/MBID/original-album props, folksonomy `IN_GENRE`
//! edges, and weighted/attributed `CROWD_SIMILAR` edges — then proves the
//! un-enriched build carries no enrichment values or edges while retaining the
//! always-present `has_lastfm_match = false` contract.
//!
//! Expected cardinalities were computed from the enrichment fixtures by hand
//! (4 artists / 4 tracks / 4 albums enriched) and cross-checked against the
//! captured enriched canonical golden.

use std::path::PathBuf;

use kglite::api::cypher::resolve_node_property;
use kglite::api::{DirGraph, Value};
use sonagram::enrich::EnrichmentData;
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
    paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).unwrap();
            AnalysisRecord::from_json(&text).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
        })
        .collect()
}

fn enrichment() -> EnrichmentData {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lastfm");
    EnrichmentData::load_from_dir(&dir).unwrap()
}

fn library() -> LibraryInfo {
    LibraryInfo {
        root: "fixtures".to_string(),
        n_tracks: 15,
    }
}

fn enriched() -> std::sync::Arc<DirGraph> {
    graph::build_graph_with_enrichment(&load_records(), Some(&enrichment()), &library()).unwrap()
}

// Content hashes of two of the four enriched tracks.
const ABBA_HASH: &str = "2221dee47329325ef2d35212fe370cf515e843d7257280ea25e30eca79d010ed";
const BRUNO_HASH: &str = "a204cc4055d23bf27a3659c2b28da5aad9c0769e6a6c587fa749ce5ad18b4419";

fn node_count(graph: &DirGraph, node_type: &str) -> usize {
    graph.type_indices.get(node_type).map(|r| r.len()).unwrap_or(0)
}

fn prop(graph: &DirGraph, node_type: &str, id: &str, prop: &str) -> Value {
    let ni = graph
        .lookup_by_id_readonly(node_type, &Value::String(id.to_string()))
        .unwrap_or_else(|| panic!("no {node_type} node with id {id}"));
    let node = graph.get_node(ni).unwrap();
    resolve_node_property(node, prop, graph)
}

/// CROWD_SIMILAR edge stats: `(track_track, track_track_with_score,
/// artist_artist, artist_artist_with_source)` — computed over the stable
/// digraph edge view, the same accessor the golden gate reads.
fn crowd_similar_stats(g: &DirGraph) -> (usize, usize, usize, usize) {
    let sg = g.graph.as_stable_digraph();
    let (mut tt, mut tt_score, mut aa, mut aa_src) = (0, 0, 0, 0);
    for e in sg.edge_indices() {
        let edge = sg.edge_weight(e).unwrap();
        if edge.connection_type_str(&g.interner) != "CROWD_SIMILAR" {
            continue;
        }
        let (si, ti) = sg.edge_endpoints(e).unwrap();
        let sn = sg.node_weight(si).unwrap();
        let tn = sg.node_weight(ti).unwrap();
        let s_type = sn.node_type_str(&g.interner);
        let t_type = tn.node_type_str(&g.interner);
        let props = edge.properties_cloned(&g.interner);
        if s_type == "Track" && t_type == "Track" {
            tt += 1;
            if matches!(props.get("score"), Some(Value::Float64(_))) {
                tt_score += 1;
            }
        } else if s_type == "Artist" && t_type == "Artist" {
            aa += 1;
            if matches!(props.get("source"), Some(Value::String(s)) if s == "lastfm") {
                aa_src += 1;
            }
        }
    }
    (tt, tt_score, aa, aa_src)
}

// ─────────────────────────── enriched: node props ───────────────────────────

#[test]
fn track_gains_lastfm_and_original_album_props() {
    let g = enriched();
    // ABBA "On and on and on" — full enrichment.
    assert_eq!(prop(&g, "Track", ABBA_HASH, "lastfm_playcount"), Value::Int64(1_800_000));
    assert_eq!(prop(&g, "Track", ABBA_HASH, "lastfm_listeners"), Value::Int64(320_000));
    assert_eq!(prop(&g, "Track", ABBA_HASH, "has_lastfm_match"), Value::Boolean(true));
    match prop(&g, "Track", ABBA_HASH, "popularity") {
        Value::Float64(v) => assert!((v - 1.0 / 3.0).abs() < 1e-12, "ABBA percentile: {v}"),
        other => panic!("popularity is not Float64: {other:?}"),
    }
    assert_eq!(
        prop(&g, "Track", ABBA_HASH, "mbid"),
        Value::String("1a1b1c1d-0000-4000-8000-000000000001".to_string())
    );
    assert_eq!(
        prop(&g, "Track", ABBA_HASH, "original_album"),
        Value::String("Super Trouper".to_string())
    );
    assert_eq!(prop(&g, "Track", ABBA_HASH, "original_album_position"), Value::Int64(7));

    // Bee Gees "Jive Talkin'": original album (Main Course) differs from the file
    // album (Number Ones, a compilation) — the back-mapping the enrichment fixes.
    let jive = "c1f2b8a8c25f751431a53f8d856b0914e3de2a236011635f2c0292489d1549c1";
    assert_eq!(
        prop(&g, "Track", jive, "original_album"),
        Value::String("Main Course".to_string())
    );
}

#[test]
fn non_enriched_track_has_no_lastfm_props() {
    let g = enriched();
    // Resolve the Estranged (Guns N' Roses) track — NOT in the enrichment set.
    let hash = load_records()
        .into_iter()
        .find(|r| {
            r.tags
                .as_ref()
                .and_then(|t| t.title.as_deref())
                .map(|t| t.eq_ignore_ascii_case("estranged"))
                .unwrap_or(false)
        })
        .map(|r| r.source.content_hash)
        .expect("estranged fixture present");

    // A null-cell property does not materialize — the un-enriched track carries
    // no lastfm_* / original_* / mbid property at all.
    assert_eq!(prop(&g, "Track", &hash, "lastfm_playcount"), Value::Null);
    assert_eq!(prop(&g, "Track", &hash, "lastfm_listeners"), Value::Null);
    assert_eq!(prop(&g, "Track", &hash, "has_lastfm_match"), Value::Boolean(false));
    assert_eq!(prop(&g, "Track", &hash, "popularity"), Value::Null);
    assert_eq!(prop(&g, "Track", &hash, "original_album"), Value::Null);
    assert_eq!(prop(&g, "Track", &hash, "mbid"), Value::Null);
}

#[test]
fn matched_track_without_listener_count_has_null_popularity() {
    let mut enrichment = enrichment();
    enrichment.tracks.get_mut(ABBA_HASH).expect("ABBA fixture").listeners = None;
    let g = graph::build_graph_with_enrichment(&load_records(), Some(&enrichment), &library())
        .unwrap();
    assert_eq!(prop(&g, "Track", ABBA_HASH, "has_lastfm_match"), Value::Boolean(true));
    assert_eq!(prop(&g, "Track", ABBA_HASH, "lastfm_listeners"), Value::Null);
    assert_eq!(prop(&g, "Track", ABBA_HASH, "popularity"), Value::Null);
}

#[test]
fn popularity_is_stable_under_input_reordering() {
    let records = load_records();
    let mut reversed = records.clone();
    reversed.reverse();
    let enrichment = enrichment();
    let base = graph::build_graph_with_enrichment(&records, Some(&enrichment), &library()).unwrap();
    let other = graph::build_graph_with_enrichment(&reversed, Some(&enrichment), &library()).unwrap();
    for hash in [ABBA_HASH, BRUNO_HASH] {
        assert_eq!(
            prop(&base, "Track", hash, "popularity"),
            prop(&other, "Track", hash, "popularity")
        );
    }
}

#[test]
fn equal_listener_counts_receive_equal_popularity() {
    let mut enrichment = enrichment();
    let listeners = enrichment
        .tracks
        .get(ABBA_HASH)
        .and_then(|record| record.listeners)
        .expect("ABBA listener fixture");
    enrichment
        .tracks
        .get_mut(BRUNO_HASH)
        .expect("Bruno fixture")
        .listeners = Some(listeners);
    let g = graph::build_graph_with_enrichment(&load_records(), Some(&enrichment), &library())
        .unwrap();
    assert_eq!(
        prop(&g, "Track", ABBA_HASH, "popularity"),
        prop(&g, "Track", BRUNO_HASH, "popularity")
    );
}

#[test]
fn artist_and_album_gain_lastfm_props() {
    let g = enriched();
    // Artist ABBA: playcount / listeners / mbid, but NO lastfm_url (Artist schema
    // is playcount/listeners/mbid only).
    assert_eq!(prop(&g, "Artist", "ABBA", "lastfm_playcount"), Value::Int64(155_000_000));
    assert_eq!(prop(&g, "Artist", "ABBA", "lastfm_listeners"), Value::Int64(3_200_000));
    assert_eq!(
        prop(&g, "Artist", "ABBA", "mbid"),
        Value::String("d87e52c5-bb8d-4da8-b941-9f4928627dc8".to_string())
    );

    // Album ABBA|Super Trouper: playcount + wiki summary.
    let alb = "ABBA|Super Trouper";
    assert_eq!(prop(&g, "Album", alb, "lastfm_playcount"), Value::Int64(2_600_000));
    match prop(&g, "Album", alb, "wiki_summary") {
        Value::String(s) => assert!(s.starts_with("Super Trouper is the seventh")),
        other => panic!("wiki_summary not a String: {other:?}"),
    }
}

// ─────────────────────────── enriched: folksonomy ───────────────────────────

#[test]
fn folksonomy_extends_genre_dimension_and_in_genre_edges() {
    let g = enriched();
    // 10 file genres + 6 folksonomy genres (swedish pop, disco, classic, funk,
    // wedding, synthpop) = 16.
    assert_eq!(node_count(&g, "Genre"), 16, "Genre nodes (file + folksonomy)");

    // The six new folksonomy Genre nodes exist (normalized/lowercased).
    for g_id in ["swedish pop", "disco", "classic", "funk", "wedding", "synthpop"] {
        assert!(
            g.lookup_by_id_readonly("Genre", &Value::String(g_id.to_string()))
                .is_some(),
            "folksonomy Genre `{g_id}` present"
        );
    }

    // Base file-genre IN_GENRE = 14; folksonomy adds 9 (7 Artist→Genre +
    // 2 Track→Genre, the "pop" duplicates deduped against the file genre) = 23.
    let counts = g.get_edge_type_counts();
    assert_eq!(counts.get("IN_GENRE").copied().unwrap_or(0), 23, "IN_GENRE total");
}

// ─────────────────────────── enriched: CROWD_SIMILAR ─────────────────────────

#[test]
fn crowd_similar_edges_weighted_and_attributed_and_dropped() {
    let g = enriched();
    let counts = g.get_edge_type_counts();
    assert_eq!(counts.get("CROWD_SIMILAR").copied().unwrap_or(0), 8, "CROWD_SIMILAR total");

    let (tt, tt_score, aa, aa_src) = crowd_similar_stats(&g);
    // 4 owned Track→Track pairs survive; every one carries a `score` weight.
    // (ABBA→{Bee Gees, Bruno Mars}, Bee Gees→ABBA, Bruno Mars→A-ha. The ABBA→
    // "Fake Song" and A-ha→"Unowned Song" similars resolve to non-owned tracks
    // and are DROPPED — proving the owned-endpoint gate.)
    assert_eq!(tt, 4, "Track→Track CROWD_SIMILAR (non-owned similars dropped)");
    assert_eq!(tt_score, 4, "every Track→Track CROWD_SIMILAR carries a score weight");
    // 4 owned Artist→Artist edges; every one carries source="lastfm". (ABBA→
    // {Bee Gees, A-ha}, Bee Gees→ABBA, A-ha→ABBA. Bruno Mars' only similar is
    // non-owned → dropped, and ABBA's "Some Unowned Artist" is dropped.)
    assert_eq!(aa, 4, "Artist→Artist CROWD_SIMILAR (non-owned similars dropped)");
    assert_eq!(aa_src, 4, "every Artist→Artist CROWD_SIMILAR carries source=lastfm");

    // The preserved match weight lands on the edge (the prototype's dropped-weight
    // bug is not re-imported): ABBA→Bruno Mars carries 0.80.
    let sg = g.graph.as_stable_digraph();
    let mut found = false;
    for e in sg.edge_indices() {
        let edge = sg.edge_weight(e).unwrap();
        if edge.connection_type_str(&g.interner) != "CROWD_SIMILAR" {
            continue;
        }
        let (si, ti) = sg.edge_endpoints(e).unwrap();
        let s = sg.node_weight(si).unwrap().id().into_owned();
        let t = sg.node_weight(ti).unwrap().id().into_owned();
        if s == Value::String(ABBA_HASH.to_string())
            && t == Value::String(BRUNO_HASH.to_string())
        {
            match edge.properties_cloned(&g.interner).get("score") {
                Some(Value::Float64(w)) => {
                    assert!((w - 0.8).abs() < 1e-6, "score weight preserved (0.80), got {w}");
                    found = true;
                }
                other => panic!("ABBA→Bruno CROWD_SIMILAR score missing/typed wrong: {other:?}"),
            }
        }
    }
    assert!(found, "ABBA→Bruno Mars CROWD_SIMILAR edge present");
}

// ─────────────────── plain build is unchanged by the feature ─────────────────

#[test]
fn plain_build_has_no_enrichment_values_or_edges() {
    let plain = graph::build_graph(&load_records(), &library()).unwrap();
    // No enrichment node/edge shapes leak into the plain build.
    assert_eq!(node_count(&plain, "Genre"), 10, "plain Genre count unchanged");
    let counts = plain.get_edge_type_counts();
    assert_eq!(counts.get("IN_GENRE").copied().unwrap_or(0), 14, "plain IN_GENRE unchanged");
    assert_eq!(counts.get("CROWD_SIMILAR").copied().unwrap_or(0), 0, "no CROWD_SIMILAR in plain");
    // Counts/rank stay null, while the recognition flag is always false.
    assert_eq!(prop(&plain, "Track", ABBA_HASH, "lastfm_playcount"), Value::Null);
    assert_eq!(prop(&plain, "Track", ABBA_HASH, "lastfm_listeners"), Value::Null);
    assert_eq!(prop(&plain, "Track", ABBA_HASH, "has_lastfm_match"), Value::Boolean(false));
    assert_eq!(prop(&plain, "Track", ABBA_HASH, "popularity"), Value::Null);
    assert_eq!(prop(&plain, "Track", ABBA_HASH, "original_album"), Value::Null);
}
