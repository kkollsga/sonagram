//! Library-owned curation regression gate.
//!
//! The synthetic set reproduces the failure class exposed by the real
//! `Quiet Complexity` playlist: individually valid tracks collapse onto a few
//! case-variant artists/albums and are arranged with weak pairwise continuity.
//! It is built from a frozen TrackAnalysis record; no audio is committed.

use sonagram::curation::{
    audit_playlist, curate_playlist, explain_playlist, profile_library, PlaylistBrief,
    PlaylistPolicy, PlaylistPreset,
};
use sonagram::graph::{self, LibraryInfo};
use sonagram::record::AnalysisRecord;

fn records() -> Vec<AnalysisRecord> {
    let template: AnalysisRecord =
        serde_json::from_str(include_str!("fixtures/analyses/04-marry-you.json")).unwrap();
    let artists = [
        "Ensemble Alpha",
        "ENSEMBLE ALPHA",
        "Ensemble Alpha",
        "Ensemble Alpha",
        "Ensemble Alpha",
        "ensemble alpha",
        "Orchestra Beta",
        "Orchestra Beta",
        "Orchestra Beta",
        "Soloist Gamma",
        "Quartet Delta",
        "Composer Epsilon",
    ];
    (0..artists.len())
        .map(|i| {
            let mut record = template.clone();
            record.source.content_hash = format!("{i:064x}");
            record.source.path = format!("track-{i:02}.mp3");
            let tags = record.tags.as_mut().unwrap();
            tags.artist = Some(artists[i].to_string());
            tags.title = Some(format!("Anonymous Piece {i:02}"));
            tags.album = Some(if i < 6 {
                "Collected Alpha"
            } else if i < 9 {
                "Collected Beta"
            } else {
                "Independent Works"
            }
            .to_string());
            record.analysis.embedding = Some(vec![if i % 2 == 0 { 0.0 } else { 1.0 }; 48]);
            record.analysis.energy = Some(i as f32 / (artists.len() - 1) as f32);
            record.analysis.duration_sec = 90.0 + i as f32 * 20.0;
            record.analysis.vocalness = Some(0.1);
            record
        })
        .collect()
}

fn graph() -> std::sync::Arc<kglite::api::DirGraph> {
    graph_from(records())
}

fn graph_from(records: Vec<AnalysisRecord>) -> std::sync::Arc<kglite::api::DirGraph> {
    graph::build_graph(
        &records,
        &LibraryInfo {
            root: "synthetic-curation".into(),
            n_tracks: records.len(),
        },
    )
    .unwrap()
}

fn ids() -> Vec<String> {
    (0..12).map(|i| format!("{i:064x}")).collect()
}

#[test]
fn caller_owned_legacy_order_fails_diversity_and_transition_audit() {
    let graph = graph();
    let policy = PlaylistPolicy::default();
    let audit = audit_playlist(&graph, &ids(), &policy).unwrap();

    assert!(!audit.passed);
    assert_eq!(audit.track_count, 12);
    assert_eq!(audit.unique_artists, 5, "case variants are one artist family");
    assert!((audit.max_artist_share - 0.5).abs() < 1e-9);
    assert!((audit.max_album_share - 0.5).abs() < 1e-9);
    let codes: Vec<&str> = audit.issues.iter().map(|i| i.code.as_str()).collect();
    assert!(codes.contains(&"artist_diversity"), "{codes:?}");
    assert!(codes.contains(&"artist_concentration"), "{codes:?}");
    assert!(codes.contains(&"album_concentration"), "{codes:?}");
    assert!(codes.contains(&"mean_transition") || codes.contains(&"worst_transition"));
}

#[test]
fn profile_and_explanation_are_deterministic_and_structured() {
    let graph = graph();
    let profile = profile_library(&graph).unwrap();
    assert_eq!(profile.tracks, 12);
    assert_eq!(profile.unique_artists, 5);
    assert_eq!(profile.unique_albums, 3);
    assert_eq!(profile.stats["energy"].present, 12);

    let first = explain_playlist(&graph, &ids(), &PlaylistPolicy::default()).unwrap();
    let second = explain_playlist(&graph, &ids(), &PlaylistPolicy::default()).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.tracks.len(), 12);
    assert_eq!(first.transitions.len(), 11);
    assert!(first.summary.iter().any(|line| line.contains("audit failed")));
}

#[test]
fn missing_and_duplicate_ids_are_hard_failures() {
    let graph = graph();
    let mut selected = ids()[..3].to_vec();
    selected.push(selected[0].clone());
    selected.push("missing".into());
    let audit = audit_playlist(&graph, &selected, &PlaylistPolicy::default()).unwrap();
    let codes: Vec<&str> = audit.issues.iter().map(|i| i.code.as_str()).collect();
    assert!(!audit.passed);
    assert!(codes.contains(&"duplicate_track"));
    assert!(codes.contains(&"missing_track"));
}

#[test]
fn constrained_selection_breaks_the_legacy_artist_album_collapse() {
    let graph = graph();
    let brief = PlaylistBrief {
        target_tracks: 6,
        ..PlaylistBrief::default()
    };
    let mut policy = PlaylistPolicy::default();
    policy.audit.max_artist_share = 2.0 / 6.0;
    policy.audit.max_album_share = 2.0 / 6.0;
    policy.audit.min_mean_transition_score = 0.0;
    policy.audit.min_worst_transition_score = 0.0;
    policy.audit.max_mean_arc_error = 1.0;
    let result = curate_playlist(&graph, &brief, &policy).unwrap();
    assert!(result.exportable, "{:?}", result.audit.issues);
    assert_eq!(result.track_ids.len(), 6);
    assert!(result.audit.max_artist_share <= 2.0 / 6.0 + f64::EPSILON);
    assert!(result.audit.max_album_share <= 2.0 / 6.0 + f64::EPSILON);
}

#[test]
fn presets_choose_different_tracks_and_results_are_deterministic() {
    let graph = graph();
    let focus_brief = PlaylistBrief {
        preset: PlaylistPreset::Focus,
        target_tracks: 3,
        ..PlaylistBrief::default()
    };
    let party_brief = PlaylistBrief {
        preset: PlaylistPreset::Party,
        target_tracks: 3,
        ..PlaylistBrief::default()
    };
    let mut focus = PlaylistPolicy::for_preset(PlaylistPreset::Focus);
    let mut party = PlaylistPolicy::for_preset(PlaylistPreset::Party);
    focus.eligibility.max_vocalness = None;
    focus.eligibility.max_energy = None;
    focus.eligibility.max_arousal = None;
    for policy in [&mut focus, &mut party] {
        policy.audit.min_mean_transition_score = 0.0;
        policy.audit.min_worst_transition_score = 0.0;
        policy.audit.max_mean_arc_error = 1.0;
        policy.audit.min_unique_artist_ratio = 0.0;
    }
    let first = curate_playlist(&graph, &focus_brief, &focus).unwrap();
    let second = curate_playlist(&graph, &focus_brief, &focus).unwrap();
    let party_result = curate_playlist(&graph, &party_brief, &party).unwrap();
    assert!(first.exportable, "{:?}", first.audit.issues);
    assert!(party_result.exportable, "{:?}", party_result.audit.issues);
    assert_eq!(first.track_ids.len(), 3);
    assert_eq!(party_result.track_ids.len(), 3);
    assert_eq!(first.track_ids, second.track_ids);
    assert_ne!(first.track_ids, party_result.track_ids);

    drop(graph);
    let mut reversed = records();
    reversed.reverse();
    let reversed_result = curate_playlist(&graph_from(reversed), &focus_brief, &focus).unwrap();
    assert_eq!(first.track_ids, reversed_result.track_ids);
}

#[test]
fn infeasible_hard_caps_return_no_exportable_playlist() {
    let graph = graph();
    let brief = PlaylistBrief {
        target_tracks: 10,
        ..PlaylistBrief::default()
    };
    let mut policy = PlaylistPolicy::default();
    policy.diversity.max_per_artist = 1;
    policy.diversity.max_per_album = 1;
    let result = curate_playlist(&graph, &brief, &policy).unwrap();
    assert!(!result.exportable);
    assert!(result.audit.issues.iter().any(|issue| issue.code == "infeasible_selection"));
}

#[test]
fn mismatched_preset_and_excess_seeds_are_structured_failures() {
    let graph = graph();
    let brief = PlaylistBrief {
        preset: PlaylistPreset::Focus,
        target_tracks: 1,
        seed_ids: ids()[..2].to_vec(),
        ..PlaylistBrief::default()
    };
    let result = curate_playlist(&graph, &brief, &PlaylistPolicy::default()).unwrap();
    let codes: Vec<&str> = result.audit.issues.iter().map(|issue| issue.code.as_str()).collect();
    assert!(!result.exportable);
    assert!(codes.contains(&"too_many_seeds"));
    assert!(codes.contains(&"preset_mismatch"));
}

#[test]
fn independent_audit_enforces_hard_artist_and_album_caps() {
    let graph = graph();
    let mut policy = PlaylistPolicy::default();
    policy.audit.min_unique_artist_ratio = 0.0;
    policy.audit.max_artist_share = 1.0;
    policy.audit.max_album_share = 1.0;
    policy.audit.min_mean_transition_score = 0.0;
    policy.audit.min_worst_transition_score = 0.0;
    policy.audit.max_mean_arc_error = 1.0;
    let audit = audit_playlist(&graph, &ids()[..6], &policy).unwrap();
    let codes: Vec<&str> = audit.issues.iter().map(|issue| issue.code.as_str()).collect();
    assert!(!audit.passed);
    assert!(codes.contains(&"artist_cap"));
    assert!(codes.contains(&"album_cap"));
}

#[test]
fn duration_target_influences_selection_and_is_met_when_feasible() {
    let graph = graph();
    let brief = PlaylistBrief {
        target_tracks: 3,
        target_duration_sec: Some(740),
        ..PlaylistBrief::default()
    };
    let mut policy = PlaylistPolicy::default();
    policy.targets.energy = Some(0.0);
    policy.audit.min_unique_artist_ratio = 0.0;
    policy.audit.min_mean_transition_score = 0.0;
    policy.audit.min_worst_transition_score = 0.0;
    policy.audit.max_mean_arc_error = 1.0;
    let result = curate_playlist(&graph, &brief, &policy).unwrap();
    assert!(result.exportable, "{:?}", result.audit.issues);
    assert!(result.audit.total_duration_sec >= 740.0);
    assert_eq!(result.repair_attempts, 1);
}
