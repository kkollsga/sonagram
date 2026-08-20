use std::cmp::Ordering;

use super::audit::{arc_target, transition_score};
use super::project::TrackCandidate;
use super::types::{PlaylistArc, PlaylistPolicy};

#[derive(Clone)]
struct BeamState {
    path: Vec<usize>,
    score: f64,
    worst_transition: f64,
    artist_gap_violations: usize,
}

pub(crate) fn sequence_tracks<'a>(
    tracks: &[&'a TrackCandidate],
    policy: &PlaylistPolicy,
    beam_width: usize,
) -> Vec<&'a TrackCandidate> {
    if tracks.len() <= 1 {
        return tracks.to_vec();
    }
    let mut beam = vec![BeamState {
        path: Vec::new(),
        score: 0.0,
        worst_transition: 1.0,
        artist_gap_violations: 0,
    }];
    for position in 0..tracks.len() {
        let mut expanded = Vec::new();
        for state in &beam {
            for next in 0..tracks.len() {
                if state.path.contains(&next) {
                    continue;
                }
                let track = tracks[next];
                let arc_fit = track
                    .energy
                    .map(|energy| {
                        (1.0 - (energy - arc_target(policy, position, tracks.len())).abs())
                            .clamp(0.0, 1.0)
                    })
                    .unwrap_or(0.5);
                let transition = state
                    .path
                    .last()
                    .map(|previous| {
                        transition_score(tracks[*previous], track, position, policy).total
                    })
                    .unwrap_or(0.5);
                let gap_violation = violates_artist_gap(&state.path, next, tracks, policy);
                let mut path = state.path.clone();
                path.push(next);
                let (transition_weight, arc_weight) = if policy.transition.arc == PlaylistArc::None
                {
                    (1.0, 0.0)
                } else {
                    (0.72, 0.28)
                };
                expanded.push(BeamState {
                    path,
                    score: state.score + transition_weight * transition + arc_weight * arc_fit,
                    worst_transition: if state.path.is_empty() {
                        state.worst_transition
                    } else {
                        state.worst_transition.min(transition)
                    },
                    artist_gap_violations: state.artist_gap_violations + usize::from(gap_violation),
                });
            }
        }
        expanded.sort_by(|a, b| compare_states(a, b, tracks));
        expanded.truncate(beam_width.max(1));
        beam = expanded;
    }
    beam.first()
        .map(|state| state.path.iter().map(|index| tracks[*index]).collect())
        .unwrap_or_else(|| tracks.to_vec())
}

pub(crate) fn repair_tracks<'a>(
    tracks: &[&'a TrackCandidate],
    policy: &PlaylistPolicy,
    max_attempts: usize,
) -> (Vec<&'a TrackCandidate>, usize) {
    let mut current = tracks.to_vec();
    let mut attempts = 0;
    while attempts < max_attempts {
        let current_metrics = sequence_metrics(&current, policy);
        if current_metrics.errors == 0 {
            break;
        }
        let mut best = current.clone();
        let mut best_metrics = current_metrics;
        for left in 0..current.len() {
            for right in left + 1..current.len() {
                let mut candidate = current.clone();
                candidate.swap(left, right);
                let metrics = sequence_metrics(&candidate, policy);
                if compare_metrics(&metrics, &candidate, &best_metrics, &best) == Ordering::Greater
                {
                    best = candidate;
                    best_metrics = metrics;
                }
            }
        }
        if best
            .iter()
            .map(|track| track.id.as_str())
            .eq(current.iter().map(|track| track.id.as_str()))
        {
            break;
        }
        current = best;
        attempts += 1;
    }
    (current, attempts)
}

pub(crate) fn compare_track_sequences(
    a: &[&TrackCandidate],
    b: &[&TrackCandidate],
    policy: &PlaylistPolicy,
) -> Ordering {
    compare_metrics(
        &sequence_metrics(a, policy),
        a,
        &sequence_metrics(b, policy),
        b,
    )
}

#[derive(Clone, Copy)]
struct SequenceMetrics {
    errors: usize,
    deficit: f64,
    mean_transition: f64,
    worst_transition: f64,
    mean_arc_error: f64,
}

fn sequence_metrics(tracks: &[&TrackCandidate], policy: &PlaylistPolicy) -> SequenceMetrics {
    let transition_values: Vec<f64> = tracks
        .windows(2)
        .enumerate()
        .map(|(position, pair)| transition_score(pair[0], pair[1], position + 1, policy).total)
        .collect();
    let mean_transition = mean(transition_values.iter().copied()).unwrap_or(1.0);
    let worst_transition = transition_values
        .into_iter()
        .min_by(f64::total_cmp)
        .unwrap_or(1.0);
    let mean_arc_error = if policy.transition.arc == PlaylistArc::None {
        0.0
    } else {
        mean(tracks.iter().enumerate().filter_map(|(position, track)| {
            track
                .energy
                .map(|energy| (energy - arc_target(policy, position, tracks.len())).abs())
        }))
        .unwrap_or(0.0)
    };
    let gap_violations = tracks
        .iter()
        .enumerate()
        .filter(|(position, track)| {
            !track.artist_key.is_empty()
                && tracks[..*position]
                    .iter()
                    .rev()
                    .take(policy.diversity.min_artist_gap.saturating_sub(1))
                    .any(|previous| previous.artist_key == track.artist_key)
        })
        .count();
    let mean_deficit = (policy.audit.min_mean_transition_score - mean_transition).max(0.0);
    let worst_deficit = (policy.audit.min_worst_transition_score - worst_transition).max(0.0);
    let arc_deficit = if policy.transition.arc == PlaylistArc::None {
        0.0
    } else {
        (mean_arc_error - policy.audit.max_mean_arc_error).max(0.0)
    };
    let errors = usize::from(mean_deficit > 0.0)
        + usize::from(worst_deficit > 0.0)
        + usize::from(arc_deficit > 0.0)
        + gap_violations;
    SequenceMetrics {
        errors,
        deficit: mean_deficit + worst_deficit + arc_deficit + gap_violations as f64,
        mean_transition,
        worst_transition,
        mean_arc_error,
    }
}

fn compare_metrics(
    a: &SequenceMetrics,
    a_tracks: &[&TrackCandidate],
    b: &SequenceMetrics,
    b_tracks: &[&TrackCandidate],
) -> Ordering {
    b.errors
        .cmp(&a.errors)
        .then_with(|| b.deficit.total_cmp(&a.deficit))
        .then_with(|| a.worst_transition.total_cmp(&b.worst_transition))
        .then_with(|| a.mean_transition.total_cmp(&b.mean_transition))
        .then_with(|| b.mean_arc_error.total_cmp(&a.mean_arc_error))
        .then_with(|| {
            b_tracks
                .iter()
                .map(|track| track.id.as_str())
                .cmp(a_tracks.iter().map(|track| track.id.as_str()))
        })
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0usize), |(sum, count), value| {
        (sum + value, count + 1)
    });
    (count > 0).then_some(sum / count as f64)
}

fn violates_artist_gap(
    path: &[usize],
    next: usize,
    tracks: &[&TrackCandidate],
    policy: &PlaylistPolicy,
) -> bool {
    let gap = policy.diversity.min_artist_gap;
    let artist = &tracks[next].artist_key;
    if gap == 0 || artist.is_empty() {
        return false;
    }
    path.iter().enumerate().any(|(position, index)| {
        path.len() - position < gap && tracks[*index].artist_key == *artist
    })
}

fn compare_states(a: &BeamState, b: &BeamState, tracks: &[&TrackCandidate]) -> Ordering {
    a.artist_gap_violations
        .cmp(&b.artist_gap_violations)
        .then_with(|| b.worst_transition.total_cmp(&a.worst_transition))
        .then_with(|| b.score.total_cmp(&a.score))
        .then_with(|| compare_paths(&a.path, &b.path, tracks))
}

fn compare_paths(a: &[usize], b: &[usize], tracks: &[&TrackCandidate]) -> Ordering {
    a.iter()
        .map(|index| tracks[*index].id.as_str())
        .cmp(b.iter().map(|index| tracks[*index].id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_order_prefers_fewer_hard_spacing_violations() {
        let better = BeamState {
            path: vec![],
            score: 0.1,
            worst_transition: 0.1,
            artist_gap_violations: 0,
        };
        let worse = BeamState {
            path: vec![],
            score: 1.0,
            worst_transition: 1.0,
            artist_gap_violations: 1,
        };
        assert_eq!(
            better
                .artist_gap_violations
                .cmp(&worse.artist_gap_violations),
            Ordering::Less
        );
    }
}
