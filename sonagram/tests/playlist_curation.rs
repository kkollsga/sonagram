//! Library-owned curation regression gate.
//!
//! The synthetic set reproduces the failure class exposed by the real
//! `Quiet Complexity` playlist: individually valid tracks collapse onto a few
//! case-variant artists/albums and are arranged with weak pairwise continuity.
//! It is built from a frozen TrackAnalysis record; no audio is committed.

use sonagram::curation::{
    audit_playlist, audit_playlist_for_brief, curate_playlist, explain_playlist,
    profile_library, PlaylistArc, PlaylistBrief, PlaylistPolicy, PlaylistPreset,
    RelativeDirection, SeedRole, SeedSimilarityPreference,
};
use sonagram::enrich::{ArtistEnrich, EnrichmentData};
use sonagram::graph::{self, LibraryInfo};
use sonagram::playlist;
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

fn alias_graph(records: Vec<AnalysisRecord>) -> std::sync::Arc<kglite::api::DirGraph> {
    let mut enrichment = EnrichmentData::default();
    for artist in ["Alias Ensemble", "The Alias Ensemble"] {
        enrichment.artists.insert(
            artist.into(),
            ArtistEnrich {
                queried_name: artist.into(),
                mbid: Some("shared-musicbrainz-id".into()),
                fetched: true,
                ..ArtistEnrich::default()
            },
        );
    }
    graph::build_graph_with_enrichment(
        &records,
        Some(&enrichment),
        &LibraryInfo {
            root: "synthetic-artist-aliases".into(),
            n_tracks: 2,
        },
    )
    .unwrap()
}

fn alias_records() -> Vec<AnalysisRecord> {
    let mut out = records()[..2].to_vec();
    for (record, (artist, album)) in out.iter_mut().zip([
        ("Alias Ensemble", "Alias Debut"),
        ("The Alias Ensemble", "Alias Return"),
    ]) {
        let tags = record.tags.as_mut().unwrap();
        tags.artist = Some(artist.into());
        tags.album = Some(album.into());
    }
    out
}

fn ids() -> Vec<String> {
    (0..12).map(|i| format!("{i:064x}")).collect()
}

fn seed_intent_records() -> Vec<AnalysisRecord> {
    let mut out = records()[..5].to_vec();
    let embeddings = [0.0, 0.05, 1.0, 0.02, 0.01];
    let energies = [0.90, 0.25, 0.10, 1.00, 0.90];
    for (index, record) in out.iter_mut().enumerate() {
        let tags = record.tags.as_mut().unwrap();
        tags.artist = Some(format!("Seed Artist {index}"));
        tags.album = Some(format!("Seed Album {index}"));
        tags.title = Some(format!("Seed Intent Track {index}"));
        record.analysis.embedding = Some(vec![embeddings[index]; 48]);
        record.analysis.energy = Some(energies[index]);
    }
    out
}

fn category_records() -> Vec<AnalysisRecord> {
    let mut out = records()[..5].to_vec();
    let genres = ["Ambient", "ambient", "Rock", "Ambient", "Ambient"];
    let years = [2004, 2008, 2006, 1996, 2009];
    for (index, record) in out.iter_mut().enumerate() {
        let tags = record.tags.as_mut().unwrap();
        tags.artist = Some(if index == 4 {
            "Blocked Artist".into()
        } else {
            format!("Category Artist {index}")
        });
        tags.album = Some(format!("Category Album {index}"));
        tags.title = Some(format!("Category Track {index}"));
        tags.genre = Some(genres[index].into());
        tags.year = Some(years[index]);
        tags.original_year = None;
    }
    out
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
fn shared_artist_mbid_groups_name_aliases_for_selection_caps_and_audit() {
    let records = alias_records();
    let graph = alias_graph(records.clone());
    let selected = vec![format!("{:064x}", 0), format!("{:064x}", 1)];
    let mut policy = PlaylistPolicy::default();
    policy.eligibility.allow_low_quality = true;
    policy.diversity.max_per_artist = 1;
    policy.diversity.max_per_album = 2;
    policy.audit.min_unique_artist_ratio = 0.0;
    policy.audit.max_artist_share = 1.0;
    policy.audit.max_album_share = 1.0;
    policy.audit.min_mean_transition_score = 0.0;
    policy.audit.min_worst_transition_score = 0.0;
    policy.audit.max_mean_arc_error = 1.0;

    let audit = audit_playlist(&graph, &selected, &policy).unwrap();
    assert_eq!(audit.unique_artists, 1, "shared MBID must define artist identity");
    assert!(audit.issues.iter().any(|issue| issue.code == "artist_cap"));

    let result = curate_playlist(
        &graph,
        &PlaylistBrief {
            target_tracks: 2,
            ..PlaylistBrief::default()
        },
        &policy,
    )
    .unwrap();
    assert!(!result.exportable);
    assert!(result.audit.issues.iter().any(|issue| issue.code == "infeasible_selection"));

    let mut reversed = records;
    reversed.reverse();
    let reversed_graph = alias_graph(reversed);
    assert_eq!(audit, audit_playlist(&reversed_graph, &selected, &policy).unwrap());
    assert_eq!(
        result,
        curate_playlist(
            &reversed_graph,
            &PlaylistBrief {
                target_tracks: 2,
                ..PlaylistBrief::default()
            },
            &policy,
        )
        .unwrap()
    );
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

fn six_track_policy() -> PlaylistPolicy {
    let mut policy = PlaylistPolicy::default();
    policy.eligibility.allow_low_quality = true;
    policy.audit.max_artist_share = 2.0 / 6.0;
    policy.audit.max_album_share = 2.0 / 6.0;
    policy.audit.min_mean_transition_score = 0.0;
    policy.audit.min_worst_transition_score = 0.0;
    policy.audit.max_mean_arc_error = 1.0;
    policy
}

fn id_index(id: &str) -> usize {
    usize::from_str_radix(&id[id.len() - 2..], 16).unwrap()
}

#[test]
fn sequencing_improves_weak_hash_order_and_holds_artist_spacing() {
    let graph = graph();
    let brief = PlaylistBrief {
        target_tracks: 6,
        ..PlaylistBrief::default()
    };
    let mut policy = six_track_policy();
    policy.audit.min_mean_transition_score = 0.50;
    policy.audit.min_worst_transition_score = 0.20;
    policy.transition.arc = PlaylistArc::Rise;
    policy.audit.max_mean_arc_error = 0.25;
    let first = curate_playlist(&graph, &brief, &policy).unwrap();
    let second = curate_playlist(&graph, &brief, &policy).unwrap();
    assert!(first.exportable, "{:?}", first.audit.issues);
    assert_eq!(first, second);
    assert!(
        first.repair_attempts > 0,
        "expected the arc gate to exercise repair: mean={:?} worst={:?} arc={:?}",
        first.audit.mean_transition_score,
        first.audit.worst_transition_score,
        first.audit.mean_arc_error
    );
    assert!(first.audit.mean_arc_error.unwrap() <= policy.audit.max_mean_arc_error);
    assert!(!first.audit.issues.iter().any(|issue| issue.code == "artist_gap"));

    let mut hash_order = first.track_ids.clone();
    hash_order.sort();
    let naive = audit_playlist(&graph, &hash_order, &policy).unwrap();
    assert!(
        first.audit.mean_transition_score.unwrap() > naive.mean_transition_score.unwrap(),
        "sequenced={:?} hash_order={:?}",
        first.audit.mean_transition_score,
        naive.mean_transition_score
    );
}

#[test]
fn rise_and_fall_arcs_order_the_same_pool_differently() {
    let graph = graph();
    let brief = PlaylistBrief {
        target_tracks: 6,
        ..PlaylistBrief::default()
    };
    let mut rise = six_track_policy();
    rise.targets.energy = Some(0.5);
    rise.transition.arc = PlaylistArc::Rise;
    rise.audit.max_mean_arc_error = 0.35;
    let mut fall = rise.clone();
    fall.transition.arc = PlaylistArc::Fall;

    let rising = curate_playlist(&graph, &brief, &rise).unwrap();
    let falling = curate_playlist(&graph, &brief, &fall).unwrap();
    assert!(rising.exportable, "{:?}", rising.audit.issues);
    assert!(falling.exportable, "{:?}", falling.audit.issues);
    assert_eq!(
        rising.track_ids.iter().cloned().collect::<std::collections::BTreeSet<_>>(),
        falling.track_ids.iter().cloned().collect()
    );
    assert_ne!(rising.track_ids, falling.track_ids);
    assert!(id_index(rising.track_ids.first().unwrap()) < id_index(rising.track_ids.last().unwrap()));
    assert!(id_index(falling.track_ids.first().unwrap()) > id_index(falling.track_ids.last().unwrap()));
    assert!(rising.audit.mean_arc_error.unwrap() <= rise.audit.max_mean_arc_error);
    assert!(falling.audit.mean_arc_error.unwrap() <= fall.audit.max_mean_arc_error);
    assert!(rising.explanation.tracks.iter().all(|track| {
        track
            .contributions
            .iter()
            .any(|contribution| contribution.component == "arc_fit")
    }));
}

#[test]
fn no_arc_sequence_is_independent_of_energy_target() {
    let graph = graph();
    let seed_ids = vec![ids()[0].clone(), ids()[6].clone(), ids()[9].clone()];
    let brief = PlaylistBrief {
        target_tracks: seed_ids.len(),
        seed_ids,
        ..PlaylistBrief::default()
    };
    let mut low = six_track_policy();
    low.audit.max_artist_share = 1.0;
    low.audit.max_album_share = 1.0;
    low.audit.min_unique_artist_ratio = 0.0;
    low.transition.arc = PlaylistArc::None;
    low.targets.energy = Some(0.0);
    let mut high = low.clone();
    high.targets.energy = Some(1.0);
    let low_result = curate_playlist(&graph, &brief, &low).unwrap();
    let high_result = curate_playlist(&graph, &brief, &high).unwrap();
    assert_eq!(low_result.track_ids, high_result.track_ids);
}

#[test]
fn independent_audit_reports_artist_spacing_positions() {
    let graph = graph();
    let mut policy = six_track_policy();
    policy.audit.min_unique_artist_ratio = 0.0;
    policy.audit.max_artist_share = 1.0;
    policy.audit.max_album_share = 1.0;
    let selected = vec![ids()[0].clone(), ids()[6].clone(), ids()[1].clone()];
    let audit = audit_playlist(&graph, &selected, &policy).unwrap();
    let issue = audit
        .issues
        .iter()
        .find(|issue| issue.code == "artist_gap")
        .expect("artist gap must be enforced by independent audit");
    assert_eq!(issue.positions, vec![1, 3]);
}

#[test]
fn reference_seed_prefers_a_similar_but_calmer_track() {
    let records = seed_intent_records();
    let graph = graph_from(records.clone());
    let brief = PlaylistBrief {
        target_tracks: 1,
        seed_ids: vec![format!("{:064x}", 0)],
        seed_role: SeedRole::Reference,
        ..PlaylistBrief::default()
    };
    let mut policy = PlaylistPolicy::default();
    policy.eligibility.allow_low_quality = true;
    policy.targets.seed_similarity = SeedSimilarityPreference::Prefer;
    policy.targets.relative_energy = RelativeDirection::Lower;
    policy.targets.relative_energy_margin = 0.10;
    policy.transition.arc = PlaylistArc::None;
    policy.audit.min_unique_artist_ratio = 0.0;
    policy.audit.max_artist_share = 1.0;
    policy.audit.max_album_share = 1.0;
    policy.audit.min_mean_transition_score = 0.0;
    policy.audit.min_worst_transition_score = 0.0;

    let result = curate_playlist(&graph, &brief, &policy).unwrap();
    assert!(result.exportable, "{:?}", result.audit.issues);
    assert_eq!(result.track_ids, vec![format!("{:064x}", 1)]);
    assert!(!result.track_ids.contains(&brief.seed_ids[0]));
    assert!(result
        .explanation
        .summary
        .iter()
        .any(|line| line.contains("typed seed-relative policy")));
    let contributions = &result.explanation.tracks[0].contributions;
    assert!(contributions.iter().any(|item| item.component == "seed_similarity"));
    assert!(contributions
        .iter()
        .any(|item| item.component == "seed_energy_baseline"));
    assert!(contributions
        .iter()
        .any(|item| item.component == "seed_energy_delta" && item.value < 0.0));

    let mut reversed = records;
    reversed.reverse();
    let again = curate_playlist(&graph_from(reversed), &brief, &policy).unwrap();
    assert_eq!(result.track_ids, again.track_ids);

    let violating = vec![format!("{:064x}", 3)];
    let audit = audit_playlist_for_brief(&graph, &violating, &brief, &policy).unwrap();
    assert!(audit
        .issues
        .iter()
        .any(|issue| issue.code == "seed_relative_not_lower"));

    let equal_energy = vec![format!("{:064x}", 4)];
    let audit = audit_playlist_for_brief(&graph, &equal_energy, &brief, &policy).unwrap();
    assert!(audit
        .issues
        .iter()
        .any(|issue| issue.code == "seed_relative_not_lower"));
}

#[test]
fn categorical_policy_is_enforced_by_selection_and_independent_audit() {
    let graph = graph_from(category_records());
    let brief = PlaylistBrief {
        target_tracks: 2,
        ..PlaylistBrief::default()
    };
    let mut policy = PlaylistPolicy::default();
    policy.eligibility.allow_low_quality = true;
    policy.eligibility.include_genres = vec![" AMBIENT ".into()];
    policy.eligibility.include_decades = vec!["2000S".into()];
    policy.eligibility.exclude_artists = vec!["blocked artist".into()];
    policy.transition.arc = PlaylistArc::None;
    policy.audit.min_unique_artist_ratio = 0.0;
    policy.audit.max_artist_share = 1.0;
    policy.audit.max_album_share = 1.0;
    policy.audit.min_mean_transition_score = 0.0;
    policy.audit.min_worst_transition_score = 0.0;

    let result = curate_playlist(&graph, &brief, &policy).unwrap();
    assert!(result.exportable, "{:?}", result.audit.issues);
    assert_eq!(
        result.track_ids.iter().cloned().collect::<std::collections::BTreeSet<_>>(),
        [format!("{:064x}", 0), format!("{:064x}", 1)]
            .into_iter()
            .collect()
    );

    let violating = vec![
        format!("{:064x}", 2),
        format!("{:064x}", 3),
        format!("{:064x}", 4),
    ];
    let audit = audit_playlist(&graph, &violating, &policy).unwrap();
    let codes: Vec<&str> = audit.issues.iter().map(|issue| issue.code.as_str()).collect();
    assert!(codes.contains(&"genre_not_included"), "{codes:?}");
    assert!(codes.contains(&"decade_not_included"), "{codes:?}");
    assert!(codes.contains(&"artist_excluded"), "{codes:?}");
}

#[test]
fn old_policy_and_brief_json_default_new_intent_fields() {
    let mut brief = serde_json::to_value(PlaylistBrief::default()).unwrap();
    brief.as_object_mut().unwrap().remove("seed_role");
    brief.as_object_mut().unwrap().remove("unsupported_intents");
    let brief: PlaylistBrief = serde_json::from_value(brief).unwrap();
    assert_eq!(brief.seed_role, SeedRole::Pinned);

    let mut policy = serde_json::to_value(PlaylistPolicy::default()).unwrap();
    let eligibility = policy["eligibility"].as_object_mut().unwrap();
    for field in [
        "include_artists",
        "exclude_artists",
        "include_genres",
        "exclude_genres",
        "include_styles",
        "exclude_styles",
        "include_decades",
        "exclude_decades",
        "min_year",
        "max_year",
    ] {
        eligibility.remove(field);
    }
    let targets = policy["targets"].as_object_mut().unwrap();
    for field in [
        "seed_similarity",
        "min_seed_similarity",
        "relative_energy",
        "relative_energy_margin",
        "relative_arousal",
        "relative_arousal_margin",
        "relative_tension",
        "relative_tension_margin",
        "relative_vocalness",
        "relative_vocalness_margin",
    ] {
        targets.remove(field);
    }
    let policy: PlaylistPolicy = serde_json::from_value(policy).unwrap();
    assert_eq!(policy.targets.seed_similarity, SeedSimilarityPreference::Neutral);
    assert!(policy.eligibility.include_genres.is_empty());
}

#[test]
fn unusable_seed_measurements_are_structured_infeasibility() {
    let mut records = seed_intent_records()[..2].to_vec();
    for record in &mut records {
        record.analysis.embedding = None;
        record.analysis.energy = None;
    }
    let graph = graph_from(records);
    let brief = PlaylistBrief {
        target_tracks: 1,
        seed_ids: vec![format!("{:064x}", 0)],
        seed_role: SeedRole::Reference,
        ..PlaylistBrief::default()
    };
    let mut policy = PlaylistPolicy::default();
    policy.eligibility.allow_low_quality = true;
    policy.targets.seed_similarity = SeedSimilarityPreference::Prefer;
    policy.targets.relative_energy = RelativeDirection::Similar;
    policy.transition.arc = PlaylistArc::None;
    policy.audit.min_unique_artist_ratio = 0.0;

    let result = curate_playlist(&graph, &brief, &policy).unwrap();
    let codes: Vec<&str> = result.audit.issues.iter().map(|item| item.code.as_str()).collect();
    assert!(!result.exportable);
    assert!(codes.contains(&"seed_similarity_missing"), "{codes:?}");
    assert!(codes.contains(&"seed_relative_missing"), "{codes:?}");
}

#[test]
fn unsupported_or_unknown_intent_never_falls_through_to_agent_guesswork() {
    let graph = graph();
    let brief = PlaylistBrief {
        target_tracks: 1,
        unsupported_intents: vec!["lyrical theme: no breakup songs".into()],
        ..PlaylistBrief::default()
    };
    let result = curate_playlist(&graph, &brief, &PlaylistPolicy::default()).unwrap();
    assert!(!result.exportable);
    assert!(result.audit.issues.iter().any(|issue| issue.code == "unsupported_intent"));

    let unknown = r#"{"preset":"focus","target_tracks":1,"lyrical_theme":"hopeful"}"#;
    let error = serde_json::from_str::<PlaylistBrief>(unknown).unwrap_err();
    assert!(error.to_string().contains("unknown field `lyrical_theme`"));
}

#[test]
fn curated_store_persists_provenance_and_rejects_failed_audits() {
    let graph = graph();
    let brief = PlaylistBrief {
        target_tracks: 6,
        ..PlaylistBrief::default()
    };
    let curated = curate_playlist(&graph, &brief, &six_track_policy()).unwrap();
    assert!(curated.exportable, "{:?}", curated.audit.issues);
    let entries = playlist::entries_from_graph(&graph, std::path::Path::new(""), &curated.track_ids)
        .unwrap();
    let store = std::env::temp_dir().join(format!(
        "sonagram-curated-store-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let saved = playlist::save_curated_playlist(
        &store,
        "Audited Focus",
        Some("focus work"),
        &curated,
        &entries,
        std::path::Path::new("/music.kgl"),
        None,
    )
    .unwrap();
    let loaded = playlist::load_playlist_meta(&store, &saved.slug).unwrap();
    let provenance = loaded.curation.expect("curation provenance");
    assert_eq!(provenance.brief, curated.brief);
    assert_eq!(provenance.policy, curated.policy);
    assert_eq!(provenance.audit.passed, curated.audit.passed);
    assert_eq!(provenance.audit.track_count, curated.audit.track_count);
    assert_eq!(provenance.audit.transitions.len(), curated.audit.transitions.len());
    assert_eq!(provenance.explanation.tracks.len(), curated.explanation.tracks.len());

    let mut failed = curated;
    failed.exportable = false;
    failed.audit.passed = false;
    let rejected_store = store.join("rejected");
    assert!(playlist::save_curated_playlist(
        &rejected_store,
        "Rejected",
        None,
        &failed,
        &entries,
        std::path::Path::new("/music.kgl"),
        None,
    )
    .is_err());
    assert!(!rejected_store.exists(), "failed audit writes nothing");
    let _ = std::fs::remove_dir_all(&store);
}
