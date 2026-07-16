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

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

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
pub(super) const STYLE_MIN_SIZE: usize = 2;

/// Two tracks join the same style-community only along a `SIMILAR_TO` edge whose
/// `score` clears this bar. Connected components are single-linkage, so a low
/// bar lets a chain of merely-adjacent tracks collapse the whole library into
/// one blob (measured: the 15 diverse fixtures stay one component up to ~0.74).
/// Tuned to **0.75**, which fragments them into a handful (2–5) of tight
/// communities with the outliers left style-less. Raising it splits styles
/// further; lowering it merges them.
pub(super) const STYLE_SCORE_THRESHOLD: f64 = 0.75;

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

/// Add `Style` community nodes + `IN_STYLE` edges.
///
/// Communities are the connected components of the `SIMILAR_TO` graph restricted
/// to edges with `score >= STYLE_SCORE_THRESHOLD` (union-find over the sorted
/// edge list — order-independent). Components smaller than [`STYLE_MIN_SIZE`] are
/// dropped (no `Style` of one track). Each surviving component becomes one
/// `Style` carrying the schema doc's agent-readable profile, with
/// `unique_id = "style-<idx>"` where the index is assigned by
/// `(n_tracks desc, min member content_hash asc)` — stable across rebuilds.
/// `IN_STYLE` edges carry `membership = 1.0` (v1 hard assignment). Returns the
/// number of `Style` nodes created.
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

    let mut uf = UnionFind::new(hashes.len());
    for e in sim_edges {
        if e.score >= STYLE_SCORE_THRESHOLD {
            if let (Some(&a), Some(&b)) = (index_of.get(e.src.as_str()), index_of.get(e.tgt.as_str()))
            {
                uf.union(a, b);
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

    for (i, members) in comps.iter().enumerate() {
        let recs: Vec<&AnalysisRecord> = members.iter().map(|h| by_hash[*h]).collect();
        let p = profile(&recs);
        let id = format!("style-{i:0width$}");

        for h in members {
            let mut props = HashMap::new();
            props.insert("membership".to_string(), Value::Float64(1.0));
            edge_specs.push(edge_prop(TRACK, h, STYLE, &id, IN_STYLE, props));
        }

        ids.push(Some(id));
        names.push(Some(p.name));
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
/// `"<tempo-band>-<acoustic|electric>-<top-genre>"`.
///
/// Rule (documented so the golden is explainable):
/// - `<tempo-band>` = [`tempo_band`] of `mean_bpm` (e.g. `"house"`).
/// - `<acoustic|electric>` = `"acoustic"` if `mean_acousticness >= 0.5`, else
///   `"electric"`.
/// - `<top-genre>` = the #1 `top_genre` (by count, name-tiebroken), or `"mixed"`
///   when the community carries no genre tags.
fn style_name(mean_bpm: f64, mean_acousticness: f64, top_genre: Option<&str>) -> String {
    let band = tempo_band(mean_bpm as f32);
    let ae = if mean_acousticness >= 0.5 {
        "acoustic"
    } else {
        "electric"
    };
    let genre = top_genre.unwrap_or("mixed");
    format!("{band}-{ae}-{genre}")
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
    fn style_name_follows_the_template() {
        // house band (110–125), acoustic (>=0.5), top genre "folk".
        assert_eq!(style_name(118.0, 0.8, Some("folk")), "house-acoustic-folk");
        // upbeat, electric (<0.5), no genre → "mixed".
        assert_eq!(style_name(130.0, 0.2, None), "upbeat-electric-mixed");
        // boundary: acousticness exactly 0.5 → acoustic.
        assert_eq!(style_name(80.0, 0.5, Some("jazz")), "downtempo-acoustic-jazz");
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
