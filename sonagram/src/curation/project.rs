use std::collections::{BTreeMap, BTreeSet};

use kglite::api::cypher::resolve_node_property;
use kglite::api::{DirGraph, NodeData, Value};
use sonara::similarity::{SIMILARITY_SCALE, WEIGHTS};

use super::types::{AggressionEvidence, AggressionStatus};
use crate::{Result, SonagramError};

pub(crate) const TRACK: &str = "Track";

pub(crate) fn graph_build_input_fingerprint(graph: &DirGraph) -> Option<String> {
    let index = graph.type_indices.get("Library")?.to_vec().into_iter().next()?;
    let node = graph.get_node(index)?;
    prop_string(node, "build_input_fingerprint", graph)
}

#[derive(Debug, Clone)]
pub(crate) struct TrackCandidate {
    pub id: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artist_key: String,
    pub album: Option<String>,
    pub album_key: String,
    pub song_key: Option<String>,
    pub style_keys: Vec<String>,
    pub genre_keys: Vec<String>,
    pub decade_keys: Vec<String>,
    pub year: Option<i64>,
    pub duration_sec: Option<f64>,
    pub is_music: bool,
    pub is_canonical: bool,
    pub quality_tier: Option<String>,
    pub recording_quality: Option<f64>,
    pub popularity: Option<f64>,
    pub energy: Option<f64>,
    pub arousal: Option<f64>,
    pub tension: Option<f64>,
    pub valence: Option<f64>,
    pub vocalness: Option<f64>,
    pub aggression: AggressionEvidence,
    pub flow_smoothness: Option<f64>,
    pub bpm: Option<f64>,
    pub bpm_confidence: Option<f64>,
    pub camelot: Option<String>,
    pub embedding: Option<Vec<f32>>,
}

pub(crate) fn project_tracks(graph: &DirGraph) -> Result<Vec<TrackCandidate>> {
    let indices = graph
        .type_indices
        .get(TRACK)
        .map(|nodes| nodes.to_vec())
        .unwrap_or_default();
    let mut relations: BTreeMap<usize, TrackRelations> = BTreeMap::new();
    let stable = graph.graph.as_stable_digraph();
    for edge_idx in stable.edge_indices() {
        let Some(edge) = stable.edge_weight(edge_idx) else {
            continue;
        };
        let kind = edge.connection_type_str(&graph.interner);
        if kind != "VERSION_OF"
            && kind != "BY_ARTIST"
            && kind != "IN_STYLE"
            && kind != "IN_GENRE"
            && kind != "FROM_DECADE"
        {
            continue;
        }
        let Some((source, target)) = stable.edge_endpoints(edge_idx) else {
            continue;
        };
        let Some(source_node) = stable.node_weight(source) else {
            continue;
        };
        if source_node.node_type_str(&graph.interner) != TRACK {
            continue;
        }
        let Some(target_node) = stable.node_weight(target) else {
            continue;
        };
        let Some(target_id) = value_string(target_node.id().into_owned()) else {
            continue;
        };
        let entry = relations.entry(source.index()).or_default();
        match kind {
            "VERSION_OF" => entry.song_key = Some(target_id),
            "BY_ARTIST" => {
                if target_node.node_type_str(&graph.interner) == "Artist" {
                    let mbid = prop_string(target_node, "mbid", graph)
                        .map(|value| group_key(Some(value.as_str())))
                        .filter(|value| !value.is_empty());
                    if let Some(mbid) = mbid {
                        // A malformed graph may contain more than one BY_ARTIST edge.
                        // Keep the lexicographically first MBID so projection remains
                        // deterministic regardless of edge insertion order.
                        if entry.artist_mbid.as_ref().is_none_or(|current| mbid < *current) {
                            entry.artist_mbid = Some(mbid);
                        }
                    }
                }
            }
            "IN_STYLE" => {
                entry.styles.insert(group_key(Some(target_id.as_str())));
                if let Some(name) = prop_string(target_node, "name", graph) {
                    entry.styles.insert(group_key(Some(name.as_str())));
                }
            }
            "IN_GENRE" => {
                entry.genres.insert(group_key(Some(target_id.as_str())));
            }
            "FROM_DECADE" => {
                entry.decades.insert(group_key(Some(target_id.as_str())));
            }
            _ => {}
        }
    }

    let embedding_store = graph
        .embeddings
        .get(&(TRACK.to_string(), "similarity".to_string()));
    let mut out = Vec::with_capacity(indices.len());
    for idx in indices {
        let node = graph.get_node(idx).ok_or_else(|| {
            SonagramError::Graph(format!("Track index {} has no node", idx.index()))
        })?;
        let id = prop_string(node, "content_hash", graph)
            .or_else(|| value_string(node.id().into_owned()))
            .ok_or_else(|| SonagramError::Graph("Track has no string content_hash".into()))?;
        let artist = prop_string(node, "artist_name", graph);
        let album = prop_string(node, "album_name", graph);
        let relations = relations.remove(&idx.index()).unwrap_or_default();
        let year = prop_i64(node, "original_year", graph).or_else(|| prop_i64(node, "year", graph));
        let aggression = aggression_evidence(node, graph);
        out.push(TrackCandidate {
            id,
            title: prop_string(node, "title", graph),
            artist_key: relations.artist_mbid.unwrap_or_else(|| group_key(artist.as_deref())),
            artist,
            album_key: group_key(album.as_deref()),
            album,
            song_key: relations.song_key,
            style_keys: relations.styles.into_iter().collect(),
            genre_keys: relations.genres.into_iter().collect(),
            decade_keys: relations.decades.into_iter().collect(),
            year,
            duration_sec: prop_f64(node, "duration_sec", graph),
            is_music: prop_bool(node, "is_music", graph).unwrap_or(true),
            is_canonical: prop_bool(node, "is_canonical", graph).unwrap_or(true),
            quality_tier: prop_string(node, "quality_tier", graph),
            recording_quality: prop_f64(node, "recording_quality", graph),
            popularity: prop_f64(node, "popularity", graph),
            energy: prop_f64(node, "energy", graph),
            arousal: prop_f64(node, "arousal_index", graph),
            tension: prop_f64(node, "tension_index", graph),
            valence: prop_f64(node, "valence_index", graph),
            vocalness: prop_f64(node, "vocalness", graph),
            aggression,
            flow_smoothness: prop_f64(node, "flow_smoothness", graph),
            bpm: prop_f64(node, "bpm", graph),
            bpm_confidence: prop_f64(node, "bpm_confidence", graph),
            camelot: prop_string(node, "camelot", graph),
            embedding: embedding_store
                .and_then(|store| store.get_embedding(idx.index()))
                .map(ToOwned::to_owned),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

impl TrackCandidate {
    pub(crate) fn aggression_score(&self) -> Option<f64> {
        (self.aggression.status == AggressionStatus::Available)
            .then_some(self.aggression.score)
            .flatten()
    }
}

fn aggression_evidence(node: &NodeData, graph: &DirGraph) -> AggressionEvidence {
    let score = aggression_number(node, "aggression", graph);
    let confidence = aggression_number(node, "aggression_confidence", graph);
    let forcefulness = aggression_number(node, "aggression_forcefulness", graph);
    let harshness = aggression_number(node, "aggression_harshness", graph);
    let tension = aggression_number(node, "aggression_tension", graph);
    let rhythm = aggression_number(node, "aggression_rhythm", graph);
    let evidence = AggressionEvidence {
        status: AggressionStatus::Missing,
        model_id: prop_string(node, "aggression_model_id", graph),
        score: score.finite_value(),
        confidence: confidence.finite_value(),
        forcefulness: forcefulness.finite_value(),
        harshness: harshness.finite_value(),
        tension: tension.finite_value(),
        rhythm: rhythm.finite_value(),
    };
    let diagnostics = [confidence, forcefulness, harshness, tension, rhythm];
    let any_numeric_evidence =
        score.is_present() || diagnostics.iter().any(|value| value.is_present());
    let status = if evidence.model_id.is_none() && !any_numeric_evidence {
        AggressionStatus::Missing
    } else if evidence.model_id.as_deref() != Some(sonara::aggression::AGGRESSION_MODEL_ID) {
        AggressionStatus::IncompatibleModel
    } else if !diagnostics.into_iter().all(NumericEvidence::is_bounded) {
        AggressionStatus::InvalidDiagnostics
    } else {
        match score {
            NumericEvidence::Finite(score) if (0.0..=1.0).contains(&score) => {
                AggressionStatus::Available
            }
            NumericEvidence::Missing => AggressionStatus::Abstained,
            NumericEvidence::Finite(_) | NumericEvidence::Invalid => {
                AggressionStatus::InvalidDiagnostics
            }
        }
    };
    AggressionEvidence { status, ..evidence }
}

#[derive(Debug, Clone, Copy)]
enum NumericEvidence {
    Missing,
    Finite(f64),
    Invalid,
}

impl NumericEvidence {
    fn finite_value(self) -> Option<f64> {
        match self {
            Self::Finite(value) => Some(value),
            Self::Missing | Self::Invalid => None,
        }
    }

    fn is_present(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    fn is_bounded(self) -> bool {
        matches!(self, Self::Finite(value) if (0.0..=1.0).contains(&value))
    }
}

fn aggression_number(node: &NodeData, name: &str, graph: &DirGraph) -> NumericEvidence {
    match resolve_node_property(node, name, graph) {
        Value::Null => NumericEvidence::Missing,
        Value::Float64(value) if value.is_finite() => NumericEvidence::Finite(value),
        Value::Int64(value) => NumericEvidence::Finite(value as f64),
        _ => NumericEvidence::Invalid,
    }
}

#[derive(Default)]
struct TrackRelations {
    artist_mbid: Option<String>,
    song_key: Option<String>,
    styles: BTreeSet<String>,
    genres: BTreeSet<String>,
    decades: BTreeSet<String>,
}

pub(crate) fn candidate_map(graph: &DirGraph) -> Result<BTreeMap<String, TrackCandidate>> {
    Ok(project_tracks(graph)?.into_iter().map(|t| (t.id.clone(), t)).collect())
}

pub(crate) fn embedding_similarity(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != WEIGHTS.len() || b.len() != WEIGHTS.len() {
        return None;
    }
    let weight_sum: f64 = WEIGHTS.iter().map(|v| *v as f64).sum();
    if weight_sum <= 0.0 {
        return None;
    }
    let squared: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let delta = *x as f64 - *y as f64;
            delta * delta
        })
        .sum();
    let distance = (squared / weight_sum).clamp(0.0, 1.0).sqrt();
    Some((1.0 - distance / SIMILARITY_SCALE as f64).clamp(0.0, 1.0))
}

pub(crate) fn group_key(value: Option<&str>) -> String {
    value.map(str::trim).filter(|s| !s.is_empty()).map(str::to_lowercase).unwrap_or_default()
}

fn prop_string(node: &NodeData, name: &str, graph: &DirGraph) -> Option<String> {
    value_string(resolve_node_property(node, name, graph))
}

fn value_string(value: Value) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

fn prop_f64(node: &NodeData, name: &str, graph: &DirGraph) -> Option<f64> {
    match resolve_node_property(node, name, graph) {
        Value::Float64(value) if value.is_finite() => Some(value),
        Value::Int64(value) => Some(value as f64),
        _ => None,
    }
}

fn prop_i64(node: &NodeData, name: &str, graph: &DirGraph) -> Option<i64> {
    match resolve_node_property(node, name, graph) {
        Value::Int64(value) => Some(value),
        _ => None,
    }
}

fn prop_bool(node: &NodeData, name: &str, graph: &DirGraph) -> Option<bool> {
    match resolve_node_property(node, name, graph) {
        Value::Boolean(value) => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_preweighted_similarity_matches_sonara_metric() {
        let a: Vec<f32> = (0..WEIGHTS.len()).map(|i| i as f32 / WEIGHTS.len() as f32).collect();
        let b: Vec<f32> = a.iter().rev().copied().collect();
        let pre_a = crate::graph::preweight(&a);
        let pre_b = crate::graph::preweight(&b);
        let expected = sonara::similarity::similarity(&a, &b) as f64;
        let actual = embedding_similarity(&pre_a, &pre_b).unwrap();
        assert!((actual - expected).abs() < 1e-6, "actual={actual} expected={expected}");
    }
}
