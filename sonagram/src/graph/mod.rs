//! Graph mapping v1: deterministic projection of analysis records into a
//! `kglite` `DirGraph`, following `dev-docs/designs/graph-schema.md`.
//!
//! sonagram owns exactly this mapping. Given a set of [`AnalysisRecord`]s (the
//! frozen fixtures or a live scan's cache) it builds one `Track` node per audio
//! content hash, a small set of low-cardinality dimension nodes agents group and
//! traverse by, the edges between them, and a pre-weighted similarity embedding
//! store — then persists the whole thing as a single `.kgl` file.
//!
//! ## Determinism
//! The same records must produce a byte-identical graph. To that end:
//! - records are sorted by `content_hash` before anything is built;
//! - every dimension collection is a `BTreeMap`/`BTreeSet`, never a `HashMap`,
//!   so node insertion order is fixed;
//! - no wall-clock property is written in v1 (`built_at` is deferred);
//! - the embedding store's slot order follows the sorted record order.
//!
//! ## Stage order (fixed — nodes before edges, no endpoint vivification)
//! dimension nodes (`Artist`, `Album`, `Genre`, static `Key`×24 / `TempoBand`×7
//! / `EnergyLevel`×10, `Decade`) → `Track` nodes (one full-width `add_nodes`
//! pass) → edges → embedding store → `SIMILAR_TO` → `CAMELOT_ADJACENT` →
//! `Style` → `Library` root **last** (it carries the adaptive `style_threshold`
//! the Style pass chooses, and has no edges so its position is free).

mod derive;
pub mod normalize;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use kglite::api::mutation::{add_edges_from_specs, add_nodes, EdgeSpec};
use kglite::api::storage::EmbeddingStore;
use kglite::api::{DirGraph, Value};
use kglite::datatypes::values::{ColumnData, ColumnType, DataFrame};
use sonara::similarity::{EMBEDDING_DIM, SIMILARITY_VERSION, WEIGHTS};

use crate::record::AnalysisRecord;
use crate::{Result, SonagramError};

use normalize::{
    album_id, album_name, artist_id, decade_id, filename_from_path, genre_id, tempo_band, KEYS,
    TEMPO_BANDS,
};

/// Version of *this* graph schema (node/edge/property layout). Distinct from the
/// analysis schema version (which lives on each `Track`). Bump when the mapping
/// changes shape.
pub const GRAPH_SCHEMA_VERSION: u32 = 1;

/// The embedding-store model identity, **derived** from sonara's
/// [`SIMILARITY_VERSION`] (format `"sonara-similarity-v{N}"`) rather than
/// hardcoded. It stamps every similarity store's `model_id`, so a stored vector
/// is never silently reinterpreted under a different similarity version: bump
/// `SIMILARITY_VERSION` upstream and this id — and thus the golden digest — moves
/// with it automatically. This is the whole point of the upstream contract.
pub fn embedding_model_id() -> String {
    format!("sonara-similarity-v{SIMILARITY_VERSION}")
}

/// The `(node_type, property)` key under which the similarity embedding store is
/// registered, and its distance metric.
pub const EMBEDDING_PROPERTY: &str = "similarity";
/// Distance metric for the similarity store. Euclidean over pre-weighted
/// vectors reproduces sonara's weighted-L2 ranking (see [`preweight`]).
pub const EMBEDDING_METRIC: &str = "euclidean";

// Node-type names (interned into the graph schema).
const LIBRARY: &str = "Library";
const TRACK: &str = "Track";
const ARTIST: &str = "Artist";
const ALBUM: &str = "Album";
const GENRE: &str = "Genre";
const KEY: &str = "Key";
const TEMPO_BAND_TYPE: &str = "TempoBand";
const ENERGY_LEVEL: &str = "EnergyLevel";
const DECADE: &str = "Decade";
const STYLE: &str = "Style";

// Edge-type names. None contains the reserved Cypher substring "CONTAINS".
const BY_ARTIST: &str = "BY_ARTIST";
const ON_ALBUM: &str = "ON_ALBUM";
const IN_GENRE: &str = "IN_GENRE";
const IN_KEY: &str = "IN_KEY";
const IN_TEMPO_BAND: &str = "IN_TEMPO_BAND";
const AT_ENERGY: &str = "AT_ENERGY";
const FROM_DECADE: &str = "FROM_DECADE";
// Phase 6 derived edges (built after the embedding store — see `derive`).
const SIMILAR_TO: &str = "SIMILAR_TO";
const CAMELOT_ADJACENT: &str = "CAMELOT_ADJACENT";
const IN_STYLE: &str = "IN_STYLE";

/// Minimal library-root metadata for the `Library` root node.
#[derive(Debug, Clone)]
pub struct LibraryInfo {
    /// The library root (a display string; a file name or label, never a user
    /// directory tree — the scanner keeps paths relative).
    pub root: String,
    /// Number of tracks in the library. Stamped as a `Library` property.
    pub n_tracks: usize,
}

/// Build a deterministic `DirGraph` from `records` per the music schema.
///
/// `records` need not be pre-sorted — they are sorted by `content_hash` here so
/// the output is identical regardless of input order. Returns an error if any
/// edge references a node that was not built (which would be a mapping bug —
/// `add_edges_from_specs` never vivifies endpoints).
pub fn build_graph(records: &[AnalysisRecord], library: &LibraryInfo) -> Result<Arc<DirGraph>> {
    let mut graph = DirGraph::new();

    // Deterministic input order: sort references by content hash.
    let mut sorted: Vec<&AnalysisRecord> = records.iter().collect();
    sorted.sort_by(|a, b| a.source.content_hash.cmp(&b.source.content_hash));

    // ── Dimension collections (BTree* → fixed iteration order) ──────────────
    // Artist id → track count.
    let mut artists: BTreeMap<String, i64> = BTreeMap::new();
    // Album id → (display name, artist id, year).
    let mut albums: BTreeMap<String, (String, String, Option<i64>)> = BTreeMap::new();
    let mut genres: BTreeSet<String> = BTreeSet::new();
    let mut decades: BTreeSet<String> = BTreeSet::new();

    for r in &sorted {
        let t = r.tags.as_ref();
        let art = artist_id(t.and_then(|t| t.artist.as_deref()));
        *artists.entry(art.clone()).or_insert(0) += 1;

        if let Some(aid) = album_id(&art, t.and_then(|t| t.album.as_deref())) {
            let name = album_name(t.and_then(|t| t.album.as_deref())).unwrap_or_default();
            let year = t.and_then(|t| t.year).map(|y| y as i64);
            albums.entry(aid).or_insert((name, art.clone(), year));
        }
        if let Some(g) = genre_id(t.and_then(|t| t.genre.as_deref())) {
            genres.insert(g);
        }
        if let Some(y) = t.and_then(|t| t.year) {
            decades.insert(decade_id(y));
        }
    }

    // ── Stage 1: dimension nodes ────────────────────────────────────────────
    // (The `Library` root is built LAST — Stage 9 — so it can carry the adaptive
    // `style_threshold` the Style pass chooses. It has no edges, so its build
    // order does not affect any endpoint.)
    add_artists(&mut graph, &artists)?;
    add_albums(&mut graph, &albums)?;
    add_genres(&mut graph, &genres)?;
    add_keys(&mut graph)?;
    add_tempo_bands(&mut graph)?;
    add_energy_levels(&mut graph)?;
    add_decades(&mut graph, &decades)?;

    // ── Stage 3: Track nodes (single full-width pass) ───────────────────────
    add_tracks(&mut graph, &sorted)?;

    // ── Stage 4: edges (all endpoints now exist) ────────────────────────────
    let specs = build_edges(&sorted, &albums);
    let report = add_edges_from_specs(&mut graph, specs).map_err(SonagramError::Graph)?;
    if report.skipped_missing_endpoint != 0 {
        return Err(SonagramError::Graph(format!(
            "{} edge(s) referenced a missing endpoint — a mapping bug",
            report.skipped_missing_endpoint
        )));
    }

    // ── Stage 5: pre-weighted similarity embedding store ────────────────────
    let mut store = EmbeddingStore::with_metric(EMBEDDING_DIM, EMBEDDING_METRIC);
    store.model_id = Some(embedding_model_id());
    for r in &sorted {
        if let Some(emb) = &r.analysis.embedding {
            if emb.len() != EMBEDDING_DIM {
                continue;
            }
            let hash = Value::String(r.source.content_hash.clone());
            if let Some(ni) = graph.lookup_by_id_readonly(TRACK, &hash) {
                store.set_embedding(ni.index(), &preweight(emb));
            }
        }
    }
    graph
        .embeddings
        .insert((TRACK.to_string(), EMBEDDING_PROPERTY.to_string()), store);

    // ── Stage 6: SIMILAR_TO (reads the store) ───────────────────────────────
    // Materialize the top-k nearest-neighbour graph; keep the scored fan-out so
    // the style detector reuses it instead of recomputing.
    let sim_edges = derive::add_similar_to(&mut graph, &sorted)?;

    // ── Stage 7: CAMELOT_ADJACENT (static wheel between the 24 Key nodes) ────
    derive::add_camelot_adjacent(&mut graph)?;

    // ── Stage 8: Style community nodes + IN_STYLE edges ─────────────────────
    // The Style pass chooses a deterministic adaptive threshold from this
    // build's own mutual-kNN score distribution (P10c) and returns it to stamp.
    let (_n_styles, style_threshold) = derive::add_styles(&mut graph, &sorted, &sim_edges)?;

    // ── Stage 9: Library root (last — carries the chosen `style_threshold`) ──
    let lib_df = build_df(vec![
        ("id", ColumnType::String, str1(&library.root)),
        ("path", ColumnType::String, str1(&library.root)),
        ("n_tracks", ColumnType::Int64, int1(library.n_tracks as i64)),
        (
            "schema_version",
            ColumnType::Int64,
            int1(GRAPH_SCHEMA_VERSION as i64),
        ),
        (
            "style_threshold",
            ColumnType::Float64,
            ColumnData::Float64(vec![Some(style_threshold)]),
        ),
    ]);
    add(&mut graph, lib_df, LIBRARY, "id", "path")?;

    Ok(Arc::new(graph))
}

/// Persist a built graph to `path` as a `.kgl` file.
///
/// Delegates to `kglite::api::io::save_graph`, which runs the mandatory
/// `prepare_save` + `enable_columnar` before writing (skipping them drops all
/// properties on reload).
pub fn save(graph: &mut Arc<DirGraph>, path: &Path) -> Result<()> {
    let p = path
        .to_str()
        .ok_or_else(|| SonagramError::Graph(format!("non-UTF-8 path: {}", path.display())))?;
    kglite::api::io::save_graph(graph, p).map_err(SonagramError::Graph)
}

/// Pre-weight a raw 48-dim similarity embedding so kglite's Euclidean metric
/// reproduces sonara's weighted-L2 `distance` ranking exactly.
///
/// sonara's `distance` is `sqrt( Σ wᵢ·dᵢ² / Σwᵢ )` with non-finite components
/// zeroed. Storing `eᵢ' = finite(eᵢ)·√wᵢ` makes the plain squared Euclidean
/// distance `Σ (a'ᵢ − b'ᵢ)² = Σ wᵢ·dᵢ²`, which differs from `distance²` only by
/// the constant factor `1/Σwᵢ` and a monotone `sqrt`/clamp — so nearest-neighbor
/// **rankings are identical**.
pub fn preweight(embedding: &[f32]) -> Vec<f32> {
    embedding
        .iter()
        .zip(WEIGHTS.iter())
        .map(|(&e, &w)| finite_or_zero(e) * w.sqrt())
        .collect()
}

#[inline]
fn finite_or_zero(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

// ─────────────────────────── Dimension node builders ────────────────────────

fn add_artists(graph: &mut DirGraph, artists: &BTreeMap<String, i64>) -> Result<()> {
    if artists.is_empty() {
        return Ok(());
    }
    let ids: Vec<Option<String>> = artists.keys().map(|k| Some(k.clone())).collect();
    let names = ids.clone();
    let counts: Vec<Option<i64>> = artists.values().map(|c| Some(*c)).collect();
    let df = build_df(vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
        ("n_tracks", ColumnType::Int64, ColumnData::Int64(counts)),
    ]);
    add(graph, df, ARTIST, "id", "name")
}

fn add_albums(
    graph: &mut DirGraph,
    albums: &BTreeMap<String, (String, String, Option<i64>)>,
) -> Result<()> {
    if albums.is_empty() {
        return Ok(());
    }
    let ids: Vec<Option<String>> = albums.keys().map(|k| Some(k.clone())).collect();
    let names: Vec<Option<String>> = albums.values().map(|(n, _, _)| Some(n.clone())).collect();
    let artist: Vec<Option<String>> = albums.values().map(|(_, a, _)| Some(a.clone())).collect();
    let years: Vec<Option<i64>> = albums.values().map(|(_, _, y)| *y).collect();
    let df = build_df(vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
        ("artist", ColumnType::String, ColumnData::String(artist)),
        ("year", ColumnType::Int64, ColumnData::Int64(years)),
    ]);
    add(graph, df, ALBUM, "id", "name")
}

fn add_genres(graph: &mut DirGraph, genres: &BTreeSet<String>) -> Result<()> {
    if genres.is_empty() {
        return Ok(());
    }
    let ids: Vec<Option<String>> = genres.iter().map(|g| Some(g.clone())).collect();
    let names = ids.clone();
    let df = build_df(vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
    ]);
    add(graph, df, GENRE, "id", "name")
}

fn add_keys(graph: &mut DirGraph) -> Result<()> {
    let ids: Vec<Option<String>> = KEYS.iter().map(|k| Some(k.name.to_string())).collect();
    let names = ids.clone();
    let camelot: Vec<Option<String>> = KEYS.iter().map(|k| Some(k.camelot.to_string())).collect();
    let mode: Vec<Option<String>> = KEYS.iter().map(|k| Some(k.mode.to_string())).collect();
    let df = build_df(vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
        ("camelot", ColumnType::String, ColumnData::String(camelot)),
        ("mode", ColumnType::String, ColumnData::String(mode)),
    ]);
    add(graph, df, KEY, "id", "name")
}

fn add_tempo_bands(graph: &mut DirGraph) -> Result<()> {
    let ids: Vec<Option<String>> = TEMPO_BANDS.iter().map(|(n, _, _)| Some(n.to_string())).collect();
    let names = ids.clone();
    let df = build_df(vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
    ]);
    add(graph, df, TEMPO_BAND_TYPE, "id", "name")
}

fn add_energy_levels(graph: &mut DirGraph) -> Result<()> {
    let ids: Vec<Option<String>> = (1..=10).map(|n| Some(n.to_string())).collect();
    let names = ids.clone();
    let levels: Vec<Option<i64>> = (1..=10).map(Some).collect();
    let df = build_df(vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
        ("level", ColumnType::Int64, ColumnData::Int64(levels)),
    ]);
    add(graph, df, ENERGY_LEVEL, "id", "name")
}

fn add_decades(graph: &mut DirGraph, decades: &BTreeSet<String>) -> Result<()> {
    if decades.is_empty() {
        return Ok(());
    }
    let ids: Vec<Option<String>> = decades.iter().map(|d| Some(d.clone())).collect();
    let names = ids.clone();
    let df = build_df(vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
    ]);
    add(graph, df, DECADE, "id", "name")
}

// ─────────────────────────────── Track builder ──────────────────────────────

/// Build the single full-width `Track` DataFrame. Every column is present for
/// every row (missing optionals → null cells) so this is one `add_nodes` pass —
/// a partial DataFrame would rebuild nodes from only its columns, dropping the
/// rest.
fn add_tracks(graph: &mut DirGraph, sorted: &[&AnalysisRecord]) -> Result<()> {
    if sorted.is_empty() {
        return Ok(());
    }

    // Helpers to project each record.
    let tag_str = |r: &AnalysisRecord, f: fn(&crate::record::TagsDto) -> Option<&str>| -> Option<String> {
        r.tags
            .as_ref()
            .and_then(|t| f(t))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };

    let unique_id: Vec<Option<String>> = sorted
        .iter()
        .map(|r| Some(r.source.content_hash.clone()))
        .collect();
    let title: Vec<Option<String>> = sorted
        .iter()
        .map(|r| {
            Some(
                tag_str(r, |t| t.title.as_deref())
                    .unwrap_or_else(|| filename_from_path(&r.source.path)),
            )
        })
        .collect();
    let path: Vec<Option<String>> = sorted.iter().map(|r| Some(r.source.path.clone())).collect();
    let filename: Vec<Option<String>> = sorted
        .iter()
        .map(|r| Some(filename_from_path(&r.source.path)))
        .collect();
    let artist_name: Vec<Option<String>> = sorted
        .iter()
        .map(|r| Some(artist_id(r.tags.as_ref().and_then(|t| t.artist.as_deref()))))
        .collect();
    let album_name_col: Vec<Option<String>> = sorted
        .iter()
        .map(|r| album_name(r.tags.as_ref().and_then(|t| t.album.as_deref())))
        .collect();
    let genre_tag: Vec<Option<String>> =
        sorted.iter().map(|r| tag_str(r, |t| t.genre.as_deref())).collect();
    let format: Vec<Option<String>> =
        sorted.iter().map(|r| Some(r.source.format.clone())).collect();

    // Int columns.
    let year = int_opt_col(sorted, |r| r.tags.as_ref().and_then(|t| t.year).map(|y| y as i64));
    let track_no = int_opt_col(sorted, |r| {
        r.tags.as_ref().and_then(|t| t.track_no).map(|n| n as i64)
    });
    let file_size = int_opt_col(sorted, |r| Some(r.source.file_size as i64));
    let energy_level = int_opt_col(sorted, |r| r.analysis.energy_level.map(|e| e as i64));
    let n_segments = int_opt_col(sorted, |r| {
        r.analysis.segments.as_ref().map(|s| s.len() as i64)
    });
    let analysis_schema_version =
        int_opt_col(sorted, |r| Some(r.analysis.provenance.schema_version as i64));
    let embedding_version =
        int_opt_col(sorted, |r| r.analysis.embedding_version.map(|v| v as i64));

    // Float columns — always-present (non-Option in the DTO) then optional.
    let duration_sec = f_col(sorted, |r| r.analysis.duration_sec);
    let bpm = f_col(sorted, |r| r.analysis.bpm);
    let bpm_raw = f_col(sorted, |r| r.analysis.bpm_raw);
    let loudness_lufs = f_col(sorted, |r| r.analysis.loudness_lufs);
    let dynamic_range_db = f_col(sorted, |r| r.analysis.dynamic_range_db);
    let spectral_centroid = f_col(sorted, |r| r.analysis.spectral_centroid_mean);
    let zero_crossing_rate = f_col(sorted, |r| r.analysis.zero_crossing_rate);
    let onset_density = f_col(sorted, |r| r.analysis.onset_density);

    let tempo_variability = fo_col(sorted, |r| r.analysis.tempo_variability);
    let grid_stability = fo_col(sorted, |r| r.analysis.grid_stability);
    let grid_offset_sec = fo_col(sorted, |r| r.analysis.grid_offset_sec);
    let energy = fo_col(sorted, |r| r.analysis.energy);
    let valence = fo_col(sorted, |r| r.analysis.valence);
    let danceability = fo_col(sorted, |r| r.analysis.danceability);
    let acousticness = fo_col(sorted, |r| r.analysis.acousticness);
    let vocalness = fo_col(sorted, |r| r.analysis.vocalness);
    let dissonance = fo_col(sorted, |r| r.analysis.dissonance);
    let mood_happy = fo_col(sorted, |r| r.analysis.mood_happy);
    let mood_aggressive = fo_col(sorted, |r| r.analysis.mood_aggressive);
    let mood_relaxed = fo_col(sorted, |r| r.analysis.mood_relaxed);
    let mood_sad = fo_col(sorted, |r| r.analysis.mood_sad);
    let instrumentalness = fo_col(sorted, |r| r.analysis.instrumentalness);
    let key_confidence = fo_col(sorted, |r| r.analysis.key_confidence);
    let chord_change_rate = fo_col(sorted, |r| r.analysis.chord_change_rate);
    let loudness_range_lu = fo_col(sorted, |r| r.analysis.loudness_range_lu);
    let true_peak_db = fo_col(sorted, |r| r.analysis.true_peak_db);
    let replaygain_db = fo_col(sorted, |r| r.analysis.replaygain_db);
    let intro_end_sec = fo_col(sorted, |r| r.analysis.intro_end_sec);
    let outro_start_sec = fo_col(sorted, |r| r.analysis.outro_start_sec);
    let leading_silence_sec = fo_col(sorted, |r| r.analysis.leading_silence_sec);
    let trailing_silence_sec = fo_col(sorted, |r| r.analysis.trailing_silence_sec);
    let spectral_flatness = fo_col(sorted, |r| r.analysis.spectral_flatness_mean);

    // String (optional) tonal/rhythm columns.
    let time_signature = str_opt_col(sorted, |r| r.analysis.time_signature.clone());
    let key = str_opt_col(sorted, |r| r.analysis.key.clone());
    let camelot = str_opt_col(sorted, |r| r.analysis.key_camelot.clone());
    let predominant_chord = str_opt_col(sorted, |r| r.analysis.predominant_chord.clone());

    let df = build_df(vec![
        ("content_hash", ColumnType::String, ColumnData::String(unique_id)),
        ("title", ColumnType::String, ColumnData::String(title)),
        ("path", ColumnType::String, ColumnData::String(path)),
        ("filename", ColumnType::String, ColumnData::String(filename)),
        ("artist_name", ColumnType::String, ColumnData::String(artist_name)),
        ("album_name", ColumnType::String, ColumnData::String(album_name_col)),
        ("genre_tag", ColumnType::String, ColumnData::String(genre_tag)),
        ("format", ColumnType::String, ColumnData::String(format)),
        ("year", ColumnType::Int64, ColumnData::Int64(year)),
        ("track_no", ColumnType::Int64, ColumnData::Int64(track_no)),
        ("file_size", ColumnType::Int64, ColumnData::Int64(file_size)),
        ("energy_level", ColumnType::Int64, ColumnData::Int64(energy_level)),
        ("n_segments", ColumnType::Int64, ColumnData::Int64(n_segments)),
        (
            "analysis_schema_version",
            ColumnType::Int64,
            ColumnData::Int64(analysis_schema_version),
        ),
        ("embedding_version", ColumnType::Int64, ColumnData::Int64(embedding_version)),
        ("duration_sec", ColumnType::Float64, ColumnData::Float64(duration_sec)),
        ("bpm", ColumnType::Float64, ColumnData::Float64(bpm)),
        ("bpm_raw", ColumnType::Float64, ColumnData::Float64(bpm_raw)),
        ("loudness_lufs", ColumnType::Float64, ColumnData::Float64(loudness_lufs)),
        ("dynamic_range_db", ColumnType::Float64, ColumnData::Float64(dynamic_range_db)),
        ("spectral_centroid", ColumnType::Float64, ColumnData::Float64(spectral_centroid)),
        ("zero_crossing_rate", ColumnType::Float64, ColumnData::Float64(zero_crossing_rate)),
        ("onset_density", ColumnType::Float64, ColumnData::Float64(onset_density)),
        ("tempo_variability", ColumnType::Float64, ColumnData::Float64(tempo_variability)),
        ("grid_stability", ColumnType::Float64, ColumnData::Float64(grid_stability)),
        ("grid_offset_sec", ColumnType::Float64, ColumnData::Float64(grid_offset_sec)),
        ("energy", ColumnType::Float64, ColumnData::Float64(energy)),
        ("valence", ColumnType::Float64, ColumnData::Float64(valence)),
        ("danceability", ColumnType::Float64, ColumnData::Float64(danceability)),
        ("acousticness", ColumnType::Float64, ColumnData::Float64(acousticness)),
        ("vocalness", ColumnType::Float64, ColumnData::Float64(vocalness)),
        ("dissonance", ColumnType::Float64, ColumnData::Float64(dissonance)),
        ("mood_happy", ColumnType::Float64, ColumnData::Float64(mood_happy)),
        ("mood_aggressive", ColumnType::Float64, ColumnData::Float64(mood_aggressive)),
        ("mood_relaxed", ColumnType::Float64, ColumnData::Float64(mood_relaxed)),
        ("mood_sad", ColumnType::Float64, ColumnData::Float64(mood_sad)),
        ("instrumentalness", ColumnType::Float64, ColumnData::Float64(instrumentalness)),
        ("key_confidence", ColumnType::Float64, ColumnData::Float64(key_confidence)),
        ("chord_change_rate", ColumnType::Float64, ColumnData::Float64(chord_change_rate)),
        ("loudness_range_lu", ColumnType::Float64, ColumnData::Float64(loudness_range_lu)),
        ("true_peak_db", ColumnType::Float64, ColumnData::Float64(true_peak_db)),
        ("replaygain_db", ColumnType::Float64, ColumnData::Float64(replaygain_db)),
        ("intro_end_sec", ColumnType::Float64, ColumnData::Float64(intro_end_sec)),
        ("outro_start_sec", ColumnType::Float64, ColumnData::Float64(outro_start_sec)),
        ("leading_silence_sec", ColumnType::Float64, ColumnData::Float64(leading_silence_sec)),
        ("trailing_silence_sec", ColumnType::Float64, ColumnData::Float64(trailing_silence_sec)),
        ("spectral_flatness", ColumnType::Float64, ColumnData::Float64(spectral_flatness)),
        ("time_signature", ColumnType::String, ColumnData::String(time_signature)),
        ("key", ColumnType::String, ColumnData::String(key)),
        ("camelot", ColumnType::String, ColumnData::String(camelot)),
        ("predominant_chord", ColumnType::String, ColumnData::String(predominant_chord)),
    ]);
    add(graph, df, TRACK, "content_hash", "title")
}

// ─────────────────────────────── Edge builder ───────────────────────────────

fn build_edges(
    sorted: &[&AnalysisRecord],
    albums: &BTreeMap<String, (String, String, Option<i64>)>,
) -> Vec<EdgeSpec> {
    let mut specs: Vec<EdgeSpec> = Vec::new();
    for r in sorted {
        let hash = &r.source.content_hash;
        let t = r.tags.as_ref();
        let art = artist_id(t.and_then(|t| t.artist.as_deref()));

        specs.push(edge(TRACK, hash, ARTIST, &art, BY_ARTIST));

        if let Some(aid) = album_id(&art, t.and_then(|t| t.album.as_deref())) {
            specs.push(edge(TRACK, hash, ALBUM, &aid, ON_ALBUM));
        }
        if let Some(g) = genre_id(t.and_then(|t| t.genre.as_deref())) {
            specs.push(edge(TRACK, hash, GENRE, &g, IN_GENRE));
        }
        if let Some(k) = &r.analysis.key {
            specs.push(edge(TRACK, hash, KEY, k, IN_KEY));
        }
        specs.push(edge(TRACK, hash, TEMPO_BAND_TYPE, tempo_band(r.analysis.bpm), IN_TEMPO_BAND));
        if let Some(el) = r.analysis.energy_level {
            specs.push(edge(TRACK, hash, ENERGY_LEVEL, &el.to_string(), AT_ENERGY));
        }
        if let Some(y) = t.and_then(|t| t.year) {
            specs.push(edge(TRACK, hash, DECADE, &decade_id(y), FROM_DECADE));
        }
    }
    // Album → Artist (one per distinct album, sorted order).
    for (aid, (_, art, _)) in albums {
        specs.push(edge(ALBUM, aid, ARTIST, art, BY_ARTIST));
    }
    specs
}

fn edge(src_type: &str, src_id: &str, tgt_type: &str, tgt_id: &str, edge_type: &str) -> EdgeSpec {
    EdgeSpec {
        source_type: src_type.to_string(),
        source_id: Value::String(src_id.to_string()),
        target_type: tgt_type.to_string(),
        target_id: Value::String(tgt_id.to_string()),
        edge_type: edge_type.to_string(),
        properties: HashMap::new(),
    }
}

// ───────────────────────────── DataFrame helpers ────────────────────────────

fn build_df(cols: Vec<(&str, ColumnType, ColumnData)>) -> DataFrame {
    let mut df = DataFrame::new(Vec::new());
    for (name, ct, data) in cols {
        df.add_column(name.to_string(), ct, data)
            .unwrap_or_else(|e| panic!("add_column({name}) failed: {e}"));
    }
    df
}

fn add(
    graph: &mut DirGraph,
    df: DataFrame,
    node_type: &str,
    id_field: &str,
    title_field: &str,
) -> Result<()> {
    add_nodes(
        graph,
        df,
        node_type.to_string(),
        id_field.to_string(),
        Some(title_field.to_string()),
        None,
    )
    .map(|_| ())
    .map_err(SonagramError::Graph)
}

/// A single-cell string column.
fn str1(v: &str) -> ColumnData {
    ColumnData::String(vec![Some(v.to_string())])
}
/// A single-cell int column.
fn int1(v: i64) -> ColumnData {
    ColumnData::Int64(vec![Some(v)])
}

fn int_opt_col(
    sorted: &[&AnalysisRecord],
    f: impl Fn(&AnalysisRecord) -> Option<i64>,
) -> Vec<Option<i64>> {
    sorted.iter().map(|r| f(r)).collect()
}
fn str_opt_col(
    sorted: &[&AnalysisRecord],
    f: impl Fn(&AnalysisRecord) -> Option<String>,
) -> Vec<Option<String>> {
    sorted.iter().map(|r| f(r)).collect()
}
/// Always-present f32 → non-null f64 column.
fn f_col(sorted: &[&AnalysisRecord], f: impl Fn(&AnalysisRecord) -> f32) -> Vec<Option<f64>> {
    sorted.iter().map(|r| Some(f(r) as f64)).collect()
}
/// Optional f32 → nullable f64 column.
fn fo_col(
    sorted: &[&AnalysisRecord],
    f: impl Fn(&AnalysisRecord) -> Option<f32>,
) -> Vec<Option<f64>> {
    sorted.iter().map(|r| f(r).map(|v| v as f64)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_model_id_derives_from_similarity_version() {
        // The id must be computed from sonara's live SIMILARITY_VERSION, not a
        // hardcoded string — so an upstream bump moves it (and the golden) here.
        assert_eq!(
            embedding_model_id(),
            format!("sonara-similarity-v{SIMILARITY_VERSION}")
        );
        // Guards the exact wire format the golden pins.
        assert_eq!(embedding_model_id(), "sonara-similarity-v2");
    }

    #[test]
    fn preweight_matches_sonara_distance_ranking() {
        // Three distinct 48-dim vectors; the rank order of pairwise
        // pre-weighted Euclidean distances must equal the rank order of
        // sonara::similarity::distance on the raw vectors.
        let mut a = vec![0.10f32; EMBEDDING_DIM];
        let mut b = vec![0.10f32; EMBEDDING_DIM];
        let mut c = vec![0.10f32; EMBEDDING_DIM];
        // Perturb a few dimensions so the three pairwise distances differ.
        for i in 0..EMBEDDING_DIM {
            a[i] = 0.10 + (i as f32) * 0.001;
            b[i] = 0.10 + (i as f32) * 0.004; // b farther from a
            c[i] = 0.10 + (i as f32) * 0.0025; // c between
        }

        let eucl = |x: &[f32], y: &[f32]| -> f32 {
            let (px, py) = (preweight(x), preweight(y));
            px.iter()
                .zip(&py)
                .map(|(u, v)| (u - v) * (u - v))
                .sum::<f32>()
                .sqrt()
        };

        let pairs = [("ab", &a, &b), ("ac", &a, &c), ("bc", &b, &c)];
        let mut ours: Vec<(&str, f32)> =
            pairs.iter().map(|(n, x, y)| (*n, eucl(x, y))).collect();
        let mut sonaras: Vec<(&str, f32)> = pairs
            .iter()
            .map(|(n, x, y)| (*n, sonara::similarity::distance(x, y)))
            .collect();
        ours.sort_by(|p, q| p.1.partial_cmp(&q.1).unwrap());
        sonaras.sort_by(|p, q| p.1.partial_cmp(&q.1).unwrap());
        let our_order: Vec<&str> = ours.iter().map(|(n, _)| *n).collect();
        let sonara_order: Vec<&str> = sonaras.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            our_order, sonara_order,
            "pre-weighted Euclidean ranking must match sonara::similarity::distance"
        );
    }

    #[test]
    fn preweight_scales_by_sqrt_weight() {
        let raw = vec![1.0f32; EMBEDDING_DIM];
        let pw = preweight(&raw);
        for i in 0..EMBEDDING_DIM {
            assert!((pw[i] - WEIGHTS[i].sqrt()).abs() < 1e-6);
        }
    }

    #[test]
    fn preweight_zeros_non_finite() {
        let mut raw = vec![0.5f32; EMBEDDING_DIM];
        raw[0] = f32::NAN;
        raw[1] = f32::INFINITY;
        let pw = preweight(&raw);
        assert_eq!(pw[0], 0.0);
        assert_eq!(pw[1], 0.0);
    }
}
