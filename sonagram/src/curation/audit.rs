use std::collections::{BTreeMap, BTreeSet};

use kglite::api::DirGraph;

use super::project::{candidate_map, embedding_similarity, group_key, TrackCandidate};
use super::types::{
    AuditIssue, AuditSeverity, PlaylistArc, PlaylistAudit, PlaylistBrief, PlaylistExplanation,
    PlaylistPolicy, RelativeDirection, ScoreContribution, SeedRole, SeedSimilarityPreference,
    TrackExplanation, TransitionScore,
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
    if policy.diversity.min_artist_gap > 0 {
        let mut artist_positions: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (position, track) in selected.iter().enumerate() {
            if !track.artist_key.is_empty() {
                artist_positions
                    .entry(track.artist_key.as_str())
                    .or_default()
                    .push(position + 1);
            }
        }
        for (artist, positions) in artist_positions {
            let too_close: BTreeSet<usize> = positions
                .windows(2)
                .filter(|pair| pair[1] - pair[0] < policy.diversity.min_artist_gap)
                .flat_map(|pair| [pair[0], pair[1]])
                .collect();
            if !too_close.is_empty() {
                issues.push(issue(
                    AuditSeverity::Error,
                    "artist_gap",
                    format!(
                        "artist {artist} repeats within the minimum gap of {} positions",
                        policy.diversity.min_artist_gap
                    ),
                    too_close.into_iter().collect(),
                ));
            }
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

/// Audits both the resolved execution policy and the request-specific seed intent.
///
/// [`audit_playlist`] remains the backward-compatible policy-only entry point;
/// curated output uses this stricter variant so seed-relative intent cannot be
/// lost between selection and export.
pub fn audit_playlist_for_brief(
    graph: &DirGraph,
    track_ids: &[String],
    brief: &PlaylistBrief,
    policy: &PlaylistPolicy,
) -> Result<PlaylistAudit> {
    let mut audit = audit_playlist(graph, track_ids, policy)?;
    let candidates = candidate_map(graph)?;
    let output_ids: BTreeSet<&str> = track_ids.iter().map(String::as_str).collect();
    for unsupported in &brief.unsupported_intents {
        audit.issues.push(issue(
            AuditSeverity::Error,
            "unsupported_intent",
            format!("typed curation contract cannot enforce: {unsupported}"),
            Vec::new(),
        ));
    }
    if brief.target_tracks == 0 {
        audit.issues.push(issue(
            AuditSeverity::Error,
            "invalid_target",
            "target_tracks must be greater than zero".into(),
            Vec::new(),
        ));
    } else if track_ids.len() != brief.target_tracks {
        audit.issues.push(issue(
            AuditSeverity::Error,
            "target_track_count",
            format!(
                "playlist has {} IDs but the brief requires {}",
                track_ids.len(), brief.target_tracks
            ),
            Vec::new(),
        ));
    }
    if audit.total_duration_sec + f64::EPSILON
        < brief.target_duration_sec.unwrap_or_default() as f64
    {
        audit.issues.push(issue(
            AuditSeverity::Error,
            "duration_shortfall",
            format!(
                "playlist duration {:.0}s is below requested {}s",
                audit.total_duration_sec,
                brief.target_duration_sec.unwrap_or_default()
            ),
            Vec::new(),
        ));
    }
    if brief.preset != policy.preset {
        audit.issues.push(issue(
            AuditSeverity::Error,
            "preset_mismatch",
            format!(
                "brief preset {:?} does not match resolved policy preset {:?}",
                brief.preset, policy.preset
            ),
            Vec::new(),
        ));
    }
    let mut seeds = Vec::new();
    let mut seen_seeds = BTreeSet::new();
    for id in &brief.seed_ids {
        if !seen_seeds.insert(id.as_str()) {
            audit.issues.push(issue(
                AuditSeverity::Error,
                "duplicate_seed",
                format!("seed {id} occurs more than once"),
                Vec::new(),
            ));
            continue;
        }
        let Some(seed) = candidates.get(id) else {
            audit.issues.push(issue(
                AuditSeverity::Error,
                "missing_seed",
                format!("seed {id} is not in the graph"),
                Vec::new(),
            ));
            continue;
        };
        match brief.seed_role {
            SeedRole::Pinned | SeedRole::PinnedAndReference
                if !output_ids.contains(id.as_str()) =>
            {
                audit.issues.push(issue(
                    AuditSeverity::Error,
                    "pinned_seed_missing",
                    format!("pinned seed {id} is absent from the playlist"),
                    Vec::new(),
                ));
            }
            SeedRole::Reference if output_ids.contains(id.as_str()) => {
                audit.issues.push(issue(
                    AuditSeverity::Error,
                    "reference_seed_exported",
                    format!("reference-only seed {id} was exported"),
                    Vec::new(),
                ));
            }
            _ => {}
        }
        if matches!(brief.seed_role, SeedRole::Reference | SeedRole::PinnedAndReference) {
            seeds.push(seed);
        }
    }

    if !(0.0..=1.0).contains(&policy.targets.min_seed_similarity.unwrap_or(0.0)) {
        audit.issues.push(issue(
            AuditSeverity::Error,
            "invalid_seed_similarity",
            "min_seed_similarity must be between 0 and 1".into(),
            Vec::new(),
        ));
    }
    for (feature, margin) in [
        ("energy", policy.targets.relative_energy_margin),
        ("arousal", policy.targets.relative_arousal_margin),
        ("tension", policy.targets.relative_tension_margin),
        ("vocalness", policy.targets.relative_vocalness_margin),
    ] {
        if !(0.0..=1.0).contains(&margin) {
            audit.issues.push(issue(
                AuditSeverity::Error,
                "invalid_relative_margin",
                format!("relative {feature} margin must be between 0 and 1"),
                Vec::new(),
            ));
        }
    }
    if seed_directives_active(policy) && seeds.is_empty() {
        audit.issues.push(issue(
            AuditSeverity::Error,
            "seed_target_without_reference",
            "seed-relative targets require seed_role reference or pinned_and_reference".into(),
            Vec::new(),
        ));
    } else if !seeds.is_empty() {
        let baselines = SeedBaselines::from_tracks(&seeds);
        for (position, id) in track_ids.iter().enumerate() {
            if seen_seeds.contains(id.as_str()) {
                continue;
            }
            let Some(track) = candidates.get(id) else {
                continue;
            };
            for (code, message) in seed_constraint_issues(track, &seeds, &baselines, policy) {
                audit.issues.push(issue(
                    AuditSeverity::Error,
                    code,
                    message,
                    vec![position + 1],
                ));
            }
        }
    }
    audit.passed = !audit.issues.iter().any(|item| item.severity == AuditSeverity::Error);
    Ok(audit)
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
            contributions: track_contributions(track, position, track_ids.len(), policy),
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

pub fn explain_playlist_for_brief(
    graph: &DirGraph,
    track_ids: &[String],
    brief: &PlaylistBrief,
    policy: &PlaylistPolicy,
) -> Result<PlaylistExplanation> {
    let candidates = candidate_map(graph)?;
    let references: Vec<&TrackCandidate> = if matches!(
        brief.seed_role,
        SeedRole::Reference | SeedRole::PinnedAndReference
    ) {
        brief
            .seed_ids
            .iter()
            .filter_map(|id| candidates.get(id))
            .collect()
    } else {
        Vec::new()
    };
    let baselines = SeedBaselines::from_tracks(&references);
    let mut explanation = explain_playlist(graph, track_ids, policy)?;
    if !references.is_empty() && seed_directives_active(policy) {
        for track in &mut explanation.tracks {
            let Some(candidate) = candidates.get(&track.content_hash) else {
                continue;
            };
            if let Some(similarity) = max_seed_similarity(candidate, &references) {
                track.contributions.push(ScoreContribution {
                    component: "seed_similarity".into(),
                    value: similarity,
                });
            }
            for (feature, actual, baseline, direction, margin) in [
                (
                    "energy",
                    candidate.energy,
                    baselines.energy,
                    policy.targets.relative_energy,
                    policy.targets.relative_energy_margin,
                ),
                (
                    "arousal",
                    candidate.arousal,
                    baselines.arousal,
                    policy.targets.relative_arousal,
                    policy.targets.relative_arousal_margin,
                ),
                (
                    "tension",
                    candidate.tension,
                    baselines.tension,
                    policy.targets.relative_tension,
                    policy.targets.relative_tension_margin,
                ),
                (
                    "vocalness",
                    candidate.vocalness,
                    baselines.vocalness,
                    policy.targets.relative_vocalness,
                    policy.targets.relative_vocalness_margin,
                ),
            ] {
                if direction != RelativeDirection::Any {
                    if let Some(seed) = baseline {
                        track.contributions.push(ScoreContribution {
                            component: format!("seed_{feature}_baseline"),
                            value: seed,
                        });
                        track.contributions.push(ScoreContribution {
                            component: format!("seed_{feature}_margin"),
                            value: margin,
                        });
                    }
                    if let Some(delta) = actual.zip(baseline).map(|(value, seed)| value - seed) {
                        track.contributions.push(ScoreContribution {
                            component: format!("seed_{feature}_delta"),
                            value: delta,
                        });
                    }
                }
            }
        }
    }
    let audit = audit_playlist_for_brief(graph, track_ids, brief, policy)?;
    explanation.summary = vec![format!(
        "{} tracks, {} artists, audit {}",
        audit.track_count,
        audit.unique_artists,
        if audit.passed { "passed" } else { "failed" }
    )];
    explanation
        .summary
        .extend(audit.issues.iter().map(|item| format!("{}: {}", item.code, item.message)));
    Ok(explanation)
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
    // Artist filters are user-facing names. `artist_key` is the identity used
    // for diversity and spacing, and may be an opaque MusicBrainz id.
    let artist_name_key = group_key(track.artist.as_deref());
    let artist = [artist_name_key.as_str()];
    check_categories(
        &mut issues,
        "artist_not_included",
        "artist_excluded",
        "artist",
        &artist,
        &e.include_artists,
        &e.exclude_artists,
    );
    check_categories(
        &mut issues,
        "genre_not_included",
        "genre_excluded",
        "genre",
        &track.genre_keys.iter().map(String::as_str).collect::<Vec<_>>(),
        &e.include_genres,
        &e.exclude_genres,
    );
    check_categories(
        &mut issues,
        "style_not_included",
        "style_excluded",
        "style",
        &track.style_keys.iter().map(String::as_str).collect::<Vec<_>>(),
        &e.include_styles,
        &e.exclude_styles,
    );
    check_categories(
        &mut issues,
        "decade_not_included",
        "decade_excluded",
        "decade",
        &track.decade_keys.iter().map(String::as_str).collect::<Vec<_>>(),
        &e.include_decades,
        &e.exclude_decades,
    );
    match track.year {
        Some(year) => {
            if e.min_year.is_some_and(|min| year < min) {
                issues.push(("year_too_early", format!("year {year} is before the minimum")));
            }
            if e.max_year.is_some_and(|max| year > max) {
                issues.push(("year_too_late", format!("year {year} is after the maximum")));
            }
        }
        None if e.min_year.is_some() || e.max_year.is_some() => {
            issues.push(("year_missing", "track has no usable original/file year".into()));
        }
        None => {}
    }
    issues
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SeedBaselines {
    pub energy: Option<f64>,
    pub arousal: Option<f64>,
    pub tension: Option<f64>,
    pub vocalness: Option<f64>,
}

impl SeedBaselines {
    pub(crate) fn from_tracks(tracks: &[&TrackCandidate]) -> Self {
        Self {
            energy: optional_mean(tracks.iter().filter_map(|track| track.energy)),
            arousal: optional_mean(tracks.iter().filter_map(|track| track.arousal)),
            tension: optional_mean(tracks.iter().filter_map(|track| track.tension)),
            vocalness: optional_mean(tracks.iter().filter_map(|track| track.vocalness)),
        }
    }
}

pub(crate) fn seed_directives_active(policy: &PlaylistPolicy) -> bool {
    policy.targets.seed_similarity != SeedSimilarityPreference::Neutral
        || policy.targets.min_seed_similarity.is_some()
        || policy.targets.relative_energy != RelativeDirection::Any
        || policy.targets.relative_arousal != RelativeDirection::Any
        || policy.targets.relative_tension != RelativeDirection::Any
        || policy.targets.relative_vocalness != RelativeDirection::Any
}

pub(crate) fn seed_constraint_issues(
    track: &TrackCandidate,
    seeds: &[&TrackCandidate],
    baselines: &SeedBaselines,
    policy: &PlaylistPolicy,
) -> Vec<(&'static str, String)> {
    let mut issues = Vec::new();
    if policy.targets.seed_similarity != SeedSimilarityPreference::Neutral
        || policy.targets.min_seed_similarity.is_some()
    {
        match max_seed_similarity(track, seeds) {
            Some(value)
                if policy
                    .targets
                    .min_seed_similarity
                    .is_some_and(|minimum| value + f64::EPSILON < minimum) =>
            {
                let minimum = policy.targets.min_seed_similarity.unwrap_or_default();
                issues.push((
                    "seed_similarity_too_low",
                    format!("seed similarity {value:.3} is below {minimum:.3}"),
                ));
            }
            None => issues.push((
                "seed_similarity_missing",
                "track or reference seed has no usable similarity embedding".into(),
            )),
            _ => {}
        }
    }
    check_relative(
        &mut issues,
        "energy",
        track.energy,
        baselines.energy,
        policy.targets.relative_energy,
        policy.targets.relative_energy_margin,
    );
    check_relative(
        &mut issues,
        "arousal",
        track.arousal,
        baselines.arousal,
        policy.targets.relative_arousal,
        policy.targets.relative_arousal_margin,
    );
    check_relative(
        &mut issues,
        "tension",
        track.tension,
        baselines.tension,
        policy.targets.relative_tension,
        policy.targets.relative_tension_margin,
    );
    check_relative(
        &mut issues,
        "vocalness",
        track.vocalness,
        baselines.vocalness,
        policy.targets.relative_vocalness,
        policy.targets.relative_vocalness_margin,
    );
    issues
}

pub(crate) fn max_seed_similarity(
    track: &TrackCandidate,
    seeds: &[&TrackCandidate],
) -> Option<f64> {
    seeds
        .iter()
        .filter_map(|seed| {
            track
                .embedding
                .as_deref()
                .zip(seed.embedding.as_deref())
                .and_then(|(a, b)| embedding_similarity(a, b))
        })
        .max_by(f64::total_cmp)
}

fn check_relative(
    issues: &mut Vec<(&'static str, String)>,
    feature: &'static str,
    actual: Option<f64>,
    baseline: Option<f64>,
    direction: RelativeDirection,
    margin: f64,
) {
    if direction == RelativeDirection::Any {
        return;
    }
    match (actual, baseline) {
        (Some(value), Some(reference)) => {
            let (code, violates) = match direction {
                RelativeDirection::Lower if margin <= f64::EPSILON => {
                    ("seed_relative_not_lower", value + f64::EPSILON >= reference)
                }
                RelativeDirection::Lower => (
                    "seed_relative_not_lower",
                    value > reference - margin + f64::EPSILON,
                ),
                RelativeDirection::Higher if margin <= f64::EPSILON => {
                    ("seed_relative_not_higher", value <= reference + f64::EPSILON)
                }
                RelativeDirection::Higher => (
                    "seed_relative_not_higher",
                    value + f64::EPSILON < reference + margin,
                ),
                RelativeDirection::Similar => (
                    "seed_relative_not_similar",
                    margin > 0.0 && (value - reference).abs() > margin + f64::EPSILON,
                ),
                RelativeDirection::Any => unreachable!(),
            };
            if violates {
                issues.push((
                    code,
                    format!(
                        "{feature} {value:.3} does not satisfy {:?} against seed baseline {reference:.3} with margin {margin:.3}",
                        direction
                    ),
                ));
            }
        }
        _ => issues.push((
            "seed_relative_missing",
            format!("{feature} is missing on the track or every reference seed"),
        )),
    }
}

fn optional_mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0usize), |(sum, count), value| {
        (sum + value, count + 1)
    });
    (count > 0).then_some(sum / count as f64)
}

fn check_categories(
    issues: &mut Vec<(&'static str, String)>,
    include_code: &'static str,
    exclude_code: &'static str,
    label: &str,
    track_values: &[&str],
    include: &[String],
    exclude: &[String],
) {
    let track_values: BTreeSet<String> =
        track_values.iter().map(|value| group_key(Some(*value))).collect();
    let include: BTreeSet<String> =
        include.iter().map(|value| group_key(Some(value.as_str()))).collect();
    let exclude: BTreeSet<String> =
        exclude.iter().map(|value| group_key(Some(value.as_str()))).collect();
    if !include.is_empty() && track_values.is_disjoint(&include) {
        issues.push((include_code, format!("track does not match an included {label}")));
    }
    if !exclude.is_empty() && !track_values.is_disjoint(&exclude) {
        issues.push((exclude_code, format!("track matches an excluded {label}")));
    }
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

fn track_contributions(
    track: &TrackCandidate,
    position: usize,
    playlist_len: usize,
    policy: &PlaylistPolicy,
) -> Vec<ScoreContribution> {
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
    if policy.transition.arc != PlaylistArc::None {
        if let Some(energy) = track.energy {
            out.push(ScoreContribution {
                component: "arc_fit".into(),
                value: (1.0 - (energy - arc_target(policy, position, playlist_len)).abs())
                    .clamp(0.0, 1.0),
            });
        }
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
