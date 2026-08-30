//! Graph mapping v1: deterministic projection of analysis records into a
//! `kglite` `DirGraph`. This module *is* the schema; `GRAPH-GATE.md` and the
//! committed goldens pin what it produces.
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
//! pass, carrying the primary P21 Stage-C `is_canonical` flag) → edges →
//! embedding store → `SIMILAR_TO` → audio-confirmed `Song` version nodes +
//! `VERSION_OF` edges and canonical-flag update → `CAMELOT_ADJACENT` →
//! `Style` → `Library` root **last** (it carries the
//! adaptive `style_threshold` the Style pass chooses, and has no edges so its
//! position is free).

mod derive;
mod features;
pub mod normalize;
mod song;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use kglite::api::mutation::{add_edges_from_specs, add_nodes, EdgeSpec};
use kglite::api::mutation::{ColumnData, ColumnType, DataFrame};
use kglite::api::storage::EmbeddingStore;
use kglite::api::{DirGraph, Value};
use sonara::similarity::{EMBEDDING_DIM, SIMILARITY_VERSION, WEIGHTS};

use crate::enrich::{similar_key, EnrichmentData};
use crate::record::{AnalysisRecord, TagsDto};
use crate::{Result, SonagramError};

use normalize::{
    album_id, album_name, artist_id, decade_id, filename_from_path, genre_id, tempo_band, KEYS,
    TEMPO_BANDS,
};

/// Version of *this* graph schema (node/edge/property layout). Distinct from the
/// analysis schema version (which lives on each `Track`). Bump when the mapping
/// changes shape.
///
/// v2 (P21): `Track` gains the curve-derived Stage-A properties (`macro_dynamics`,
/// `energy_arc_range`, `energy_builds_per_min`, `flow_smoothness`, `chord_vocab`,
/// `chord_entropy`, `chord_churn`, `tempo_steadiness`, `seg_density`), the
/// percentile-calibrated Stage-B axes (`arousal_index`, `valence_index`,
/// `tension_index`, `recording_quality`, `quality_tier`), and the Stage-C version
/// layer: an `is_canonical` bool on every `Track`, plus a `Song` node per version
/// group (≥2 recordings sharing `(artist_id, normalized_title)`) with
/// `Track -[:VERSION_OF]-> Song` edges. P21b extends the same unreleased v2 with
/// always-present `lastfm_listeners`, `lastfm_playcount`, `has_lastfm_match`, and
/// listener-percentile `popularity` Track columns; Last.fm recognition precedes
/// audio quality when choosing the canonical recording.
///
/// v3 (Sonara 0.3.1): `Track` gains the distinct fused-aggression properties
/// `aggression`, `aggression_confidence`, `aggression_forcefulness`,
/// `aggression_harshness`, `aggression_tension`, `aggression_rhythm`, and
/// `aggression_model_id`. These preserve Sonara's nullable rank, evidence
/// support, component diagnostics and exact model identity without replacing
/// the separate legacy `mood_aggressive` heuristic.
pub const GRAPH_SCHEMA_VERSION: u32 = 3;

/// The embedding-store model identity, **derived** from sonara's
/// [`SIMILARITY_VERSION`] (format `"sonara-similarity-v{N}"`) rather than
/// hardcoded. It stamps every similarity store's `model_id`, so a stored vector
/// is never silently reinterpreted under a different similarity version: bump
/// `SIMILARITY_VERSION` upstream and this id — and thus the golden digest — moves
/// with it automatically. This is the whole point of the upstream contract.
pub fn embedding_model_id() -> String {
    format!("sonara-similarity-v{SIMILARITY_VERSION}")
}

/// Versioned deterministic identity of the exact cached analysis records a
/// graph consumes. Unlike [`crate::scan::cache::scan_fingerprint`], this moves
/// when analysis values or their provenance/model identity change even if the
/// source files' path, size, and mtime do not.
pub fn build_input_fingerprint(records: &[AnalysisRecord]) -> Result<String> {
    let mut serialized = Vec::with_capacity(records.len());
    for record in records {
        serialized.push((
            record.source.content_hash.as_str(),
            record.to_json_pretty()?,
        ));
    }
    serialized.sort_by(|a, b| a.0.cmp(b.0).then_with(|| a.1.cmp(&b.1)));

    let mut hasher = blake3::Hasher::new();
    hash_fingerprint_field(&mut hasher, b"sonagram-build-input-v1");
    hash_fingerprint_field(
        &mut hasher,
        sonara::analyze::ANALYSIS_SCHEMA_VERSION
            .to_string()
            .as_bytes(),
    );
    hash_fingerprint_field(&mut hasher, SIMILARITY_VERSION.to_string().as_bytes());
    hash_fingerprint_field(&mut hasher, GRAPH_SCHEMA_VERSION.to_string().as_bytes());
    hash_fingerprint_field(&mut hasher, serialized.len().to_string().as_bytes());
    for (content_hash, json) in serialized {
        hash_fingerprint_field(&mut hasher, content_hash.as_bytes());
        hash_fingerprint_field(&mut hasher, json.as_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn hash_fingerprint_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn combined_build_input_fingerprint(source_fingerprints: &BTreeMap<String, String>) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_fingerprint_field(&mut hasher, b"sonagram-combined-build-input-v1");
    hash_fingerprint_field(
        &mut hasher,
        source_fingerprints.len().to_string().as_bytes(),
    );
    for (root, fingerprint) in source_fingerprints {
        hash_fingerprint_field(&mut hasher, root.as_bytes());
        hash_fingerprint_field(&mut hasher, fingerprint.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// The `(node_type, property)` key under which the similarity embedding store is
/// registered, and its distance metric.
pub const EMBEDDING_PROPERTY: &str = "similarity";
/// Distance metric for the similarity store. Euclidean over pre-weighted
/// vectors reproduces sonara's weighted-L2 ranking (see [`preweight`]).
pub const EMBEDDING_METRIC: &str = "euclidean";

// Node-type names (interned into the graph schema).
const LIBRARY: &str = "Library";
const SOURCE: &str = "Source";
const TRACK: &str = "Track";
const ARTIST: &str = "Artist";
const ALBUM: &str = "Album";
const GENRE: &str = "Genre";
const KEY: &str = "Key";
const TEMPO_BAND_TYPE: &str = "TempoBand";
const ENERGY_LEVEL: &str = "EnergyLevel";
const DECADE: &str = "Decade";
const STYLE: &str = "Style";
// P21 Stage C: one Song node per version group (≥2 recordings of the same song).
const SONG: &str = "Song";

// Edge-type names. None contains the reserved Cypher substring "CONTAINS".
const BY_ARTIST: &str = "BY_ARTIST";
const ON_ALBUM: &str = "ON_ALBUM";
const IN_GENRE: &str = "IN_GENRE";
const IN_KEY: &str = "IN_KEY";
const IN_TEMPO_BAND: &str = "IN_TEMPO_BAND";
const AT_ENERGY: &str = "AT_ENERGY";
const FROM_DECADE: &str = "FROM_DECADE";
// Phase 17 multi-source edge: Track → the Source it was scanned from.
const FROM_SOURCE: &str = "FROM_SOURCE";
// Phase 6 derived edges (built after the embedding store — see `derive`).
const SIMILAR_TO: &str = "SIMILAR_TO";
const CAMELOT_ADJACENT: &str = "CAMELOT_ADJACENT";
const IN_STYLE: &str = "IN_STYLE";
// P21 Stage C: every member recording → its Song version group.
const VERSION_OF: &str = "VERSION_OF";
// Phase 12 enrichment edge: human co-listening similarity from Last.fm. Carries
// `score` (the match weight) on Track→Track; `source="lastfm"` on Artist→Artist.
const CROWD_SIMILAR: &str = "CROWD_SIMILAR";

/// Sentinel `era_source` values stamped on each `Track` so an agent can tell
/// which year fed the track's `Decade`/`FROM_DECADE` era placement.
const ERA_SOURCE_ORIGINAL: &str = "original_year";
const ERA_SOURCE_FILE: &str = "file_year";

/// The year sonagram uses for era reasoning (the `Decade` dimension and the
/// `FROM_DECADE` edge), with its provenance. sonara 0.2.4 splits release year in
/// two: `tags.year` is the **file/edition** date (the reissue/compilation date on
/// re-releases) while `tags.original_year` is the **true original** release year.
/// For era placement we therefore **prefer `original_year`** and fall back to
/// `year`, returning `(year, source)` where `source` is [`ERA_SOURCE_ORIGINAL`] or
/// [`ERA_SOURCE_FILE`]. `None` when neither tag is present (no `FROM_DECADE` edge).
fn era_year(tags: Option<&TagsDto>) -> Option<(u32, &'static str)> {
    let t = tags?;
    match t.original_year {
        Some(oy) => Some((oy, ERA_SOURCE_ORIGINAL)),
        None => t.year.map(|y| (y, ERA_SOURCE_FILE)),
    }
}

/// Minimal library-root metadata for the `Library` root node.
#[derive(Debug, Clone)]
pub struct LibraryInfo {
    /// The library root (a display string; a file name or label, never a user
    /// directory tree — the scanner keeps paths relative). For a **single-source**
    /// build this is also the `Source` node id and every `Track.source_root`; a
    /// **multi-source** build (see [`build_graph_from_sources`]) overrides the
    /// `Library` label with `"multi-source"` and takes each source's root from the
    /// [`SourceInput`] instead.
    pub root: String,
    /// Number of tracks in the library. Stamped as a `Library` property.
    pub n_tracks: usize,
}

/// One configured source contributing records to a build (P17). Its `root` is the
/// absolute source directory (or a label for fixture builds) that becomes the
/// `Source` node id + `path` and every contained `Track.source_root`; playlist
/// export resolves absolute paths off `source_root`, so a multi-source graph
/// needs no `library_root` argument.
pub struct SourceInput<'a> {
    /// Absolute source root (Source node id + `Track.source_root`).
    pub root: String,
    /// The source's cached analysis records (need not be pre-sorted).
    pub records: &'a [AnalysisRecord],
    /// P19: the source's scan-state fingerprint (blake3 over its `index.json` at
    /// scan time), stamped as the `Source.scan_fingerprint` property so
    /// `sonagram status` can compare the graph against the current disk state.
    /// `None` for builds with no scan index (e.g. the frozen fixtures) — the
    /// column is then omitted entirely, keeping the golden digest byte-unchanged.
    pub scan_fingerprint: Option<String>,
}

/// Build a deterministic `DirGraph` from `records` per the music schema.
///
/// `records` need not be pre-sorted — they are sorted by `content_hash` here so
/// the output is identical regardless of input order. Returns an error if any
/// edge references a node that was not built (which would be a mapping bug —
/// `add_edges_from_specs` never vivifies endpoints).
pub fn build_graph(records: &[AnalysisRecord], library: &LibraryInfo) -> Result<Arc<DirGraph>> {
    build_graph_with_enrichment(records, None, library)
}

/// Build the graph, optionally folding in Last.fm [`EnrichmentData`] (P12).
///
/// This is the **single-source** entry point: all `records` belong to one source
/// whose root is `library.root` (so `Track.source_root` = `library.root` and a
/// single `Source` node id = `library.root` is stamped — P17). With
/// `enrichment == None` it matches [`build_graph`]. With `Some`, the enrichment
/// adds popularity/MBID/original-album properties to `Track`/`Artist`/`Album`
/// nodes, folds folksonomy tags into the existing `Genre` dimension (extra
/// `IN_GENRE` edges), and adds `CROWD_SIMILAR` human co-listening edges between
/// owned tracks (weighted) and owned artists.
///
/// Determinism is preserved: enrichment maps are `BTreeMap`-iterated, every new
/// edge set is deduped through a `BTreeSet` and grouped by `add_edges_from_specs`.
pub fn build_graph_with_enrichment(
    records: &[AnalysisRecord],
    enrichment: Option<&EnrichmentData>,
    library: &LibraryInfo,
) -> Result<Arc<DirGraph>> {
    let source = SourceInput {
        root: library.root.clone(),
        records,
        scan_fingerprint: None,
    };
    build_graph_from_sources(std::slice::from_ref(&source), enrichment, library)
}

/// Build the graph from **one or more** sources (P17), deterministically merging
/// their records into a single `Track` per audio content hash.
///
/// Sources are iterated in **sorted `root` order**, and the first source to carry
/// a given content hash wins the `Track` (its `path`/tags/document); later
/// duplicates of the same recording are dropped — so the same track present in
/// two libraries is one node, resolvable via its winning source's `source_root`.
/// Each source becomes a `Source` node (`path` + `n_tracks`) with a
/// `Track-[:FROM_SOURCE]->Source` edge. A multi-source build labels the `Library`
/// node `"multi-source"` and adds an `n_sources` property; a single-source build
/// keeps the existing single-root `Library` semantics.
pub fn build_graph_from_sources(
    sources: &[SourceInput],
    enrichment: Option<&EnrichmentData>,
    library: &LibraryInfo,
) -> Result<Arc<DirGraph>> {
    let mut graph = DirGraph::new();

    // ── Merge across sources (sorted root order, first-source-wins) ─────────
    let mut srcs: Vec<&SourceInput> = sources.iter().collect();
    srcs.sort_by(|a, b| a.root.cmp(&b.root));

    // content_hash → winning source root, and per-source winning-track counts.
    let mut source_of: BTreeMap<String, String> = BTreeMap::new();
    let mut source_counts: BTreeMap<String, i64> = BTreeMap::new();
    // P19: source root → its scan-state fingerprint (if any). Parallel to
    // `source_counts`, so `add_sources` can stamp `Source.scan_fingerprint`.
    let mut source_fingerprints: BTreeMap<String, Option<String>> = BTreeMap::new();
    // Exact cached-analysis identity, independent of source file stats. Always
    // present, including fixture builds, so graph and playlist provenance can
    // prove which analysis/model outputs were consumed.
    let mut source_build_fingerprints: BTreeMap<String, String> = BTreeMap::new();
    // Every configured source gets a node even if it wins no unique track.
    for s in &srcs {
        source_counts.entry(s.root.clone()).or_insert(0);
        source_fingerprints
            .entry(s.root.clone())
            .or_insert_with(|| s.scan_fingerprint.clone());
        let fingerprint = build_input_fingerprint(s.records)?;
        source_build_fingerprints
            .entry(s.root.clone())
            .or_insert(fingerprint);
    }
    let combined_build_fingerprint = combined_build_input_fingerprint(&source_build_fingerprints);
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut sorted: Vec<&AnalysisRecord> = Vec::new();
    for s in &srcs {
        for r in s.records {
            if seen.insert(r.source.content_hash.as_str()) {
                sorted.push(r);
                source_of.insert(r.source.content_hash.clone(), s.root.clone());
                *source_counts.get_mut(&s.root).expect("source seeded") += 1;
            }
        }
    }
    // Deterministic input order for the rest of the pipeline: sort by content hash.
    sorted.sort_by(|a, b| a.source.content_hash.cmp(&b.source.content_hash));
    let multi_source = srcs.len() > 1;

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
        // Decade is derived from the era year (original_year preferred over the
        // file/edition year) so a reissue lands in its true decade.
        if let Some((y, _)) = era_year(t) {
            decades.insert(decade_id(y));
        }
    }

    // Fold folksonomy tags (owned tracks + owned artists) into the Genre
    // dimension so the Genre nodes exist before any IN_GENRE edge is added.
    if let Some(enr) = enrichment {
        for r in &sorted {
            if let Some(rec) = enr.tracks.get(&r.source.content_hash) {
                for tag in &rec.tags {
                    if let Some(g) = genre_id(Some(tag)) {
                        genres.insert(g);
                    }
                }
            }
        }
        for art in artists.keys() {
            if let Some(rec) = enr.artists.get(art) {
                for tag in &rec.tags {
                    if let Some(g) = genre_id(Some(tag)) {
                        genres.insert(g);
                    }
                }
            }
        }
    }

    // ── Stage 1: dimension nodes ────────────────────────────────────────────
    // (The `Library` root is built LAST — Stage 9 — so it can carry the adaptive
    // `style_threshold` the Style pass chooses. It has no edges, so its build
    // order does not affect any endpoint.)
    add_artists(&mut graph, &artists, enrichment)?;
    add_albums(&mut graph, &albums, enrichment)?;
    add_genres(&mut graph, &genres)?;
    add_keys(&mut graph)?;
    add_tempo_bands(&mut graph)?;
    add_energy_levels(&mut graph)?;
    add_decades(&mut graph, &decades)?;
    // P17: one Source node per configured source (endpoints for FROM_SOURCE).
    // P19: also stamps each source's scan_fingerprint when available.
    add_sources(
        &mut graph,
        &source_counts,
        &source_fingerprints,
        &source_build_fingerprints,
    )?;

    // P21 Stage A curve features + Stage B composite axes, computed once over the
    // sorted set (pure functions of the cached record curves/scalars — no
    // re-scan). Hoisted here so the Stage-C version grouping can read
    // `recording_quality` to pick each song's canonical take, and so both `Track`
    // properties and the `Song` layer see identical values.
    let feats: Vec<features::CurveFeatures> =
        sorted.iter().map(|r| features::curve_features(r)).collect();
    let axes = features::composite_axes(&sorted, &feats);
    let quality: Vec<Option<f64>> = axes.iter().map(|a| a.recording_quality).collect();
    let popularity = track_popularity_columns(&sorted, enrichment);
    let grouping = song::group_songs(&sorted, &quality, &popularity.has_lastfm_match);

    // ── Stage 3: Track nodes (single full-width pass) ───────────────────────
    add_tracks(
        &mut graph,
        &sorted,
        &source_of,
        enrichment,
        &feats,
        &axes,
        &popularity,
        &grouping.is_canonical,
    )?;

    // ── Stage 4: edges (all endpoints now exist) ────────────────────────────
    let specs = build_edges(&sorted, &albums, &source_of);
    let report = add_edges_from_specs(&mut graph, specs).map_err(SonagramError::Graph)?;
    if report.skipped_missing_endpoint != 0 {
        return Err(SonagramError::Graph(format!(
            "{} edge(s) referenced a missing endpoint — a mapping bug",
            report.skipped_missing_endpoint
        )));
    }

    // ── Stage 4b: enrichment edges (folksonomy IN_GENRE + CROWD_SIMILAR) ─────
    // After the base edges (so Track→Genre dedup can see them), before the
    // derived similarity/style stages (which read only the embedding store).
    if let Some(enr) = enrichment {
        add_enrichment_edges(&mut graph, &sorted, &artists, enr)?;
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

    // The primary groups above are the fixed target universe. Junk-tagged
    // tracks may move only when an either-direction SIMILAR_TO edge reaches an
    // original member of one unique non-junk target; reassigned tracks never
    // become confirmation anchors for a cascade.
    let grouping = song::refine_songs(
        &sorted,
        &quality,
        &popularity.has_lastfm_match,
        &grouping,
        &sim_edges,
    );
    song::update_canonical_flags(&mut graph, &sorted, &grouping.is_canonical)?;
    song::add_songs(&mut graph, &grouping)?;

    // ── Stage 7: CAMELOT_ADJACENT (static wheel between the 24 Key nodes) ────
    derive::add_camelot_adjacent(&mut graph)?;

    // ── Stage 8: Style community nodes + IN_STYLE edges ─────────────────────
    // The Style pass chooses a deterministic adaptive threshold from this
    // build's own mutual-kNN score distribution (P10c) and returns it to stamp.
    let (_n_styles, style_threshold) = derive::add_styles(&mut graph, &sorted, &sim_edges)?;

    // ── Stage 9: Library root (last — carries the chosen `style_threshold`) ──
    // Single-source: id/path = the one source root (existing semantics).
    // Multi-source (P17): id/path = "multi-source" + an `n_sources` property.
    let lib_label = if multi_source {
        "multi-source".to_string()
    } else {
        library.root.clone()
    };
    let mut lib_cols = vec![
        ("id", ColumnType::String, str1(&lib_label)),
        ("path", ColumnType::String, str1(&lib_label)),
        ("n_tracks", ColumnType::Int64, int1(sorted.len() as i64)),
        (
            "build_input_fingerprint",
            ColumnType::String,
            str1(&combined_build_fingerprint),
        ),
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
    ];
    if multi_source {
        lib_cols.push(("n_sources", ColumnType::Int64, int1(srcs.len() as i64)));
    }
    add(&mut graph, build_df(lib_cols), LIBRARY, "id", "path")?;

    Ok(Arc::new(graph))
}

/// Always-present Track popularity columns derived from the optional Last.fm
/// cache. A usable match is a fetched, non-failed record; listener/playcount
/// values remain nullable because Last.fm may resolve a track without returning
/// those statistics.
struct TrackPopularityColumns {
    listeners: Vec<Option<i64>>,
    playcount: Vec<Option<i64>>,
    has_lastfm_match: Vec<bool>,
    popularity: Vec<Option<f64>>,
}

/// Project Last.fm track records onto the content-hash-sorted input and rank
/// listener counts within the library. Equal listener counts receive the same
/// midrank percentile, so song-level Last.fm statistics do not invent a false
/// ordering between versions of the same song.
fn track_popularity_columns(
    sorted: &[&AnalysisRecord],
    enrichment: Option<&EnrichmentData>,
) -> TrackPopularityColumns {
    let mut listeners = Vec::with_capacity(sorted.len());
    let mut playcount = Vec::with_capacity(sorted.len());
    let mut has_lastfm_match = Vec::with_capacity(sorted.len());

    for r in sorted {
        let matched = enrichment
            .and_then(|enr| enr.tracks.get(&r.source.content_hash))
            .filter(|record| record.fetched && !record.failed);
        listeners.push(matched.and_then(|record| record.listeners));
        playcount.push(matched.and_then(|record| record.playcount));
        has_lastfm_match.push(matched.is_some());
    }

    let popularity = listener_percentile_rank(&listeners, sorted);
    TrackPopularityColumns {
        listeners,
        playcount,
        has_lastfm_match,
        popularity,
    }
}

/// Percentile rank non-null listener counts into `[0, 1]`; unmatched or
/// statistic-less tracks stay null, and a single ranked track sits at `0.5`.
fn listener_percentile_rank(
    listeners: &[Option<i64>],
    sorted: &[&AnalysisRecord],
) -> Vec<Option<f64>> {
    let mut ranked: Vec<(usize, i64, &str)> = listeners
        .iter()
        .enumerate()
        .filter_map(|(i, value)| value.map(|v| (i, v, sorted[i].source.content_hash.as_str())))
        .collect();
    ranked.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(b.2)));

    let mut out = vec![None; listeners.len()];
    match ranked.len() {
        0 => {}
        1 => out[ranked[0].0] = Some(0.5),
        n => {
            let denominator = (n - 1) as f64;
            let mut start = 0usize;
            while start < n {
                let mut end = start + 1;
                while end < n && ranked[end].1 == ranked[start].1 {
                    end += 1;
                }
                let midrank = (start + end - 1) as f64 / 2.0;
                let percentile = midrank / denominator;
                for (index, _, _) in &ranked[start..end] {
                    out[*index] = Some(percentile);
                }
                start = end;
            }
        }
    }
    out
}

/// Persist a built graph to `path` as a `.kgl` file.
///
/// Delegates to `kglite::api::io::save_graph`, which runs the mandatory
/// `prepare_save` before writing (skipping it drops all properties on reload).
pub fn save(graph: &mut Arc<DirGraph>, path: &Path) -> Result<()> {
    let p = path
        .to_str()
        .ok_or_else(|| SonagramError::Graph(format!("non-UTF-8 path: {}", path.display())))?;
    kglite::api::io::save_graph(graph, p).map_err(|e| SonagramError::Graph(e.to_string()))
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

fn add_artists(
    graph: &mut DirGraph,
    artists: &BTreeMap<String, i64>,
    enrichment: Option<&EnrichmentData>,
) -> Result<()> {
    if artists.is_empty() {
        return Ok(());
    }
    let ids: Vec<Option<String>> = artists.keys().map(|k| Some(k.clone())).collect();
    let names = ids.clone();
    let counts: Vec<Option<i64>> = artists.values().map(|c| Some(*c)).collect();
    let mut cols = vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
        ("n_tracks", ColumnType::Int64, ColumnData::Int64(counts)),
    ];
    // P12: enrichment properties. Only appended when enrichment is present, so
    // the plain build's Artist table (and golden digest) is byte-unchanged.
    if let Some(enr) = enrichment {
        let get = |k: &String| enr.artists.get(k).filter(|r| r.fetched && !r.failed);
        let playcount: Vec<Option<i64>> = artists
            .keys()
            .map(|k| get(k).and_then(|r| r.playcount))
            .collect();
        let listeners: Vec<Option<i64>> = artists
            .keys()
            .map(|k| get(k).and_then(|r| r.listeners))
            .collect();
        let mbid: Vec<Option<String>> = artists
            .keys()
            .map(|k| get(k).and_then(|r| r.mbid.clone()))
            .collect();
        cols.push((
            "lastfm_playcount",
            ColumnType::Int64,
            ColumnData::Int64(playcount),
        ));
        cols.push((
            "lastfm_listeners",
            ColumnType::Int64,
            ColumnData::Int64(listeners),
        ));
        cols.push(("mbid", ColumnType::String, ColumnData::String(mbid)));
    }
    add(graph, build_df(cols), ARTIST, "id", "name")
}

fn add_albums(
    graph: &mut DirGraph,
    albums: &BTreeMap<String, (String, String, Option<i64>)>,
    enrichment: Option<&EnrichmentData>,
) -> Result<()> {
    if albums.is_empty() {
        return Ok(());
    }
    let ids: Vec<Option<String>> = albums.keys().map(|k| Some(k.clone())).collect();
    let names: Vec<Option<String>> = albums.values().map(|(n, _, _)| Some(n.clone())).collect();
    let artist: Vec<Option<String>> = albums.values().map(|(_, a, _)| Some(a.clone())).collect();
    let years: Vec<Option<i64>> = albums.values().map(|(_, _, y)| *y).collect();
    let mut cols = vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
        ("artist", ColumnType::String, ColumnData::String(artist)),
        ("year", ColumnType::Int64, ColumnData::Int64(years)),
    ];
    // P12: enrichment properties (null-safe; only when enrichment is present).
    if let Some(enr) = enrichment {
        let get = |k: &String| enr.albums.get(k).filter(|r| r.fetched && !r.failed);
        let playcount: Vec<Option<i64>> = albums
            .keys()
            .map(|k| get(k).and_then(|r| r.playcount))
            .collect();
        let listeners: Vec<Option<i64>> = albums
            .keys()
            .map(|k| get(k).and_then(|r| r.listeners))
            .collect();
        let mbid: Vec<Option<String>> = albums
            .keys()
            .map(|k| get(k).and_then(|r| r.mbid.clone()))
            .collect();
        let url: Vec<Option<String>> = albums
            .keys()
            .map(|k| get(k).and_then(|r| r.url.clone()))
            .collect();
        let wiki: Vec<Option<String>> = albums
            .keys()
            .map(|k| get(k).and_then(|r| r.wiki_summary.clone()))
            .collect();
        cols.push((
            "lastfm_playcount",
            ColumnType::Int64,
            ColumnData::Int64(playcount),
        ));
        cols.push((
            "lastfm_listeners",
            ColumnType::Int64,
            ColumnData::Int64(listeners),
        ));
        cols.push(("mbid", ColumnType::String, ColumnData::String(mbid)));
        cols.push(("lastfm_url", ColumnType::String, ColumnData::String(url)));
        cols.push(("wiki_summary", ColumnType::String, ColumnData::String(wiki)));
    }
    add(graph, build_df(cols), ALBUM, "id", "name")
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
    let ids: Vec<Option<String>> = TEMPO_BANDS
        .iter()
        .map(|(n, _, _)| Some(n.to_string()))
        .collect();
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

/// P17: one `Source` node per configured source, id = the absolute source root,
/// with `path` (= id) and `n_tracks` (winning-track count). `BTreeMap`-iterated,
/// so the node order is fixed.
fn add_sources(
    graph: &mut DirGraph,
    source_counts: &BTreeMap<String, i64>,
    source_fingerprints: &BTreeMap<String, Option<String>>,
    source_build_fingerprints: &BTreeMap<String, String>,
) -> Result<()> {
    if source_counts.is_empty() {
        return Ok(());
    }
    let ids: Vec<Option<String>> = source_counts.keys().map(|k| Some(k.clone())).collect();
    let paths = ids.clone();
    let counts: Vec<Option<i64>> = source_counts.values().map(|c| Some(*c)).collect();
    let build_fingerprints: Vec<Option<String>> = source_counts
        .keys()
        .map(|root| source_build_fingerprints.get(root).cloned())
        .collect();
    let mut cols = vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("path", ColumnType::String, ColumnData::String(paths)),
        ("n_tracks", ColumnType::Int64, ColumnData::Int64(counts)),
        (
            "build_input_fingerprint",
            ColumnType::String,
            ColumnData::String(build_fingerprints),
        ),
    ];
    // P19: stamp `scan_fingerprint` ONLY when at least one source carries one, so a
    // fixture build (no index) omits the column entirely and the golden digest is
    // byte-unchanged. Sources without a fingerprint get a null cell.
    if source_fingerprints.values().any(Option::is_some) {
        let fps: Vec<Option<String>> = source_counts
            .keys()
            .map(|k| source_fingerprints.get(k).cloned().flatten())
            .collect();
        cols.push((
            "scan_fingerprint",
            ColumnType::String,
            ColumnData::String(fps),
        ));
    }
    add(graph, build_df(cols), SOURCE, "id", "path")
}

// ─────────────────────────────── Track builder ──────────────────────────────

/// Build the single full-width `Track` DataFrame. Every column is present for
/// every row (missing optionals → null cells) so this is one `add_nodes` pass —
/// a partial DataFrame would rebuild nodes from only its columns, dropping the
/// rest.
#[allow(clippy::too_many_arguments)]
fn add_tracks(
    graph: &mut DirGraph,
    sorted: &[&AnalysisRecord],
    source_of: &BTreeMap<String, String>,
    enrichment: Option<&EnrichmentData>,
    feats: &[features::CurveFeatures],
    axes: &[features::CompositeAxes],
    popularity: &TrackPopularityColumns,
    is_canonical: &[bool],
) -> Result<()> {
    if sorted.is_empty() {
        return Ok(());
    }

    // Helpers to project each record.
    let tag_str =
        |r: &AnalysisRecord, f: fn(&crate::record::TagsDto) -> Option<&str>| -> Option<String> {
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
    // P17: the absolute source root the track was scanned from, so playlist
    // export resolves `source_root` + relative `path` without a library_root arg.
    let source_root: Vec<Option<String>> = sorted
        .iter()
        .map(|r| source_of.get(&r.source.content_hash).cloned())
        .collect();
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
    let genre_tag: Vec<Option<String>> = sorted
        .iter()
        .map(|r| tag_str(r, |t| t.genre.as_deref()))
        .collect();
    let format: Vec<Option<String>> = sorted
        .iter()
        .map(|r| Some(r.source.format.clone()))
        .collect();
    let genre_model_id = str_opt_col(sorted, |r| r.analysis.provenance.genre_model_id.clone());
    let vocalness_model_id =
        str_opt_col(sorted, |r| r.analysis.provenance.vocalness_model_id.clone());
    let aggression_model_id = str_opt_col(sorted, |r| {
        r.analysis.provenance.aggression_model_id.clone()
    });

    // Int columns.
    let year = int_opt_col(sorted, |r| {
        r.tags.as_ref().and_then(|t| t.year).map(|y| y as i64)
    });
    // sonara 0.2.4: original release year (reissue-safe), and the source of the
    // era year the Decade/FROM_DECADE mapping actually used ("original_year" |
    // "file_year" | null when no year tag at all).
    let original_year = int_opt_col(sorted, |r| {
        r.tags
            .as_ref()
            .and_then(|t| t.original_year)
            .map(|y| y as i64)
    });
    let era_source = str_opt_col(sorted, |r| {
        era_year(r.tags.as_ref()).map(|(_, s)| s.to_string())
    });
    let track_no = int_opt_col(sorted, |r| {
        r.tags.as_ref().and_then(|t| t.track_no).map(|n| n as i64)
    });
    let file_size = int_opt_col(sorted, |r| Some(r.source.file_size as i64));
    let energy_level = int_opt_col(sorted, |r| r.analysis.energy_level.map(|e| e as i64));
    let n_segments = int_opt_col(sorted, |r| {
        r.analysis.segments.as_ref().map(|s| s.len() as i64)
    });
    let analysis_schema_version = int_opt_col(sorted, |r| {
        Some(r.analysis.provenance.schema_version as i64)
    });
    let embedding_version = int_opt_col(sorted, |r| r.analysis.embedding_version.map(|v| v as i64));

    // Float columns — always-present (non-Option in the DTO) then optional.
    let duration_sec = f_col(sorted, |r| r.analysis.duration_sec);
    let bpm = f_col(sorted, |r| r.analysis.bpm);
    let bpm_raw = f_col(sorted, |r| r.analysis.bpm_raw);
    let bpm_confidence = f_col(sorted, |r| r.analysis.bpm_confidence);
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
    // Sonara 0.3.1 fused aggression. Project every value independently: a null
    // rank with present support/components is a valid low-content abstention,
    // not missing analysis. `mood_aggressive` above remains a distinct legacy
    // mood heuristic and is never overwritten or used as fallback.
    let aggression = fo_col(sorted, |r| r.analysis.aggression_score);
    let aggression_confidence = fo_col(sorted, |r| r.analysis.aggression_confidence);
    let aggression_forcefulness = fo_col(sorted, |r| r.analysis.aggression_forcefulness);
    let aggression_harshness = fo_col(sorted, |r| r.analysis.aggression_harshness);
    let aggression_tension = fo_col(sorted, |r| r.analysis.aggression_tension);
    let aggression_rhythm = fo_col(sorted, |r| r.analysis.aggression_rhythm);

    // P21 Stage A curve features + Stage B composite axes are computed once by the
    // caller (see `build_graph_from_sources`) so the Stage-C song grouping and
    // these `Track` columns read identical values; here they are just projected
    // into columns.
    let macro_dynamics: Vec<Option<f64>> = feats.iter().map(|f| f.macro_dynamics).collect();
    let energy_arc_range: Vec<Option<f64>> = feats.iter().map(|f| f.energy_arc_range).collect();
    let energy_builds_per_min: Vec<Option<f64>> =
        feats.iter().map(|f| f.energy_builds_per_min).collect();
    let flow_smoothness: Vec<Option<f64>> = feats.iter().map(|f| f.flow_smoothness).collect();
    let chord_vocab: Vec<Option<i64>> = feats.iter().map(|f| f.chord_vocab).collect();
    let chord_entropy: Vec<Option<f64>> = feats.iter().map(|f| f.chord_entropy).collect();
    let chord_churn: Vec<Option<f64>> = feats.iter().map(|f| f.chord_churn).collect();
    let tempo_steadiness: Vec<Option<f64>> = feats.iter().map(|f| f.tempo_steadiness).collect();
    let seg_density: Vec<Option<f64>> = feats.iter().map(|f| f.seg_density).collect();

    let arousal_index: Vec<Option<f64>> = axes.iter().map(|a| a.arousal_index).collect();
    let valence_index: Vec<Option<f64>> = axes.iter().map(|a| a.valence_index).collect();
    let tension_index: Vec<Option<f64>> = axes.iter().map(|a| a.tension_index).collect();
    let recording_quality: Vec<Option<f64>> = axes.iter().map(|a| a.recording_quality).collect();
    let quality_tier: Vec<Option<String>> = axes.iter().map(|a| a.quality_tier.clone()).collect();

    // P21b: Last.fm popularity columns are part of the Track schema even for a
    // plain build. Counts and percentile stay null without a usable match;
    // `has_lastfm_match` is always a concrete boolean.
    let lastfm_listeners = popularity.listeners.clone();
    let lastfm_playcount = popularity.playcount.clone();
    let popularity_rank = popularity.popularity.clone();
    let has_lastfm_match: Vec<Option<bool>> = popularity
        .has_lastfm_match
        .iter()
        .map(|&matched| Some(matched))
        .collect();
    let is_music: Vec<Option<bool>> = axes.iter().map(|a| Some(a.is_music)).collect();

    // P21 Stage C: `is_canonical` is non-null on every Track (all singletons and
    // every version group's best take are `true`), so `WHERE t.is_canonical` is
    // the universal "skip duplicate/inferior takes" filter.
    let is_canonical_col: Vec<Option<bool>> = is_canonical.iter().map(|&b| Some(b)).collect();

    // String (optional) tonal/rhythm columns.
    let time_signature = str_opt_col(sorted, |r| r.analysis.time_signature.clone());
    let key = str_opt_col(sorted, |r| r.analysis.key.clone());
    let camelot = str_opt_col(sorted, |r| r.analysis.key_camelot.clone());
    let predominant_chord = str_opt_col(sorted, |r| r.analysis.predominant_chord.clone());

    let mut cols = vec![
        (
            "content_hash",
            ColumnType::String,
            ColumnData::String(unique_id),
        ),
        ("title", ColumnType::String, ColumnData::String(title)),
        ("path", ColumnType::String, ColumnData::String(path)),
        (
            "source_root",
            ColumnType::String,
            ColumnData::String(source_root),
        ),
        ("filename", ColumnType::String, ColumnData::String(filename)),
        (
            "artist_name",
            ColumnType::String,
            ColumnData::String(artist_name),
        ),
        (
            "album_name",
            ColumnType::String,
            ColumnData::String(album_name_col),
        ),
        (
            "genre_tag",
            ColumnType::String,
            ColumnData::String(genre_tag),
        ),
        ("format", ColumnType::String, ColumnData::String(format)),
        ("year", ColumnType::Int64, ColumnData::Int64(year)),
        (
            "original_year",
            ColumnType::Int64,
            ColumnData::Int64(original_year),
        ),
        (
            "era_source",
            ColumnType::String,
            ColumnData::String(era_source),
        ),
        ("track_no", ColumnType::Int64, ColumnData::Int64(track_no)),
        ("file_size", ColumnType::Int64, ColumnData::Int64(file_size)),
        (
            "energy_level",
            ColumnType::Int64,
            ColumnData::Int64(energy_level),
        ),
        (
            "n_segments",
            ColumnType::Int64,
            ColumnData::Int64(n_segments),
        ),
        (
            "analysis_schema_version",
            ColumnType::Int64,
            ColumnData::Int64(analysis_schema_version),
        ),
        (
            "embedding_version",
            ColumnType::Int64,
            ColumnData::Int64(embedding_version),
        ),
        (
            "genre_model_id",
            ColumnType::String,
            ColumnData::String(genre_model_id),
        ),
        (
            "vocalness_model_id",
            ColumnType::String,
            ColumnData::String(vocalness_model_id),
        ),
        (
            "aggression_model_id",
            ColumnType::String,
            ColumnData::String(aggression_model_id),
        ),
        (
            "duration_sec",
            ColumnType::Float64,
            ColumnData::Float64(duration_sec),
        ),
        ("bpm", ColumnType::Float64, ColumnData::Float64(bpm)),
        ("bpm_raw", ColumnType::Float64, ColumnData::Float64(bpm_raw)),
        (
            "bpm_confidence",
            ColumnType::Float64,
            ColumnData::Float64(bpm_confidence),
        ),
        (
            "loudness_lufs",
            ColumnType::Float64,
            ColumnData::Float64(loudness_lufs),
        ),
        (
            "dynamic_range_db",
            ColumnType::Float64,
            ColumnData::Float64(dynamic_range_db),
        ),
        (
            "spectral_centroid",
            ColumnType::Float64,
            ColumnData::Float64(spectral_centroid),
        ),
        (
            "zero_crossing_rate",
            ColumnType::Float64,
            ColumnData::Float64(zero_crossing_rate),
        ),
        (
            "onset_density",
            ColumnType::Float64,
            ColumnData::Float64(onset_density),
        ),
        (
            "tempo_variability",
            ColumnType::Float64,
            ColumnData::Float64(tempo_variability),
        ),
        (
            "grid_stability",
            ColumnType::Float64,
            ColumnData::Float64(grid_stability),
        ),
        (
            "grid_offset_sec",
            ColumnType::Float64,
            ColumnData::Float64(grid_offset_sec),
        ),
        ("energy", ColumnType::Float64, ColumnData::Float64(energy)),
        ("valence", ColumnType::Float64, ColumnData::Float64(valence)),
        (
            "danceability",
            ColumnType::Float64,
            ColumnData::Float64(danceability),
        ),
        (
            "acousticness",
            ColumnType::Float64,
            ColumnData::Float64(acousticness),
        ),
        (
            "vocalness",
            ColumnType::Float64,
            ColumnData::Float64(vocalness),
        ),
        (
            "dissonance",
            ColumnType::Float64,
            ColumnData::Float64(dissonance),
        ),
        (
            "mood_happy",
            ColumnType::Float64,
            ColumnData::Float64(mood_happy),
        ),
        (
            "mood_aggressive",
            ColumnType::Float64,
            ColumnData::Float64(mood_aggressive),
        ),
        (
            "mood_relaxed",
            ColumnType::Float64,
            ColumnData::Float64(mood_relaxed),
        ),
        (
            "mood_sad",
            ColumnType::Float64,
            ColumnData::Float64(mood_sad),
        ),
        (
            "instrumentalness",
            ColumnType::Float64,
            ColumnData::Float64(instrumentalness),
        ),
        (
            "key_confidence",
            ColumnType::Float64,
            ColumnData::Float64(key_confidence),
        ),
        (
            "chord_change_rate",
            ColumnType::Float64,
            ColumnData::Float64(chord_change_rate),
        ),
        (
            "loudness_range_lu",
            ColumnType::Float64,
            ColumnData::Float64(loudness_range_lu),
        ),
        (
            "true_peak_db",
            ColumnType::Float64,
            ColumnData::Float64(true_peak_db),
        ),
        (
            "replaygain_db",
            ColumnType::Float64,
            ColumnData::Float64(replaygain_db),
        ),
        (
            "intro_end_sec",
            ColumnType::Float64,
            ColumnData::Float64(intro_end_sec),
        ),
        (
            "outro_start_sec",
            ColumnType::Float64,
            ColumnData::Float64(outro_start_sec),
        ),
        (
            "leading_silence_sec",
            ColumnType::Float64,
            ColumnData::Float64(leading_silence_sec),
        ),
        (
            "trailing_silence_sec",
            ColumnType::Float64,
            ColumnData::Float64(trailing_silence_sec),
        ),
        (
            "spectral_flatness",
            ColumnType::Float64,
            ColumnData::Float64(spectral_flatness),
        ),
        // Sonara fused aggression rank + evidence diagnostics (graph schema v3).
        (
            "aggression",
            ColumnType::Float64,
            ColumnData::Float64(aggression),
        ),
        (
            "aggression_confidence",
            ColumnType::Float64,
            ColumnData::Float64(aggression_confidence),
        ),
        (
            "aggression_forcefulness",
            ColumnType::Float64,
            ColumnData::Float64(aggression_forcefulness),
        ),
        (
            "aggression_harshness",
            ColumnType::Float64,
            ColumnData::Float64(aggression_harshness),
        ),
        (
            "aggression_tension",
            ColumnType::Float64,
            ColumnData::Float64(aggression_tension),
        ),
        (
            "aggression_rhythm",
            ColumnType::Float64,
            ColumnData::Float64(aggression_rhythm),
        ),
        // P21 Stage A: curve-derived flat features.
        (
            "macro_dynamics",
            ColumnType::Float64,
            ColumnData::Float64(macro_dynamics),
        ),
        (
            "energy_arc_range",
            ColumnType::Float64,
            ColumnData::Float64(energy_arc_range),
        ),
        (
            "energy_builds_per_min",
            ColumnType::Float64,
            ColumnData::Float64(energy_builds_per_min),
        ),
        (
            "flow_smoothness",
            ColumnType::Float64,
            ColumnData::Float64(flow_smoothness),
        ),
        (
            "chord_vocab",
            ColumnType::Int64,
            ColumnData::Int64(chord_vocab),
        ),
        (
            "chord_entropy",
            ColumnType::Float64,
            ColumnData::Float64(chord_entropy),
        ),
        (
            "chord_churn",
            ColumnType::Float64,
            ColumnData::Float64(chord_churn),
        ),
        (
            "tempo_steadiness",
            ColumnType::Float64,
            ColumnData::Float64(tempo_steadiness),
        ),
        (
            "seg_density",
            ColumnType::Float64,
            ColumnData::Float64(seg_density),
        ),
        // P21 Stage B: percentile-calibrated composite axes.
        (
            "is_music",
            ColumnType::Boolean,
            ColumnData::Boolean(is_music),
        ),
        (
            "arousal_index",
            ColumnType::Float64,
            ColumnData::Float64(arousal_index),
        ),
        (
            "valence_index",
            ColumnType::Float64,
            ColumnData::Float64(valence_index),
        ),
        (
            "tension_index",
            ColumnType::Float64,
            ColumnData::Float64(tension_index),
        ),
        (
            "recording_quality",
            ColumnType::Float64,
            ColumnData::Float64(recording_quality),
        ),
        (
            "quality_tier",
            ColumnType::String,
            ColumnData::String(quality_tier),
        ),
        // P21b: recognition/popularity (always-present columns).
        (
            "lastfm_listeners",
            ColumnType::Int64,
            ColumnData::Int64(lastfm_listeners),
        ),
        (
            "lastfm_playcount",
            ColumnType::Int64,
            ColumnData::Int64(lastfm_playcount),
        ),
        (
            "has_lastfm_match",
            ColumnType::Boolean,
            ColumnData::Boolean(has_lastfm_match),
        ),
        (
            "popularity",
            ColumnType::Float64,
            ColumnData::Float64(popularity_rank),
        ),
        // P21 Stage C: canonical-take flag (non-null bool).
        (
            "is_canonical",
            ColumnType::Boolean,
            ColumnData::Boolean(is_canonical_col),
        ),
        (
            "time_signature",
            ColumnType::String,
            ColumnData::String(time_signature),
        ),
        ("key", ColumnType::String, ColumnData::String(key)),
        ("camelot", ColumnType::String, ColumnData::String(camelot)),
        (
            "predominant_chord",
            ColumnType::String,
            ColumnData::String(predominant_chord),
        ),
    ];

    // P12: optional enrichment metadata, joined by content_hash. The P21b
    // popularity columns above are always present; these older metadata columns
    // remain conditional to preserve their existing schema contract.
    if let Some(enr) = enrichment {
        let get = |r: &AnalysisRecord| {
            enr.tracks
                .get(&r.source.content_hash)
                .filter(|e| e.fetched && !e.failed)
        };
        let mbid = str_opt_col(sorted, |r| get(r).and_then(|e| e.mbid.clone()));
        let lastfm_url = str_opt_col(sorted, |r| get(r).and_then(|e| e.url.clone()));
        let original_album = str_opt_col(sorted, |r| get(r).and_then(|e| e.album_title.clone()));
        let original_album_position =
            int_opt_col(sorted, |r| get(r).and_then(|e| e.album_position));
        cols.push(("mbid", ColumnType::String, ColumnData::String(mbid)));
        cols.push((
            "lastfm_url",
            ColumnType::String,
            ColumnData::String(lastfm_url),
        ));
        cols.push((
            "original_album",
            ColumnType::String,
            ColumnData::String(original_album),
        ));
        cols.push((
            "original_album_position",
            ColumnType::Int64,
            ColumnData::Int64(original_album_position),
        ));
    }

    add(graph, build_df(cols), TRACK, "content_hash", "title")
}

// ─────────────────────────────── Edge builder ───────────────────────────────

fn build_edges(
    sorted: &[&AnalysisRecord],
    albums: &BTreeMap<String, (String, String, Option<i64>)>,
    source_of: &BTreeMap<String, String>,
) -> Vec<EdgeSpec> {
    let mut specs: Vec<EdgeSpec> = Vec::new();
    for r in sorted {
        let hash = &r.source.content_hash;
        let t = r.tags.as_ref();
        let art = artist_id(t.and_then(|t| t.artist.as_deref()));

        specs.push(edge(TRACK, hash, ARTIST, &art, BY_ARTIST));

        // P17: Track → the Source it was scanned from.
        if let Some(src_root) = source_of.get(hash) {
            specs.push(edge(TRACK, hash, SOURCE, src_root, FROM_SOURCE));
        }

        if let Some(aid) = album_id(&art, t.and_then(|t| t.album.as_deref())) {
            specs.push(edge(TRACK, hash, ALBUM, &aid, ON_ALBUM));
        }
        if let Some(g) = genre_id(t.and_then(|t| t.genre.as_deref())) {
            specs.push(edge(TRACK, hash, GENRE, &g, IN_GENRE));
        }
        if let Some(k) = &r.analysis.key {
            specs.push(edge(TRACK, hash, KEY, k, IN_KEY));
        }
        specs.push(edge(
            TRACK,
            hash,
            TEMPO_BAND_TYPE,
            tempo_band(r.analysis.bpm),
            IN_TEMPO_BAND,
        ));
        if let Some(el) = r.analysis.energy_level {
            specs.push(edge(TRACK, hash, ENERGY_LEVEL, &el.to_string(), AT_ENERGY));
        }
        // FROM_DECADE uses the era year (original_year preferred over file year),
        // matching the Decade dimension; `Track.era_source` records which was used.
        if let Some((y, _)) = era_year(t) {
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

fn edge_with(
    src_type: &str,
    src_id: &str,
    tgt_type: &str,
    tgt_id: &str,
    edge_type: &str,
    properties: HashMap<String, Value>,
) -> EdgeSpec {
    EdgeSpec {
        source_type: src_type.to_string(),
        source_id: Value::String(src_id.to_string()),
        target_type: tgt_type.to_string(),
        target_id: Value::String(tgt_id.to_string()),
        edge_type: edge_type.to_string(),
        properties,
    }
}

// ───────────────────────── Enrichment edge builder (P12) ────────────────────

/// Add the Last.fm enrichment edges, in deterministic, deduped order:
///
/// - **folksonomy `IN_GENRE`** — extra `Track→Genre` (from a track's Last.fm
///   tags) and `Artist→Genre` (from an artist's Last.fm tags). Track→Genre edges
///   are deduped against the base file-genre `IN_GENRE` edges (skip-safe); the
///   `Genre` nodes already exist (folded into the dimension in Stage 1).
/// - **`CROWD_SIMILAR` `Track→Track`** — for each owned track's Last.fm similar
///   list, an edge to the resolved owned track (by normalized `artist::title`),
///   carrying `score` = the preserved match weight. Similar entries that resolve
///   to a non-owned track are dropped; self-loops are dropped.
/// - **`CROWD_SIMILAR` `Artist→Artist`** — for each owned artist's Last.fm
///   similar-artist names, an edge to the resolved owned artist (case-insensitive),
///   carrying `source = "lastfm"` (no weight is available). Self-loops dropped.
///
/// Everything is `BTreeMap`/`BTreeSet`-derived, so the edge set is identical
/// across runs and input orderings.
fn add_enrichment_edges(
    graph: &mut DirGraph,
    sorted: &[&AnalysisRecord],
    artists: &BTreeMap<String, i64>,
    enr: &EnrichmentData,
) -> Result<()> {
    // Resolvers built from the owned records.
    // track key ("artist-lower::title-lower") → content_hash.
    let mut track_key_to_hash: BTreeMap<String, String> = BTreeMap::new();
    // content_hash → its own track key (to skip self-loops by identity).
    for r in sorted {
        let t = r.tags.as_ref();
        let art = artist_id(t.and_then(|t| t.artist.as_deref()));
        if let Some(title) = t
            .and_then(|t| t.title.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            track_key_to_hash
                .entry(similar_key(&art, title))
                .or_insert_with(|| r.source.content_hash.clone());
        }
    }
    // lowercased artist id → artist id.
    let artist_lower_to_id: BTreeMap<String, String> = artists
        .keys()
        .map(|a| (a.trim().to_lowercase(), a.clone()))
        .collect();

    // ── Folksonomy IN_GENRE ──
    // Base Track→Genre edges (file genre), so folksonomy adds don't duplicate.
    let mut base_track_genre: BTreeSet<(String, String)> = BTreeSet::new();
    for r in sorted {
        if let Some(g) = genre_id(r.tags.as_ref().and_then(|t| t.genre.as_deref())) {
            base_track_genre.insert((r.source.content_hash.clone(), g));
        }
    }

    let mut genre_specs: Vec<EdgeSpec> = Vec::new();
    let mut emitted_track_genre: BTreeSet<(String, String)> = BTreeSet::new();
    for r in sorted {
        if let Some(rec) = enr.tracks.get(&r.source.content_hash) {
            for tag in &rec.tags {
                if let Some(g) = genre_id(Some(tag)) {
                    let key = (r.source.content_hash.clone(), g.clone());
                    if base_track_genre.contains(&key) || !emitted_track_genre.insert(key) {
                        continue; // dedup vs base + within folksonomy
                    }
                    genre_specs.push(edge(TRACK, &r.source.content_hash, GENRE, &g, IN_GENRE));
                }
            }
        }
    }
    let mut emitted_artist_genre: BTreeSet<(String, String)> = BTreeSet::new();
    for art in artists.keys() {
        if let Some(rec) = enr.artists.get(art) {
            for tag in &rec.tags {
                if let Some(g) = genre_id(Some(tag)) {
                    if emitted_artist_genre.insert((art.clone(), g.clone())) {
                        genre_specs.push(edge(ARTIST, art, GENRE, &g, IN_GENRE));
                    }
                }
            }
        }
    }
    check_edges(
        add_edges_from_specs(graph, genre_specs),
        "folksonomy IN_GENRE",
    )?;

    // ── CROWD_SIMILAR Track→Track (weighted) ──
    let mut crowd_specs: Vec<EdgeSpec> = Vec::new();
    let mut emitted_tt: BTreeSet<(String, String)> = BTreeSet::new();
    for r in sorted {
        let src = &r.source.content_hash;
        if let Some(rec) = enr.tracks.get(src) {
            for sim in &rec.similar {
                let key = similar_key(&sim.artist, &sim.title);
                let Some(tgt) = track_key_to_hash.get(&key) else {
                    continue; // resolves to a non-owned track → drop
                };
                if tgt == src || !emitted_tt.insert((src.clone(), tgt.clone())) {
                    continue; // self-loop / duplicate
                }
                let mut props = HashMap::new();
                props.insert("score".to_string(), Value::Float64(sim.match_weight as f64));
                crowd_specs.push(edge_with(TRACK, src, TRACK, tgt, CROWD_SIMILAR, props));
            }
        }
    }

    // ── CROWD_SIMILAR Artist→Artist (unweighted, source="lastfm") ──
    let mut emitted_aa: BTreeSet<(String, String)> = BTreeSet::new();
    for art in artists.keys() {
        if let Some(rec) = enr.artists.get(art) {
            for name in &rec.similar {
                let Some(tgt) = artist_lower_to_id.get(&name.trim().to_lowercase()) else {
                    continue; // non-owned artist → drop
                };
                if tgt == art || !emitted_aa.insert((art.clone(), tgt.clone())) {
                    continue;
                }
                let mut props = HashMap::new();
                props.insert("source".to_string(), Value::String("lastfm".to_string()));
                crowd_specs.push(edge_with(ARTIST, art, ARTIST, tgt, CROWD_SIMILAR, props));
            }
        }
    }
    check_edges(add_edges_from_specs(graph, crowd_specs), "CROWD_SIMILAR")?;

    Ok(())
}

/// Turn an `add_edges_from_specs` result into an error if any edge referenced a
/// missing endpoint (a mapping bug — endpoints are never vivified here).
fn check_edges(
    result: std::result::Result<kglite::api::mutation::EdgeSpecReport, String>,
    what: &str,
) -> Result<()> {
    let report = result.map_err(SonagramError::Graph)?;
    if report.skipped_missing_endpoint != 0 {
        return Err(SonagramError::Graph(format!(
            "{} {} edge(s) referenced a missing endpoint — a mapping bug",
            report.skipped_missing_endpoint, what
        )));
    }
    Ok(())
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

    fn tags_with(year: Option<u32>, original_year: Option<u32>) -> TagsDto {
        TagsDto {
            title: None,
            artist: None,
            album: None,
            genre: None,
            year,
            original_year,
            track_no: None,
        }
    }

    #[test]
    fn era_year_prefers_original_year_then_falls_back() {
        // original_year present ⇒ use it, source "original_year" (reissue-safe).
        assert_eq!(
            era_year(Some(&tags_with(Some(2015), Some(1972)))),
            Some((1972, ERA_SOURCE_ORIGINAL))
        );
        // No original_year ⇒ fall back to the file year, source "file_year".
        assert_eq!(
            era_year(Some(&tags_with(Some(2015), None))),
            Some((2015, ERA_SOURCE_FILE))
        );
        // Neither year present, or no tags at all ⇒ no era (no FROM_DECADE edge).
        assert_eq!(era_year(Some(&tags_with(None, None))), None);
        assert_eq!(era_year(None), None);
        // original_year present but no file year still resolves via original_year.
        assert_eq!(
            era_year(Some(&tags_with(None, Some(1969)))),
            Some((1969, ERA_SOURCE_ORIGINAL))
        );
    }

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
        let mut ours: Vec<(&str, f32)> = pairs.iter().map(|(n, x, y)| (*n, eucl(x, y))).collect();
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
