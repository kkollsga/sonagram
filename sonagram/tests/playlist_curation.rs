//! Library-owned curation regression gate.
//!
//! The synthetic set reproduces the failure class exposed by the real
//! `Quiet Complexity` playlist: individually valid tracks collapse onto a few
//! case-variant artists/albums and are arranged with weak pairwise continuity.
//! It is built from a frozen TrackAnalysis record; no audio is committed.

use sonagram::curation::{audit_playlist, explain_playlist, profile_library, PlaylistPolicy};
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
            record
        })
        .collect()
}

fn graph() -> std::sync::Arc<kglite::api::DirGraph> {
    let records = records();
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
