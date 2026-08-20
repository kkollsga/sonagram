use std::collections::{BTreeMap, BTreeSet};

use kglite::api::DirGraph;

use super::project::{project_tracks, TrackCandidate};
use super::types::{AggressionStatus, LibraryProfile, PlaylistPolicy, StatSummary};
use crate::Result;

pub fn profile_library(graph: &DirGraph) -> Result<LibraryProfile> {
    let tracks = project_tracks(graph)?;
    let default_policy = PlaylistPolicy::default();
    let artists: BTreeSet<&str> = tracks
        .iter()
        .map(|t| t.artist_key.as_str())
        .filter(|v| !v.is_empty())
        .collect();
    let albums: BTreeSet<&str> = tracks
        .iter()
        .map(|t| t.album_key.as_str())
        .filter(|v| !v.is_empty())
        .collect();
    let songs: BTreeSet<&str> = tracks
        .iter()
        .map(|t| t.song_key.as_deref().unwrap_or(t.id.as_str()))
        .collect();
    let styles: BTreeSet<&str> = tracks
        .iter()
        .flat_map(|t| t.style_keys.iter().map(String::as_str))
        .collect();
    let mut quality_tiers = BTreeMap::new();
    let mut aggression_models = BTreeMap::new();
    for track in &tracks {
        *quality_tiers
            .entry(
                track
                    .quality_tier
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
            )
            .or_insert(0) += 1;
        if let Some(model_id) = &track.aggression.model_id {
            *aggression_models.entry(model_id.clone()).or_insert(0) += 1;
        }
    }
    let mut stats = BTreeMap::new();
    for (name, values) in [
        ("duration_sec", values(&tracks, |t| t.duration_sec)),
        ("energy", values(&tracks, |t| t.energy)),
        ("arousal_index", values(&tracks, |t| t.arousal)),
        ("tension_index", values(&tracks, |t| t.tension)),
        ("valence_index", values(&tracks, |t| t.valence)),
        ("vocalness", values(&tracks, |t| t.vocalness)),
        (
            "aggression",
            values(&tracks, TrackCandidate::aggression_score),
        ),
        (
            "aggression_confidence",
            aggression_values(&tracks, |t| t.aggression.confidence),
        ),
        (
            "aggression_forcefulness",
            aggression_values(&tracks, |t| t.aggression.forcefulness),
        ),
        (
            "aggression_harshness",
            aggression_values(&tracks, |t| t.aggression.harshness),
        ),
        (
            "aggression_tension",
            aggression_values(&tracks, |t| t.aggression.tension),
        ),
        (
            "aggression_rhythm",
            aggression_values(&tracks, |t| t.aggression.rhythm),
        ),
        ("flow_smoothness", values(&tracks, |t| t.flow_smoothness)),
        (
            "recording_quality",
            values(&tracks, |t| t.recording_quality),
        ),
        ("popularity", values(&tracks, |t| t.popularity)),
    ] {
        stats.insert(name.to_string(), summarize(values, tracks.len()));
    }
    Ok(LibraryProfile {
        tracks: tracks.len(),
        music_tracks: tracks.iter().filter(|t| t.is_music).count(),
        canonical_tracks: tracks.iter().filter(|t| t.is_canonical).count(),
        eligible_default_tracks: tracks
            .iter()
            .filter(|t| super::audit::eligibility_issues(t, &default_policy).is_empty())
            .count(),
        unique_artists: artists.len(),
        unique_albums: albums.len(),
        unique_songs: songs.len(),
        unique_styles: styles.len(),
        quality_tiers,
        aggression_models,
        stats,
    })
}

fn aggression_values(
    tracks: &[TrackCandidate],
    get: impl Fn(&TrackCandidate) -> Option<f64>,
) -> Vec<f64> {
    tracks
        .iter()
        .filter(|track| {
            matches!(
                track.aggression.status,
                AggressionStatus::Available | AggressionStatus::Abstained
            )
        })
        .filter_map(get)
        .collect()
}

fn values(tracks: &[TrackCandidate], get: impl Fn(&TrackCandidate) -> Option<f64>) -> Vec<f64> {
    tracks
        .iter()
        .filter_map(get)
        .filter(|v| v.is_finite())
        .collect()
}

fn summarize(mut values: Vec<f64>, total: usize) -> StatSummary {
    values.sort_by(f64::total_cmp);
    let present = values.len();
    let mean = (!values.is_empty()).then(|| values.iter().sum::<f64>() / present as f64);
    StatSummary {
        present,
        total,
        mean,
        min: values.first().copied(),
        p25: percentile(&values, 0.25),
        median: percentile(&values, 0.50),
        p75: percentile(&values, 0.75),
        max: values.last().copied(),
    }
}

fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let pos = p.clamp(0.0, 1.0) * (values.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let weight = pos - lo as f64;
    Some(values[lo] * (1.0 - weight) + values[hi] * weight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_interpolates_and_empty_is_none() {
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(percentile(&[1.0, 3.0], 0.5), Some(2.0));
    }
}
