//! Phase 6 derived structures: the three graph enrichments that turn the flat
//! P4/P5 projection into something an agent can traverse for similarity, harmonic
//! mixing, and style discovery. All three are **deterministic** — the same
//! records produce a byte-identical graph — which is what lets them live under
//! the golden gate.
//!
//! - [`add_similar_to`] — top-k=10 nearest-neighbour `SIMILAR_TO` edges per
//!   `Track`, ranked by the pre-weighted similarity embedding via kglite's
//!   `vector_search`, with the edge `score` computed by `sonara::similarity`.
//! - [`add_camelot_adjacent`] — the 72 static `CAMELOT_ADJACENT` edges of the
//!   Camelot wheel between the 24 `Key` nodes.
//! - [`add_styles`] — `Style` community nodes with agent-readable profiles.
//!
//! ## Why connected-components, not Leiden, for styles
//! The schema doc names Leiden as the style detector, but kglite's sealed public
//! API (`kglite::api::algorithms`) exposes `connected_components`,
//! `weakly_connected_components`, `louvain_communities`, and `label_propagation`
//! — **not** `leiden_communities` (it lives behind the `pub(crate) mod graph`
//! wall). Rather than reach past the sealed surface, and because P6's headline
//! requirement is *determinism* (CLAUDE.md: "the same library scanned twice must
//! produce a byte-identical graph"), styles are the documented deterministic
//! fallback: **connected components over the `SIMILAR_TO` graph filtered to
//! `score >= STYLE_SCORE_THRESHOLD`**. Components are computed here with a
//! union-find over the sorted edge list, so the result is order-independent and
//! reproducible by construction — no randomised refinement to verify. This
//! choice is reported to the PM for an upstream `notify` decision.
//!
//! ## P10b: mutual-kNN input + unique names
//! Plain connected components over the directed top-k `SIMILAR_TO` graph is
//! **single-linkage** and chains a diverse library into one mega-community. P10b
//! feeds the union-find a **mutual-kNN** edge set instead (pairs reciprocated in
//! both tracks' top-k — see [`mutual_pairs`]), then the score threshold, then the
//! same union-find. The directed `SIMILAR_TO` edges in the graph are unchanged.
//! Style names are also uniqued ([`unique_names`]) and the acoustic/electric term
//! is three-way (see [`style_name`]).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use kglite::api::algorithms::{vector_search, DistanceMetric, VectorSearchOptions};
use kglite::api::mutation::{add_edges_from_specs, EdgeSpec};
use kglite::api::{CurrentSelection, DirGraph, Value};
use kglite::datatypes::values::{ColumnData, ColumnType};
use sonara::similarity::{self, EMBEDDING_DIM};

use crate::record::AnalysisRecord;
use crate::{Result, SonagramError};

use super::normalize::{artist_id, filename_from_path, genre_id, tempo_band, KEYS};
use super::{
    add, build_df, preweight, CAMELOT_ADJACENT, EMBEDDING_PROPERTY, IN_STYLE, KEY, SIMILAR_TO,
    STYLE, TRACK,
};

/// How many neighbours to request from `vector_search` per track: `k + 1` so the
/// track's own zero-distance self-hit can be dropped, leaving `TOP_K`.
const K_QUERY: usize = 11;
/// Materialised `SIMILAR_TO` fan-out per track (schema doc: top-k = 10).
const TOP_K: usize = 10;

/// Minimum members for a connected component to become a `Style` node. Singletons
/// (and any component of one track) carry no `Style` — a "style" of one is not a
/// discovered pattern, and it would explode node count on a diverse library.
/// Kept at 2 after P10b tuning: on the 456-track subset the mutual-kNN edge set
/// yields 28 styles at 2 (no singleton-pair explosion), so raising to 3 only
/// discards small genuine communities.
pub(super) const STYLE_MIN_SIZE: usize = 2;

/// Two tracks join the same style-community only along a **mutual-kNN** edge (see
/// [`mutual_pairs`]) whose `score` clears this bar. Connected components are
/// single-linkage, so a low bar lets a chain of merely-adjacent tracks collapse
/// the whole library into one blob.
///
/// P10b tuning (measured on the frozen 456-track subset — see the permanent
/// `graph_derive::style_threshold_tuning` diagnostic): sonara 0.2.2's similarity
/// scores are compressed high, so the library's mega-community only fragments in
/// a sharp phase transition between ~0.84 and ~0.85. **0.85** is the lowest bar
/// that puts the largest community under 15% of tracks (measured: 456-track
/// subset → 28 styles, largest 8.1%, coverage 24.6%). Mutual-kNN symmetrization
/// alone does *not* break this blob (it is densely reciprocal — 70% at 0.75); the
/// threshold does the work, and mutual-kNN + this bar are identical here but keep
/// the edge set robust for less-compressed future distributions.
///
/// This bar is **calibrated to sonara 0.2.2's observed compressed score range**
/// and MUST be revisited when sonara recalibrates similarity. NOTE (P10b): the
/// 15 diverse committed fixtures top out at pair-score ~0.78, so at 0.85 they
/// produce **zero** styles — the fixture set and the real library have
/// non-overlapping score distributions (flagged to PM).
pub(super) const STYLE_SCORE_THRESHOLD: f64 = 0.85;

/// One materialised `SIMILAR_TO` edge, kept so [`add_styles`] can build
/// components from the same scored fan-out that was written to the graph.
pub(super) struct SimEdge {
    pub src: String,
    pub tgt: String,
    pub score: f64,
}

// ─────────────────────────────── SIMILAR_TO ─────────────────────────────────

/// Add directed `SIMILAR_TO` edges: for every `Track` (iterated in sorted
/// `content_hash` order), the [`TOP_K`] nearest other tracks by the pre-weighted
/// similarity embedding. Ranking is delegated to kglite's `vector_search`
/// (Euclidean over the pre-weighted vectors — provably the same order as
/// `sonara::similarity`), but the edge `score` property is computed directly by
/// `sonara::similarity::similarity` on the **raw** embeddings, so it is exactly
/// sonara's calibrated `[0, 1]` similarity regardless of the embedding-store
/// scale factor.
///
/// Determinism: candidates come from `type_indices` (sorted build order), and the
/// returned neighbours are re-sorted by `(score desc, target content_hash asc)`
/// before truncation, so ties resolve identically across runs and input
/// orderings. Self-loops are dropped. Returns the written edges for reuse.
pub(super) fn add_similar_to(
    graph: &mut DirGraph,
    sorted: &[&AnalysisRecord],
) -> Result<Vec<SimEdge>> {
    let track_nodes = graph
        .type_indices
        .get(TRACK)
        .map(|r| r.to_vec())
        .unwrap_or_default();
    if track_nodes.is_empty() {
        return Ok(Vec::new());
    }

    // node-index → content_hash, for mapping vector_search results back to ids.
    let mut idx_to_hash: BTreeMap<usize, String> = BTreeMap::new();
    for r in sorted {
        let h = r.source.content_hash.clone();
        if let Some(ni) = graph.lookup_by_id_readonly(TRACK, &Value::String(h.clone())) {
            idx_to_hash.insert(ni.index(), h);
        }
    }

    // content_hash → raw 48-dim embedding (score is computed on the raw vector).
    let mut raw: BTreeMap<&str, &Vec<f32>> = BTreeMap::new();
    for r in sorted {
        if let Some(e) = &r.analysis.embedding {
            if e.len() == EMBEDDING_DIM {
                raw.insert(r.source.content_hash.as_str(), e);
            }
        }
    }

    // One selection over every Track node, reused for every query.
    let mut selection = CurrentSelection::new();
    selection
        .get_level_mut(0)
        .expect("CurrentSelection::new seeds level 0")
        .add_selection(None, track_nodes);
    let opts = VectorSearchOptions::default()
        .with_top_k(K_QUERY)
        .with_metric(DistanceMetric::Euclidean)
        .with_exact(true);

    let mut sim_edges: Vec<SimEdge> = Vec::new();
    let mut specs: Vec<EdgeSpec> = Vec::new();
    for r in sorted {
        let self_hash = r.source.content_hash.as_str();
        let Some(self_raw) = raw.get(self_hash) else {
            continue; // no embedding → no outgoing SIMILAR_TO
        };
        let query = preweight(self_raw);
        let results = vector_search(graph, &selection, EMBEDDING_PROPERTY, &query, &opts)
            .map_err(SonagramError::Graph)?;

        let mut cands: Vec<(f64, String)> = Vec::with_capacity(results.len());
        for res in results {
            let Some(tgt_hash) = idx_to_hash.get(&res.node_idx.index()) else {
                continue;
            };
            if tgt_hash == self_hash {
                continue; // drop self-hit (distance 0, always rank 1)
            }
            let Some(tgt_raw) = raw.get(tgt_hash.as_str()) else {
                continue;
            };
            let score = similarity::similarity(self_raw, tgt_raw) as f64;
            cands.push((score, tgt_hash.clone()));
        }
        // Deterministic tiebreak: score desc, then target content_hash asc.
        cands.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        cands.truncate(TOP_K);

        for (score, tgt) in cands {
            let mut props = HashMap::new();
            props.insert("score".to_string(), Value::Float64(score));
            specs.push(edge_prop(TRACK, self_hash, TRACK, &tgt, SIMILAR_TO, props));
            sim_edges.push(SimEdge {
                src: self_hash.to_string(),
                tgt,
                score,
            });
        }
    }

    let report = add_edges_from_specs(graph, specs).map_err(SonagramError::Graph)?;
    if report.skipped_missing_endpoint != 0 {
        return Err(SonagramError::Graph(format!(
            "{} SIMILAR_TO edge(s) referenced a missing endpoint — a mapping bug",
            report.skipped_missing_endpoint
        )));
    }
    Ok(sim_edges)
}

// ──────────────────────────── CAMELOT_ADJACENT ──────────────────────────────

/// Add the static `CAMELOT_ADJACENT` edges of the Camelot wheel between the 24
/// `Key` nodes. Each key emits exactly three directed edges, distinguished by the
/// `transition` property:
/// - same letter, number **+1** (12 wraps to 1) → `"energy_up"`
/// - same letter, number **-1** (1 wraps to 12) → `"energy_down"`
/// - same number, **A↔B** → `"mode_switch"`
///
/// So 24 keys × 3 = **72** directed edges (24 `energy_up` + 24 `energy_down` +
/// 24 `mode_switch`; the up/down pairs are mutual inverses, the mode switches are
/// stored both ways). Generated from the static [`KEYS`] table in fixed order, so
/// the set is fully deterministic. Returns the edge count.
pub(super) fn add_camelot_adjacent(graph: &mut DirGraph) -> Result<usize> {
    // camelot code ("8A") → key name ("A minor"), so edges reference node ids.
    let by_code: BTreeMap<&str, &str> = KEYS.iter().map(|k| (k.camelot, k.name)).collect();

    let mut specs: Vec<EdgeSpec> = Vec::with_capacity(KEYS.len() * 3);
    for k in KEYS.iter() {
        let (num, letter) = parse_camelot(k.camelot);
        let up = format!("{}{}", if num == 12 { 1 } else { num + 1 }, letter);
        let down = format!("{}{}", if num == 1 { 12 } else { num - 1 }, letter);
        let other = if letter == 'A' { 'B' } else { 'A' };
        let switch = format!("{num}{other}");

        for (code, transition) in [
            (up, "energy_up"),
            (down, "energy_down"),
            (switch, "mode_switch"),
        ] {
            let tgt = by_code
                .get(code.as_str())
                .unwrap_or_else(|| panic!("camelot code {code} has no Key node"));
            let mut props = HashMap::new();
            props.insert("transition".to_string(), Value::String(transition.to_string()));
            specs.push(edge_prop(KEY, k.name, KEY, tgt, CAMELOT_ADJACENT, props));
        }
    }

    let n = specs.len();
    let report = add_edges_from_specs(graph, specs).map_err(SonagramError::Graph)?;
    if report.skipped_missing_endpoint != 0 {
        return Err(SonagramError::Graph(format!(
            "{} CAMELOT_ADJACENT edge(s) referenced a missing endpoint — a mapping bug",
            report.skipped_missing_endpoint
        )));
    }
    Ok(n)
}

/// Parse a Camelot code like `"8A"` / `"12B"` into `(number, letter)`.
fn parse_camelot(code: &str) -> (u32, char) {
    let letter = code.chars().last().expect("non-empty camelot code");
    let num: u32 = code[..code.len() - 1]
        .parse()
        .unwrap_or_else(|_| panic!("bad camelot number in {code}"));
    (num, letter)
}

// ─────────────────────────────── Style nodes ────────────────────────────────

/// The **mutual-kNN** edge set for style-community detection: the unordered
/// pairs `(a, b)` where `a` is in `b`'s top-k `SIMILAR_TO` fan-out **and** `b` is
/// in `a`'s. This is the P10b fix for single-linkage chaining — a track pulls the
/// whole library into one blob only along edges that are *not* reciprocated (one
/// track's 8th-nearest that does not return the favour); requiring reciprocity
/// prunes those bridges before the threshold is even applied.
///
/// `sim_edges` are the directed top-k edges already written as `SIMILAR_TO`, so
/// no new similarity is computed. Because `sonara::similarity` is symmetric, both
/// directed edges of a mutual pair carry the exact same `score`, returned once.
/// Deterministic: emitted in sorted `(src, tgt)` order with `src < tgt`.
///
/// Note this changes **only** the Style community input — the directed top-10
/// `SIMILAR_TO` edges in the graph are untouched.
pub(super) fn mutual_pairs(sim_edges: &[SimEdge]) -> Vec<(&str, &str, f64)> {
    let directed: BTreeSet<(&str, &str)> = sim_edges
        .iter()
        .map(|e| (e.src.as_str(), e.tgt.as_str()))
        .collect();
    let mut out: Vec<(&str, &str, f64)> = Vec::new();
    for e in sim_edges {
        let (a, b) = (e.src.as_str(), e.tgt.as_str());
        // Emit once (a < b) and only when the reverse edge is also present.
        if a < b && directed.contains(&(b, a)) {
            out.push((a, b, e.score));
        }
    }
    out
}

/// Add `Style` community nodes + `IN_STYLE` edges.
///
/// Communities are the connected components of the **mutual-kNN** graph (see
/// [`mutual_pairs`]) restricted to pairs with `score >= STYLE_SCORE_THRESHOLD`
/// (union-find over the sorted pair list — order-independent). Components smaller
/// than [`STYLE_MIN_SIZE`] are dropped (no `Style` of one track). Each surviving
/// component becomes one `Style` carrying the schema doc's agent-readable
/// profile, with `unique_id = "style-<idx>"` where the index is assigned by
/// `(n_tracks desc, min member content_hash asc)` — stable across rebuilds.
/// `IN_STYLE` edges carry `membership = 1.0` (v1 hard assignment). Returns the
/// number of `Style` nodes created.
///
/// Style **names** are made unique in the same index order (see [`unique_names`]):
/// the descriptive base name gets a `-2`, `-3`, … suffix on collision.
///
/// Degenerate cases are handled: every track its own component ⇒ no `Style`
/// nodes; all tracks one component ⇒ a single `Style`.
pub(super) fn add_styles(
    graph: &mut DirGraph,
    sorted: &[&AnalysisRecord],
    sim_edges: &[SimEdge],
) -> Result<usize> {
    if sorted.is_empty() {
        return Ok(0);
    }

    // hash → dense index in sorted order (union-find domain).
    let hashes: Vec<&str> = sorted.iter().map(|r| r.source.content_hash.as_str()).collect();
    let index_of: BTreeMap<&str, usize> =
        hashes.iter().enumerate().map(|(i, h)| (*h, i)).collect();
    let by_hash: BTreeMap<&str, &AnalysisRecord> =
        sorted.iter().map(|r| (r.source.content_hash.as_str(), *r)).collect();

    // Mutual-kNN symmetrization THEN threshold THEN union-find (P10b).
    let mut uf = UnionFind::new(hashes.len());
    for (a, b, score) in mutual_pairs(sim_edges) {
        if score >= STYLE_SCORE_THRESHOLD {
            if let (Some(&ia), Some(&ib)) = (index_of.get(a), index_of.get(b)) {
                uf.union(ia, ib);
            }
        }
    }

    // Group members by root, keeping each group sorted by content_hash.
    let mut groups: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    for (i, h) in hashes.iter().enumerate() {
        groups.entry(uf.find(i)).or_default().push(h);
    }
    let mut comps: Vec<Vec<&str>> = groups
        .into_values()
        .filter(|m| m.len() >= STYLE_MIN_SIZE)
        .collect();
    // Order: n_tracks desc, then min member hash asc (each group already sorted).
    comps.sort_by(|a, b| {
        b.len()
            .cmp(&a.len())
            .then_with(|| a[0].cmp(b[0]))
    });

    if comps.is_empty() {
        return Ok(0);
    }

    let width = comps.len().to_string().len().max(3);

    // Profile every component in the stable comps order, then unique the base
    // names in that same order (see `unique_names`) so a `-2`/`-3` suffix is
    // deterministic and collision-free.
    let profiles: Vec<StyleProfile> = comps
        .iter()
        .map(|members| profile(&members.iter().map(|h| by_hash[*h]).collect::<Vec<_>>()))
        .collect();
    let unique = unique_names(profiles.iter().map(|p| p.name.as_str()));

    // Build the Style node table + IN_STYLE edges.
    let mut ids: Vec<Option<String>> = Vec::new();
    let mut names: Vec<Option<String>> = Vec::new();
    let mut mean_bpm: Vec<Option<f64>> = Vec::new();
    let mut mean_energy: Vec<Option<f64>> = Vec::new();
    let mut mean_valence: Vec<Option<f64>> = Vec::new();
    let mut mean_acoustic: Vec<Option<f64>> = Vec::new();
    let mut n_tracks: Vec<Option<i64>> = Vec::new();
    let mut top_genres: Vec<Option<Vec<Value>>> = Vec::new();
    let mut top_artists: Vec<Option<Vec<Value>>> = Vec::new();
    let mut exemplars_col: Vec<Option<Vec<Value>>> = Vec::new();
    let mut edge_specs: Vec<EdgeSpec> = Vec::new();

    for (i, (members, p)) in comps.iter().zip(profiles.iter()).enumerate() {
        let id = format!("style-{i:0width$}");

        for h in members {
            let mut props = HashMap::new();
            props.insert("membership".to_string(), Value::Float64(1.0));
            edge_specs.push(edge_prop(TRACK, h, STYLE, &id, IN_STYLE, props));
        }

        ids.push(Some(id));
        names.push(Some(unique[i].clone()));
        mean_bpm.push(Some(p.mean_bpm));
        mean_energy.push(Some(p.mean_energy));
        mean_valence.push(Some(p.mean_valence));
        mean_acoustic.push(Some(p.mean_acousticness));
        n_tracks.push(Some(p.n_tracks));
        top_genres.push(Some(str_list(&p.top_genres)));
        top_artists.push(Some(str_list(&p.top_artists)));
        exemplars_col.push(Some(str_list(&p.exemplar_titles)));
    }

    let df = build_df(vec![
        ("unique_id", ColumnType::String, ColumnData::String(ids)),
        ("name", ColumnType::String, ColumnData::String(names)),
        ("mean_bpm", ColumnType::Float64, ColumnData::Float64(mean_bpm)),
        ("mean_energy", ColumnType::Float64, ColumnData::Float64(mean_energy)),
        ("mean_valence", ColumnType::Float64, ColumnData::Float64(mean_valence)),
        ("mean_acousticness", ColumnType::Float64, ColumnData::Float64(mean_acoustic)),
        ("n_tracks", ColumnType::Int64, ColumnData::Int64(n_tracks)),
        ("top_genres", ColumnType::List, ColumnData::List(top_genres)),
        ("top_artists", ColumnType::List, ColumnData::List(top_artists)),
        ("exemplar_titles", ColumnType::List, ColumnData::List(exemplars_col)),
    ]);
    let n_styles = comps.len();
    add(graph, df, STYLE, "unique_id", "name")?;

    let report = add_edges_from_specs(graph, edge_specs).map_err(SonagramError::Graph)?;
    if report.skipped_missing_endpoint != 0 {
        return Err(SonagramError::Graph(format!(
            "{} IN_STYLE edge(s) referenced a missing endpoint — a mapping bug",
            report.skipped_missing_endpoint
        )));
    }
    Ok(n_styles)
}

/// The agent-readable profile of one style community.
struct StyleProfile {
    name: String,
    mean_bpm: f64,
    mean_energy: f64,
    mean_valence: f64,
    mean_acousticness: f64,
    top_genres: Vec<String>,
    top_artists: Vec<String>,
    n_tracks: i64,
    exemplar_titles: Vec<String>,
}

/// Compute a [`StyleProfile`] from a component's member records.
fn profile(members: &[&AnalysisRecord]) -> StyleProfile {
    let mean_bpm = mean(members.iter().map(|r| Some(r.analysis.bpm as f64)));
    let mean_energy = mean(members.iter().map(|r| r.analysis.energy.map(|v| v as f64)));
    let mean_valence = mean(members.iter().map(|r| r.analysis.valence.map(|v| v as f64)));
    let mean_acoustic = mean(members.iter().map(|r| r.analysis.acousticness.map(|v| v as f64)));

    let top_genres = top_counts(
        members
            .iter()
            .filter_map(|r| genre_id(r.tags.as_ref().and_then(|t| t.genre.as_deref()))),
        3,
    );
    let top_artists = top_counts(
        members
            .iter()
            .map(|r| artist_id(r.tags.as_ref().and_then(|t| t.artist.as_deref()))),
        3,
    );

    // Canonical-length member embeddings, and the (hash, title, embedding)
    // tuples the exemplar ranker consumes — kept minimal so both helpers are
    // pure and unit-testable without fabricating whole records.
    let embs: Vec<&[f32]> = members
        .iter()
        .filter_map(|r| {
            r.analysis
                .embedding
                .as_deref()
                .filter(|e| e.len() == EMBEDDING_DIM)
        })
        .collect();
    let centroid = centroid(&embs);
    let ex_inputs: Vec<(&str, String, Option<&[f32]>)> = members
        .iter()
        .map(|r| {
            (
                r.source.content_hash.as_str(),
                track_title(r),
                r.analysis
                    .embedding
                    .as_deref()
                    .filter(|e| e.len() == EMBEDDING_DIM),
            )
        })
        .collect();
    let exemplar_titles = exemplars(&ex_inputs, centroid.as_deref(), 5);

    let name = style_name(mean_bpm, mean_acoustic, top_genres.first().map(String::as_str));

    StyleProfile {
        name,
        mean_bpm,
        mean_energy,
        mean_valence,
        mean_acousticness: mean_acoustic,
        top_genres,
        top_artists,
        n_tracks: members.len() as i64,
        exemplar_titles,
    }
}

/// Derive a deterministic style name from the profile, per the schema doc:
/// `"<tempo-band>-[acoustic|electric-]<top-genre>"`.
///
/// Rule (documented so the golden is explainable):
/// - `<tempo-band>` = [`tempo_band`] of `mean_bpm` (e.g. `"house"`).
/// - acoustic/electric term is **three-way** with distinctive cutoffs (P10b):
///   `"acoustic"` if `mean_acousticness >= 0.70`, `"electric"` if `<= 0.50`, else
///   the term is **omitted entirely** (e.g. `"house-pop"`). The old 0.5 split
///   named ~84% of tracks "acoustic" because sonara 0.2.2's `acousticness` is
///   compressed to roughly `[0.42, 0.93]` (avg ~0.63) — these cutoffs are
///   **calibrated to that observed compressed range** and MUST be revisited when
///   sonara recalibrates.
/// - `<top-genre>` = the #1 `top_genre` (by count, name-tiebroken), or `"mixed"`
///   when the community carries no genre tags.
fn style_name(mean_bpm: f64, mean_acousticness: f64, top_genre: Option<&str>) -> String {
    let band = tempo_band(mean_bpm as f32);
    let genre = top_genre.unwrap_or("mixed");
    match acoustic_term(mean_acousticness) {
        Some(ae) => format!("{band}-{ae}-{genre}"),
        None => format!("{band}-{genre}"),
    }
}

/// The three-way acoustic/electric term, or `None` when the middle band omits it.
/// Cutoffs calibrated to sonara 0.2.2's compressed `acousticness` range (P10b).
fn acoustic_term(mean_acousticness: f64) -> Option<&'static str> {
    if mean_acousticness >= 0.70 {
        Some("acoustic")
    } else if mean_acousticness <= 0.50 {
        Some("electric")
    } else {
        None
    }
}

/// Make the ordered base style names unique: the first occurrence of a name is
/// kept verbatim, later collisions get a `-2`, `-3`, … suffix in the same order
/// (which is the stable `(n_tracks desc, min-hash asc)` style-index order). The
/// suffixed form is itself checked against the taken set so a base name that
/// already ends in a number can never alias a generated one.
fn unique_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for base in names {
        let n = seen.entry(base.to_string()).or_insert(0);
        *n += 1;
        let name = if *n == 1 {
            base.to_string()
        } else {
            // Advance the suffix until it is genuinely free.
            let mut k = *n;
            loop {
                let candidate = format!("{base}-{k}");
                if !taken.contains(&candidate) {
                    break candidate;
                }
                k += 1;
            }
        };
        taken.insert(name.clone());
        out.push(name);
    }
    out
}

/// Element-wise mean of the given raw embeddings (each [`EMBEDDING_DIM`] long).
/// `None` if no embedding is supplied.
fn centroid(embeddings: &[&[f32]]) -> Option<Vec<f32>> {
    if embeddings.is_empty() {
        return None;
    }
    let mut acc = vec![0.0f64; EMBEDDING_DIM];
    for e in embeddings {
        for (a, v) in acc.iter_mut().zip(e.iter()) {
            *a += *v as f64;
        }
    }
    let n = embeddings.len() as f64;
    Some(acc.iter().map(|a| (*a / n) as f32).collect())
}

/// The `k` member titles nearest the feature centroid (by `sonara::distance`),
/// tiebroken by content_hash so the list is deterministic. Members without an
/// embedding (or when there is no centroid) rank last (infinite distance), still
/// hash-ordered among themselves.
fn exemplars(
    members: &[(&str, String, Option<&[f32]>)],
    centroid: Option<&[f32]>,
    k: usize,
) -> Vec<String> {
    let mut ranked: Vec<(f64, &str, &str)> = members
        .iter()
        .map(|(hash, title, emb)| {
            let dist = match (centroid, emb) {
                (Some(c), Some(e)) => similarity::distance(e, c) as f64,
                _ => f64::INFINITY,
            };
            (dist, *hash, title.as_str())
        })
        .collect();
    ranked.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.1.cmp(b.1))
    });
    ranked.into_iter().take(k).map(|(_, _, t)| t.to_string()).collect()
}

/// The display title for a track — tag title, else the file name (same rule the
/// P4 `Track` builder uses).
fn track_title(r: &AnalysisRecord) -> String {
    r.tags
        .as_ref()
        .and_then(|t| t.title.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| filename_from_path(&r.source.path))
}

/// Mean over the present (`Some`) values of an iterator; `0.0` when all absent.
fn mean(vals: impl Iterator<Item = Option<f64>>) -> f64 {
    let mut sum = 0.0;
    let mut n = 0usize;
    for v in vals.flatten() {
        sum += v;
        n += 1;
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f64
    }
}

/// Up to `k` most-frequent items, ordered by `(count desc, name asc)`.
fn top_counts(items: impl Iterator<Item = String>, k: usize) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for it in items {
        *counts.entry(it).or_insert(0) += 1;
    }
    let mut ranked: Vec<(usize, String)> = counts.into_iter().map(|(name, c)| (c, name)).collect();
    // count desc, then name asc (BTreeMap already gives name-sorted input, but be
    // explicit so the tiebreak is self-evident).
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    ranked.into_iter().take(k).map(|(_, name)| name).collect()
}

/// A `Vec<String>` as a `Vec<Value::String>` for a `ColumnData::List` cell.
fn str_list(items: &[String]) -> Vec<Value> {
    items.iter().map(|s| Value::String(s.clone())).collect()
}

// ───────────────────────────────── helpers ──────────────────────────────────

/// Build an [`EdgeSpec`] carrying `properties`.
fn edge_prop(
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

/// A minimal union-find (disjoint-set) over `0..n`, path-compressed and
/// union-by-size — used to compute `SIMILAR_TO` connected components
/// deterministically (order-independent given the same edge set).
struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
            size: vec![1; n],
        }
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
        // Union by size; ties broken toward the smaller root index so the
        // representative is deterministic (components are re-grouped by member
        // hash afterwards, so this only affects internal bookkeeping).
        let (big, small) = if self.size[ra] >= self.size[rb] {
            (ra, rb)
        } else {
            (rb, ra)
        };
        self.parent[small] = big;
        self.size[big] += self.size[small];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelot_parses_number_and_letter() {
        assert_eq!(parse_camelot("8A"), (8, 'A'));
        assert_eq!(parse_camelot("12B"), (12, 'B'));
        assert_eq!(parse_camelot("1A"), (1, 'A'));
    }

    #[test]
    fn style_name_follows_the_three_way_template() {
        // house band (110–125), acoustic (>=0.70), top genre "folk".
        assert_eq!(style_name(118.0, 0.8, Some("folk")), "house-acoustic-folk");
        // upbeat, electric (<=0.50), no genre → "mixed".
        assert_eq!(style_name(130.0, 0.2, None), "upbeat-electric-mixed");
        // P10b: mid band (0.50, 0.70) omits the acoustic/electric term entirely.
        assert_eq!(style_name(100.0, 0.60, Some("pop")), "mid-pop");
        // P10b boundary: exactly 0.70 → acoustic; exactly 0.50 → electric.
        assert_eq!(style_name(80.0, 0.70, Some("jazz")), "downtempo-acoustic-jazz");
        assert_eq!(style_name(80.0, 0.50, Some("jazz")), "downtempo-electric-jazz");
    }

    #[test]
    fn acoustic_term_is_three_way_with_calibrated_cutoffs() {
        assert_eq!(acoustic_term(0.93), Some("acoustic"));
        assert_eq!(acoustic_term(0.70), Some("acoustic")); // inclusive high cutoff
        assert_eq!(acoustic_term(0.69), None); // just inside the omitted middle
        assert_eq!(acoustic_term(0.60), None);
        assert_eq!(acoustic_term(0.51), None);
        assert_eq!(acoustic_term(0.50), Some("electric")); // inclusive low cutoff
        assert_eq!(acoustic_term(0.42), Some("electric"));
    }

    #[test]
    fn unique_names_suffixes_collisions_in_order() {
        // First keeps the base name; 2nd/3rd get -2/-3 in style-index order.
        let got = unique_names(
            ["house-acoustic-pop", "house-acoustic-pop", "downtempo-folk", "house-acoustic-pop"]
                .into_iter(),
        );
        assert_eq!(
            got,
            vec![
                "house-acoustic-pop".to_string(),
                "house-acoustic-pop-2".to_string(),
                "downtempo-folk".to_string(),
                "house-acoustic-pop-3".to_string(),
            ]
        );
    }

    #[test]
    fn unique_names_avoids_aliasing_a_pre_numbered_base() {
        // A base already ending "-2" must not be aliased by the generated suffix.
        let got = unique_names(["mid-pop", "mid-pop-2", "mid-pop"].into_iter());
        // 1st "mid-pop" verbatim; "mid-pop-2" verbatim; 2nd "mid-pop" wants "-2"
        // but it is taken, so it advances to "-3".
        assert_eq!(
            got,
            vec![
                "mid-pop".to_string(),
                "mid-pop-2".to_string(),
                "mid-pop-3".to_string(),
            ]
        );
    }

    #[test]
    fn mutual_pairs_keep_only_reciprocated_edges() {
        // a<->b reciprocated; a->c one-way (c never returns a); b<->d reciprocated.
        let edges = vec![
            SimEdge { src: "a".into(), tgt: "b".into(), score: 0.9 },
            SimEdge { src: "b".into(), tgt: "a".into(), score: 0.9 },
            SimEdge { src: "a".into(), tgt: "c".into(), score: 0.8 },
            SimEdge { src: "b".into(), tgt: "d".into(), score: 0.7 },
            SimEdge { src: "d".into(), tgt: "b".into(), score: 0.7 },
        ];
        let got = mutual_pairs(&edges);
        // Only (a,b) and (b,d) survive; a->c is excluded (one-way); emitted once
        // per pair with src < tgt.
        assert_eq!(got, vec![("a", "b", 0.9), ("b", "d", 0.7)]);
    }

    #[test]
    fn top_counts_orders_by_count_then_name() {
        let items = ["pop", "rock", "pop", "jazz", "rock", "pop"]
            .into_iter()
            .map(String::from);
        assert_eq!(top_counts(items, 3), vec!["pop", "rock", "jazz"]);

        // Count tie resolves by name ascending; k truncates.
        let tie = ["b", "a", "c"].into_iter().map(String::from);
        assert_eq!(top_counts(tie, 2), vec!["a", "b"]);
    }

    #[test]
    fn mean_skips_absent_and_defaults_zero() {
        assert_eq!(mean([Some(2.0), None, Some(4.0)].into_iter()), 3.0);
        assert_eq!(mean([None, None].into_iter()), 0.0);
        assert!(mean(std::iter::empty()) == 0.0);
    }

    #[test]
    fn centroid_is_elementwise_mean() {
        let zeros = vec![0.0f32; EMBEDDING_DIM];
        let ones = vec![1.0f32; EMBEDDING_DIM];
        let c = centroid(&[zeros.as_slice(), ones.as_slice()]).expect("centroid present");
        assert_eq!(c.len(), EMBEDDING_DIM);
        for v in c {
            assert!((v - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn centroid_none_without_embeddings() {
        assert!(centroid(&[]).is_none());
    }

    #[test]
    fn exemplars_rank_by_distance_then_hash() {
        // Centroid at 0.5; members at increasing distance (0.5, 0.6, 0.9).
        let near = vec![0.5f32; EMBEDDING_DIM];
        let mid = vec![0.6f32; EMBEDDING_DIM];
        let far = vec![0.9f32; EMBEDDING_DIM];
        let centroid = vec![0.5f32; EMBEDDING_DIM];
        // Passed out of distance order to prove the sort, not the input order.
        let members = vec![
            ("aa-far", "Far".to_string(), Some(far.as_slice())),
            ("cc-near", "Near".to_string(), Some(near.as_slice())),
            ("bb-mid", "Mid".to_string(), Some(mid.as_slice())),
        ];
        assert_eq!(exemplars(&members, Some(&centroid), 5), vec!["Near", "Mid", "Far"]);
        // k truncates to the nearest two.
        assert_eq!(exemplars(&members, Some(&centroid), 2), vec!["Near", "Mid"]);
    }

    #[test]
    fn exemplars_tiebreak_on_hash_when_equidistant() {
        // Two members at identical distance → hash ascending decides order.
        let v = vec![0.4f32; EMBEDDING_DIM];
        let centroid = vec![0.5f32; EMBEDDING_DIM];
        let members = vec![
            ("zzz", "Zed".to_string(), Some(v.as_slice())),
            ("aaa", "Ann".to_string(), Some(v.as_slice())),
        ];
        assert_eq!(exemplars(&members, Some(&centroid), 5), vec!["Ann", "Zed"]);
    }

    #[test]
    fn exemplars_without_centroid_fall_to_hash_order() {
        let members = vec![
            ("bbb", "Bee".to_string(), None),
            ("aaa", "Ann".to_string(), None),
        ];
        assert_eq!(exemplars(&members, None, 5), vec!["Ann", "Bee"]);
    }

    #[test]
    fn union_find_groups_connected_nodes() {
        let mut uf = UnionFind::new(5);
        uf.union(0, 1);
        uf.union(1, 2);
        uf.union(3, 4);
        assert_eq!(uf.find(0), uf.find(2));
        assert_ne!(uf.find(0), uf.find(3));
        assert_eq!(uf.find(3), uf.find(4));
    }
}
