use std::collections::{BTreeMap, BTreeSet};

use kglite::api::DirGraph;

use super::project::{candidate_map, embedding_similarity, TrackCandidate};
use super::types::{
    AuditIssue, AuditSeverity, PlaylistArc, PlaylistAudit, PlaylistExplanation, PlaylistPolicy,
    ScoreContribution, TrackExplanation, TransitionScore,
};
use crate::Result;

pub fn audit_playlist(
    graph: &DirGraph,
    track_ids: &[String],
    policy: &PlaylistPolicy,
) -> Result<PlaylistAudit> {
    let candidates = candidate_map(graph)?;
    let mut issues = Vec::new();
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut duplicate_ids = 0;
    for (position, id) in track_ids.iter().enumerate() {
        if !seen.insert(id.as_str()) {
            duplicate_ids += 1;
            issues.push(issue(
                AuditSeverity::Error,
                "duplicate_track",
                format!("track id {id} occurs more than once"),
                vec![position + 1],
            ));
        }
        match candidates.get(id) {
            Some(track) => {
                for (code, message) in eligibility_issues(track, policy) {
                    issues.push(issue(AuditSeverity::Error, code, message, vec![position + 1]));
                }
                selected.push(track);
            }
            None => issues.push(issue(
                AuditSeverity::Error,
                "missing_track",
                format!("track id {id} is not present in the graph"),
                vec![position + 1],
            )),
        }
    }

    let track_count = selected.len();
    let mut artists = BTreeMap::new();
    let mut albums = BTreeMap::new();
    let mut songs = BTreeMap::new();
    for track in &selected {
        if !track.artist_key.is_empty() {
            *artists.entry(track.artist_key.as_str()).or_insert(0usize) += 1;
        }
        if !track.album_key.is_empty() {
            *albums.entry(track.album_key.as_str()).or_insert(0usize) += 1;
        }
        let song = track.song_key.as_deref().unwrap_or(track.id.as_str());
        *songs.entry(song).or_insert(0usize) += 1;
    }
    let denominator = track_count.max(1) as f64;
    let unique_artist_ratio = artists.len() as f64 / denominator;
    let max_artist_share = artists.values().copied().max().unwrap_or(0) as f64 / denominator;
    let max_album_share = albums.values().copied().max().unwrap_or(0) as f64 / denominator;

    for (artist, count) in &artists {
        if *count > policy.diversity.max_per_artist {
            issues.push(issue(
                AuditSeverity::Error,
                "artist_cap",
                format!(
                    "artist {artist} occurs {count} times; maximum is {}",
                    policy.diversity.max_per_artist
                ),
                Vec::new(),
            ));
        }
    }
    for (album, count) in &albums {
        if *count > policy.diversity.max_per_album {
            issues.push(issue(
                AuditSeverity::Error,
                "album_cap",
                format!(
                    "album {album} occurs {count} times; maximum is {}",
                    policy.diversity.max_per_album
                ),
                Vec::new(),
            ));
        }
    }

    if unique_artist_ratio + f64::EPSILON < policy.audit.min_unique_artist_ratio {
        issues.push(issue(
            AuditSeverity::Error,
            "artist_diversity",
            format!(
                "unique artist ratio {:.3} is below {:.3}",
                unique_artist_ratio, policy.audit.min_unique_artist_ratio
            ),
            Vec::new(),
        ));
    }
    let effective_artist_share = policy
        .audit
        .max_artist_share
        .max(1.0 / denominator);
    if max_artist_share > effective_artist_share + f64::EPSILON {
        issues.push(issue(
            AuditSeverity::Error,
            "artist_concentration",
            format!(
                "largest artist share {:.3} exceeds {:.3}",
                max_artist_share, effective_artist_share
            ),
            Vec::new(),
        ));
    }
    let effective_album_share = policy
        .audit
        .max_album_share
        .max(1.0 / denominator);
    if max_album_share > effective_album_share + f64::EPSILON {
        issues.push(issue(
            AuditSeverity::Error,
            "album_concentration",
            format!(
                "largest album share {:.3} exceeds {:.3}",
                max_album_share, effective_album_share
            ),
            Vec::new(),
        ));
    }
    for (song, count) in &songs {
        if *count > policy.diversity.max_per_song {
            issues.push(issue(
                AuditSeverity::Error,
                "song_repetition",
                format!("Song {song} occurs {count} times"),
                Vec::new(),
            ));
        }
    }

    let transitions: Vec<TransitionScore> = selected
        .windows(2)
        .enumerate()
        .map(|(i, pair)| transition_score(pair[0], pair[1], i + 1, policy))
        .collect();
    let mean_transition_score = mean(transitions.iter().map(|t| t.total));
    let worst_transition_score = transitions.iter().map(|t| t.total).min_by(f64::total_cmp);
    if let Some(mean_score) = mean_transition_score {
        if mean_score + f64::EPSILON < policy.audit.min_mean_transition_score {
            issues.push(issue(
                AuditSeverity::Error,
                "mean_transition",
                format!(
                    "mean transition score {:.3} is below {:.3}",
                    mean_score, policy.audit.min_mean_transition_score
                ),
                Vec::new(),
            ));
        }
    }
    if let Some(worst_score) = worst_transition_score {
        if worst_score + f64::EPSILON < policy.audit.min_worst_transition_score {
            let positions = transitions
                .iter()
                .filter(|t| (t.total - worst_score).abs() <= f64::EPSILON)
                .flat_map(|t| [t.from_position, t.to_position])
                .collect();
            issues.push(issue(
                AuditSeverity::Error,
                "worst_transition",
                format!(
                    "worst transition score {:.3} is below {:.3}",
                    worst_score, policy.audit.min_worst_transition_score
                ),
                positions,
            ));
        }
    }

    let arc_errors: Vec<f64> = selected
        .iter()
        .enumerate()
        .filter_map(|(i, track)| {
            track.energy.map(|energy| {
                let target = arc_target(policy, i, selected.len());
                (energy - target).abs()
            })
        })
        .collect();
    let mean_arc_error = mean(arc_errors.into_iter());
    if let Some(error) = mean_arc_error {
        if policy.transition.arc != PlaylistArc::None
            && error > policy.audit.max_mean_arc_error + f64::EPSILON
        {
            issues.push(issue(
                AuditSeverity::Error,
                "arc_deviation",
                format!(
                    "mean energy-arc error {:.3} exceeds {:.3}",
                    error, policy.audit.max_mean_arc_error
                ),
                Vec::new(),
            ));
        }
    } else if !selected.is_empty() {
        issues.push(issue(
            AuditSeverity::Warning,
            "missing_arc_stats",
            "no selected track has energy; arc could not be audited".into(),
            Vec::new(),
        ));
    }

    let passed = !issues.iter().any(|i| i.severity == AuditSeverity::Error);
    Ok(PlaylistAudit {
        passed,
        track_count,
        total_duration_sec: selected.iter().filter_map(|t| t.duration_sec).sum(),
        unique_artists: artists.len(),
        unique_albums: albums.len(),
        unique_songs: songs.len(),
        unique_artist_ratio,
        max_artist_share,
        max_album_share,
        duplicate_ids,
        mean_transition_score,
        worst_transition_score,
        mean_arc_error,
        transitions,
        issues,
    })
}

pub fn explain_playlist(
    graph: &DirGraph,
    track_ids: &[String],
    policy: &PlaylistPolicy,
) -> Result<PlaylistExplanation> {
    let candidates = candidate_map(graph)?;
    let audit = audit_playlist(graph, track_ids, policy)?;
    let tracks = track_ids
        .iter()
        .enumerate()
        .filter_map(|(position, id)| candidates.get(id).map(|track| (position, track)))
        .map(|(position, track)| TrackExplanation {
            position: position + 1,
            content_hash: track.id.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            title: track.title.clone(),
            contributions: track_contributions(track, policy),
        })
        .collect();
    let mut summary = vec![format!(
        "{} tracks, {} artists, audit {}",
        audit.track_count,
        audit.unique_artists,
        if audit.passed { "passed" } else { "failed" }
    )];
    summary.extend(audit.issues.iter().map(|issue| format!("{}: {}", issue.code, issue.message)));
    Ok(PlaylistExplanation {
        tracks,
        transitions: audit.transitions,
        summary,
    })
}

pub(crate) fn eligibility_issues(
    track: &TrackCandidate,
    policy: &PlaylistPolicy,
) -> Vec<(&'static str, String)> {
    let mut issues = Vec::new();
    let e = &policy.eligibility;
    if e.require_music && !track.is_music {
        issues.push(("non_music", "track is not classified as music".into()));
    }
    if e.require_canonical && !track.is_canonical {
        issues.push(("non_canonical", "track is not the canonical recording".into()));
    }
    if !e.allow_low_quality && track.quality_tier.as_deref() == Some("low") {
        issues.push(("low_quality", "track is in the low quality tier".into()));
    }
    check_min(&mut issues, "duration_too_short", "duration", track.duration_sec, e.min_duration_sec);
    check_max(&mut issues, "duration_too_long", "duration", track.duration_sec, e.max_duration_sec);
    check_max(&mut issues, "too_vocal", "vocalness", track.vocalness, e.max_vocalness);
    check_min(&mut issues, "energy_too_low", "energy", track.energy, e.min_energy);
    check_max(&mut issues, "energy_too_high", "energy", track.energy, e.max_energy);
    check_min(&mut issues, "arousal_too_low", "arousal", track.arousal, e.min_arousal);
    check_max(&mut issues, "arousal_too_high", "arousal", track.arousal, e.max_arousal);
    check_min(&mut issues, "tension_too_low", "tension", track.tension, e.min_tension);
    check_max(&mut issues, "tension_too_high", "tension", track.tension, e.max_tension);
    issues
}

pub(crate) fn transition_score(
    a: &TrackCandidate,
    b: &TrackCandidate,
    from_position: usize,
    policy: &PlaylistPolicy,
) -> TransitionScore {
    let embedding = a
        .embedding
        .as_deref()
        .zip(b.embedding.as_deref())
        .and_then(|(x, y)| embedding_similarity(x, y));
    let features = mean(
        [
            feature_similarity(a.energy, b.energy),
            feature_similarity(a.arousal, b.arousal),
            feature_similarity(a.tension, b.tension),
        ]
        .into_iter()
        .flatten(),
    );
    let tempo = tempo_similarity(a, b);
    let key = key_similarity(a.camelot.as_deref(), b.camelot.as_deref());
    let weighted = [
        (embedding, policy.transition.embedding_weight),
        (features, policy.transition.feature_weight),
        (tempo, policy.transition.tempo_weight),
        (key, policy.transition.key_weight),
    ];
    let denominator: f64 = weighted.iter().filter_map(|(v, w)| v.map(|_| *w)).sum();
    let base = if denominator > 0.0 {
        weighted.iter().filter_map(|(v, w)| v.map(|x| x * *w)).sum::<f64>() / denominator
    } else {
        0.0
    };
    let same_artist = !a.artist_key.is_empty() && a.artist_key == b.artist_key;
    let penalty = if same_artist { policy.transition.same_artist_penalty } else { 0.0 };
    TransitionScore {
        from_position,
        to_position: from_position + 1,
        from_id: a.id.clone(),
        to_id: b.id.clone(),
        total: (base - penalty).clamp(0.0, 1.0),
        embedding,
        features,
        tempo,
        key,
        same_artist_penalty: penalty,
    }
}

pub(crate) fn arc_target(policy: &PlaylistPolicy, position: usize, len: usize) -> f64 {
    let base = policy.targets.energy.unwrap_or(0.5);
    if len <= 1 {
        return base;
    }
    let x = position as f64 / (len - 1) as f64;
    match policy.transition.arc {
        PlaylistArc::None | PlaylistArc::Flat => base,
        PlaylistArc::Rise => (base - 0.18 + 0.36 * x).clamp(0.0, 1.0),
        PlaylistArc::Fall => (base + 0.18 - 0.36 * x).clamp(0.0, 1.0),
        PlaylistArc::RiseAndFall => (base - 0.16 + 0.32 * (1.0 - (2.0 * x - 1.0).abs())).clamp(0.0, 1.0),
    }
}

fn track_contributions(track: &TrackCandidate, policy: &PlaylistPolicy) -> Vec<ScoreContribution> {
    let mut out = Vec::new();
    for (name, actual, target) in [
        ("energy_fit", track.energy, policy.targets.energy),
        ("arousal_fit", track.arousal, policy.targets.arousal),
        ("tension_fit", track.tension, policy.targets.tension),
        ("vocalness_fit", track.vocalness, policy.targets.vocalness),
    ] {
        if let (Some(actual), Some(target)) = (actual, target) {
            out.push(ScoreContribution {
                component: name.into(),
                value: (1.0 - (actual - target).abs()).clamp(0.0, 1.0),
            });
        }
    }
    if let Some(quality) = track.recording_quality {
        out.push(ScoreContribution { component: "recording_quality".into(), value: quality });
    }
    out
}

fn tempo_similarity(a: &TrackCandidate, b: &TrackCandidate) -> Option<f64> {
    if a.bpm_confidence? < 0.6 || b.bpm_confidence? < 0.6 {
        return None;
    }
    let (a_bpm, b_bpm) = (a.bpm?, b.bpm?);
    if a_bpm <= 0.0 || b_bpm <= 0.0 {
        return None;
    }
    Some((1.0 - (a_bpm / b_bpm).log2().abs().min(1.0)).clamp(0.0, 1.0))
}

fn key_similarity(a: Option<&str>, b: Option<&str>) -> Option<f64> {
    let (a_num, a_mode) = parse_camelot(a?)?;
    let (b_num, b_mode) = parse_camelot(b?)?;
    if a_num == b_num && a_mode == b_mode {
        Some(1.0)
    } else if a_num == b_num || (a_mode == b_mode && circular_distance(a_num, b_num) == 1) {
        Some(0.8)
    } else {
        Some(0.2)
    }
}

fn parse_camelot(value: &str) -> Option<(i32, char)> {
    let mode = value.chars().last()?;
    let number = value[..value.len().saturating_sub(mode.len_utf8())].parse().ok()?;
    (matches!(mode, 'A' | 'B') && (1..=12).contains(&number)).then_some((number, mode))
}

fn circular_distance(a: i32, b: i32) -> i32 {
    let direct = (a - b).abs();
    direct.min(12 - direct)
}

fn feature_similarity(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    Some((1.0 - (a? - b?).abs()).clamp(0.0, 1.0))
}

fn check_min(
    issues: &mut Vec<(&'static str, String)>,
    code: &'static str,
    name: &str,
    actual: Option<f64>,
    minimum: Option<f64>,
) {
    if let (Some(actual), Some(minimum)) = (actual, minimum) {
        if actual < minimum {
            issues.push((code, format!("{name} {actual:.3} is below {minimum:.3}")));
        }
    }
}

fn check_max(
    issues: &mut Vec<(&'static str, String)>,
    code: &'static str,
    name: &str,
    actual: Option<f64>,
    maximum: Option<f64>,
) {
    if let (Some(actual), Some(maximum)) = (actual, maximum) {
        if actual > maximum {
            issues.push((code, format!("{name} {actual:.3} exceeds {maximum:.3}")));
        }
    }
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0usize), |(sum, count), value| (sum + value, count + 1));
    (count > 0).then_some(sum / count as f64)
}

fn issue(
    severity: AuditSeverity,
    code: impl Into<String>,
    message: String,
    positions: Vec<usize>,
) -> AuditIssue {
    AuditIssue { severity, code: code.into(), message, positions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelot_compatibility_is_symmetric() {
        assert_eq!(key_similarity(Some("8A"), Some("8A")), Some(1.0));
        assert_eq!(key_similarity(Some("8A"), Some("8B")), Some(0.8));
        assert_eq!(key_similarity(Some("12A"), Some("1A")), Some(0.8));
        assert_eq!(key_similarity(Some("1A"), Some("12A")), Some(0.8));
    }
}
