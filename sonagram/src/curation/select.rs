use std::collections::{BTreeMap, BTreeSet};
use std::cmp::Ordering;

use kglite::api::DirGraph;

use super::audit::{audit_playlist, eligibility_issues, explain_playlist};
use super::project::{embedding_similarity, project_tracks, TrackCandidate};
use super::types::{
    AuditIssue, AuditSeverity, CuratedPlaylist, FamiliarityPreference, PlaylistBrief,
    PlaylistPolicy,
};
use crate::Result;

pub fn curate_playlist(
    graph: &DirGraph,
    brief: &PlaylistBrief,
    policy: &PlaylistPolicy,
) -> Result<CuratedPlaylist> {
    let tracks = project_tracks(graph)?;
    let by_id: BTreeMap<&str, &TrackCandidate> = tracks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut selected: Vec<&TrackCandidate> = Vec::new();
    let mut selected_ids: BTreeSet<&str> = BTreeSet::new();
    let mut artist_counts = BTreeMap::new();
    let mut album_counts = BTreeMap::new();
    let mut song_counts = BTreeMap::new();
    let mut selection_issues = Vec::new();
    let artist_cap = effective_cap(
        policy.diversity.max_per_artist,
        policy.audit.max_artist_share,
        brief.target_tracks,
    );
    let album_cap = effective_cap(
        policy.diversity.max_per_album,
        policy.audit.max_album_share,
        brief.target_tracks,
    );

    if brief.target_tracks == 0 {
        selection_issues.push(error("invalid_target", "target_tracks must be greater than zero"));
    }
    if brief.seed_ids.len() > brief.target_tracks {
        selection_issues.push(error(
            "too_many_seeds",
            format!(
                "{} seeds exceed the target of {} tracks",
                brief.seed_ids.len(), brief.target_tracks
            ),
        ));
    }
    if brief.preset != policy.preset {
        selection_issues.push(error(
            "preset_mismatch",
            format!(
                "brief preset {:?} does not match resolved policy preset {:?}",
                brief.preset, policy.preset
            ),
        ));
    }
    if policy.diversity.max_per_artist == 0
        || policy.diversity.max_per_album == 0
        || policy.diversity.max_per_song == 0
    {
        selection_issues.push(error(
            "invalid_policy",
            "diversity caps must be greater than zero",
        ));
    }

    for id in &brief.seed_ids {
        if !selected_ids.insert(id) {
            selection_issues.push(error("duplicate_seed", format!("seed {id} occurs more than once")));
            continue;
        }
        let Some(track) = by_id.get(id.as_str()).copied() else {
            selection_issues.push(error("missing_seed", format!("seed {id} is not in the graph")));
            continue;
        };
        let eligibility = eligibility_issues(track, policy);
        if !eligibility.is_empty() {
            selection_issues.push(error(
                "ineligible_seed",
                format!("seed {id} violates policy: {}", eligibility[0].1),
            ));
            continue;
        }
        if !within_caps(
            track,
            artist_cap,
            album_cap,
            policy.diversity.max_per_song,
            &artist_counts,
            &album_counts,
            &song_counts,
        ) {
            selection_issues.push(error(
                "seed_diversity_conflict",
                format!("seed {id} exceeds a diversity cap"),
            ));
            continue;
        }
        increment_counts(track, &mut artist_counts, &mut album_counts, &mut song_counts);
        selected.push(track);
    }
    let seed_count = selected.len();

    let eligible: Vec<&TrackCandidate> = tracks
        .iter()
        .filter(|t| eligibility_issues(t, policy).is_empty())
        .collect();
    while selected.len() < brief.target_tracks {
        let best = eligible
            .iter()
            .copied()
            .filter(|track| !selected_ids.contains(track.id.as_str()))
            .filter(|track| {
                within_caps(
                    track,
                    artist_cap,
                    album_cap,
                    policy.diversity.max_per_song,
                    &artist_counts,
                    &album_counts,
                    &song_counts,
                )
            })
            .map(|track| {
                (
                    selection_score(track, &selected, brief, policy),
                    track,
                )
            })
            .max_by(|(a_score, a), (b_score, b)| {
                compare_rank(*a_score, &a.id, *b_score, &b.id)
            })
            .map(|(_, track)| track);
        let Some(track) = best else {
            break;
        };
        selected_ids.insert(track.id.as_str());
        increment_counts(track, &mut artist_counts, &mut album_counts, &mut song_counts);
        selected.push(track);
    }

    let mut duration_fallback_used = false;
    if let Some(target_duration) = brief.target_duration_sec {
        let selected_duration: f64 = selected.iter().filter_map(|track| track.duration_sec).sum();
        if selected_duration + f64::EPSILON < target_duration as f64 {
            let fallback = duration_first_selection(
                &eligible,
                &selected[..seed_count],
                brief.target_tracks,
                artist_cap,
                album_cap,
                policy.diversity.max_per_song,
            );
            let fallback_duration: f64 =
                fallback.iter().filter_map(|track| track.duration_sec).sum();
            if fallback.len() == brief.target_tracks
                && fallback_duration + f64::EPSILON >= target_duration as f64
            {
                selected = fallback;
                duration_fallback_used = true;
            }
        }
    }

    let track_ids: Vec<String> = selected.iter().map(|t| t.id.clone()).collect();
    let mut audit = audit_playlist(graph, &track_ids, policy)?;
    if track_ids.len() < brief.target_tracks {
        selection_issues.push(error(
            "infeasible_selection",
            format!(
                "policy can supply only {} of {} requested tracks without relaxing hard constraints",
                track_ids.len(), brief.target_tracks
            ),
        ));
    }
    if let Some(target_duration) = brief.target_duration_sec {
        if audit.total_duration_sec + f64::EPSILON < target_duration as f64 {
            selection_issues.push(error(
                "duration_shortfall",
                format!(
                    "selected duration {:.0}s is below requested {target_duration}s",
                    audit.total_duration_sec
                ),
            ));
        }
    }
    audit.issues.extend(selection_issues);
    audit.passed = !audit.issues.iter().any(|issue| issue.severity == AuditSeverity::Error);
    let mut explanation = explain_playlist(graph, &track_ids, policy)?;
    explanation.summary = vec![format!(
        "{} tracks, {} artists, audit {}",
        audit.track_count,
        audit.unique_artists,
        if audit.passed { "passed" } else { "failed" }
    )];
    explanation.summary.extend(
        audit
            .issues
            .iter()
            .map(|issue| format!("{}: {}", issue.code, issue.message)),
    );
    explanation.summary.push(format!(
        "deterministic constrained selection chose {} of {} requested tracks",
        track_ids.len(), brief.target_tracks
    ));
    if duration_fallback_used {
        explanation
            .summary
            .push("duration-first fallback satisfied the requested minimum".into());
    }
    Ok(CuratedPlaylist {
        exportable: audit.passed && track_ids.len() == brief.target_tracks,
        track_ids,
        brief: brief.clone(),
        policy: policy.clone(),
        audit,
        explanation,
        repair_attempts: usize::from(duration_fallback_used),
    })
}

fn duration_first_selection<'a>(
    eligible: &[&'a TrackCandidate],
    seeds: &[&'a TrackCandidate],
    target_tracks: usize,
    artist_cap: usize,
    album_cap: usize,
    song_cap: usize,
) -> Vec<&'a TrackCandidate> {
    let mut selected = seeds.to_vec();
    let mut selected_ids: BTreeSet<&str> = seeds.iter().map(|track| track.id.as_str()).collect();
    let mut artists = BTreeMap::new();
    let mut albums = BTreeMap::new();
    let mut songs = BTreeMap::new();
    for track in seeds {
        increment_counts(track, &mut artists, &mut albums, &mut songs);
    }
    let mut candidates = eligible.to_vec();
    candidates.sort_by(|a, b| {
        b.duration_sec
            .unwrap_or(0.0)
            .total_cmp(&a.duration_sec.unwrap_or(0.0))
            .then_with(|| a.id.cmp(&b.id))
    });
    for track in candidates {
        if selected.len() == target_tracks {
            break;
        }
        if selected_ids.contains(track.id.as_str())
            || !within_caps(
                track,
                artist_cap,
                album_cap,
                song_cap,
                &artists,
                &albums,
                &songs,
            )
        {
            continue;
        }
        selected_ids.insert(track.id.as_str());
        increment_counts(track, &mut artists, &mut albums, &mut songs);
        selected.push(track);
    }
    selected
}

fn selection_score(
    track: &TrackCandidate,
    selected: &[&TrackCandidate],
    brief: &PlaylistBrief,
    policy: &PlaylistPolicy,
) -> f64 {
    let fits = [
        fit(track.energy, policy.targets.energy),
        fit(track.arousal, policy.targets.arousal),
        fit(track.tension, policy.targets.tension),
        fit(track.vocalness, policy.targets.vocalness),
    ];
    let (fit_sum, fit_count) = fits
        .into_iter()
        .flatten()
        .fold((0.0, 0usize), |(sum, count), value| (sum + value, count + 1));
    let relevance = if fit_count == 0 { 0.5 } else { fit_sum / fit_count as f64 };
    let quality = track.recording_quality.unwrap_or(0.5).clamp(0.0, 1.0);
    let popularity = track.popularity.unwrap_or(0.5).clamp(0.0, 1.0);
    let familiarity = match policy.targets.familiarity {
        FamiliarityPreference::Avoid => 1.0 - popularity,
        FamiliarityPreference::Neutral => 0.5,
        FamiliarityPreference::Prefer => popularity,
    };
    let diversity = if selected.is_empty() {
        1.0
    } else {
        let artist_new = !track.artist_key.is_empty()
            && selected.iter().all(|other| other.artist_key != track.artist_key);
        let album_new = !track.album_key.is_empty()
            && selected.iter().all(|other| other.album_key != track.album_key);
        let song = track.song_key.as_deref().unwrap_or(track.id.as_str());
        let song_new = selected
            .iter()
            .all(|other| other.song_key.as_deref().unwrap_or(other.id.as_str()) != song);
        [artist_new, album_new, song_new]
            .into_iter()
            .filter(|value| *value)
            .count() as f64
            / 3.0
    };
    let novelty = selected
        .iter()
        .filter_map(|other| {
            track
                .embedding
                .as_deref()
                .zip(other.embedding.as_deref())
                .and_then(|(a, b)| embedding_similarity(a, b))
        })
        .max_by(f64::total_cmp)
        .map(|similarity| 1.0 - similarity)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    let duration_fit = brief
        .target_duration_sec
        .and_then(|target| {
            let selected_duration: f64 = selected.iter().filter_map(|item| item.duration_sec).sum();
            let remaining_slots = brief.target_tracks.saturating_sub(selected.len());
            (remaining_slots > 0).then(|| {
                let needed = (target as f64 - selected_duration).max(0.0) / remaining_slots as f64;
                if needed <= f64::EPSILON {
                    0.5
                } else {
                    (track.duration_sec.unwrap_or(0.0) / needed).clamp(0.0, 1.0)
                }
            })
        })
        .unwrap_or(0.5);
    0.50 * relevance
        + 0.20 * quality
        + 0.10 * familiarity
        + 0.10 * diversity
        + 0.05 * novelty
        + 0.05 * duration_fit
}

fn within_caps(
    track: &TrackCandidate,
    artist_cap: usize,
    album_cap: usize,
    song_cap: usize,
    artists: &BTreeMap<String, usize>,
    albums: &BTreeMap<String, usize>,
    songs: &BTreeMap<String, usize>,
) -> bool {
    below_cap(&track.artist_key, artists, artist_cap)
        && below_cap(&track.album_key, albums, album_cap)
        && below_cap(
            track.song_key.as_deref().unwrap_or(track.id.as_str()),
            songs,
            song_cap,
        )
}

fn effective_cap(hard_cap: usize, max_share: f64, target_tracks: usize) -> usize {
    let share_cap = (max_share.clamp(0.0, 1.0) * target_tracks as f64)
        .floor()
        .max(1.0) as usize;
    hard_cap.min(share_cap)
}

fn below_cap(key: &str, counts: &BTreeMap<String, usize>, cap: usize) -> bool {
    key.is_empty() || counts.get(key).copied().unwrap_or(0) < cap
}

fn increment_counts(
    track: &TrackCandidate,
    artists: &mut BTreeMap<String, usize>,
    albums: &mut BTreeMap<String, usize>,
    songs: &mut BTreeMap<String, usize>,
) {
    increment(&track.artist_key, artists);
    increment(&track.album_key, albums);
    increment(track.song_key.as_deref().unwrap_or(track.id.as_str()), songs);
}

fn increment(key: &str, counts: &mut BTreeMap<String, usize>) {
    if !key.is_empty() {
        *counts.entry(key.to_string()).or_insert(0) += 1;
    }
}

fn fit(actual: Option<f64>, target: Option<f64>) -> Option<f64> {
    Some((1.0 - (actual? - target?).abs()).clamp(0.0, 1.0))
}

fn compare_rank(a_score: f64, a_id: &str, b_score: f64, b_id: &str) -> Ordering {
    a_score
        .total_cmp(&b_score)
        .then_with(|| b_id.cmp(a_id))
}

fn error(code: impl Into<String>, message: impl Into<String>) -> AuditIssue {
    AuditIssue {
        severity: AuditSeverity::Error,
        code: code.into(),
        message: message.into(),
        positions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_score_ties_rank_ascending_content_hash() {
        assert_eq!(compare_rank(0.5, "a", 0.5, "b"), Ordering::Greater);
        assert_eq!(compare_rank(0.5, "b", 0.5, "a"), Ordering::Less);
    }
}
