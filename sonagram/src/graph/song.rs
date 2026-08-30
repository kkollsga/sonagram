//! P21 Stage C: the Song version layer — grouping the many recordings of one
//! composition so an agent can filter to the best take.
//!
//! A large library holds many recordings of the same song (studio master,
//! session outtake, live take, cover), and threshold queries otherwise treat a
//! bootleg #7 the same as the canonical master. This module groups tracks that
//! share a **version key** `(artist_id, normalized_title)` (see
//! [`normalize::normalized_title`]) and, for every group of two or more, emits a
//! `Song` node with `Track -[:VERSION_OF]-> Song` edges plus a `canonical_hash`
//! pointing at the best member. It also stamps [`is_canonical`](SongGrouping) on
//! every track, so `WHERE t.is_canonical` is the universal "skip duplicate /
//! inferior takes" filter.
//!
//! ## What "best" means
//! Within a group a usable Last.fm match wins first (a recognized release beats
//! an unmatched outtake), then the highest `recording_quality` wins (nulls sort
//! **lowest**), and `content_hash` ascending breaks any remaining tie. A plain
//! build has no matches and therefore degrades exactly to the audio-quality
//! ordering.
//!
//! ## Conservative junk-tag repair
//! Primary groups still key on normalized title + artist. After `SIMILAR_TO` is
//! materialised, an explicitly junk-tagged track may move to a unique non-junk
//! primary Song of the same normalized title when an audio edge in either
//! direction reaches one of that Song's original members. Known-artist covers
//! never move, and reassigned tracks never become anchors for further moves.
//!
//! ## Determinism
//! Groups are keyed in a `BTreeMap`, so iteration is sorted-key order; members
//! within a group inherit the caller's `content_hash`-sorted record order; the
//! canonical pick is a total order over `(has_lastfm_match, recording_quality,
//! content_hash)`. Song nodes and `VERSION_OF` edges are therefore emitted
//! identically across runs and input permutations.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use kglite::api::mutation::{add_edges_from_specs, add_nodes};
use kglite::api::mutation::{ColumnData, ColumnType, DataFrame};
use kglite::api::DirGraph;

use crate::record::AnalysisRecord;
use crate::Result;

use super::derive::SimEdge;
use super::normalize::{artist_id, filename_from_path, normalized_title, UNKNOWN_ARTIST};
use super::{add, build_df, check_edges, edge, SONG, TRACK, VERSION_OF};

/// One version group of two or more recordings of the same song.
pub(super) struct SongGroup {
    /// Node id: `"<artist_id>|<normalized_title>"`.
    pub id: String,
    /// The normalized title (the `Song.title` property).
    pub title: String,
    /// The artist id (the `Song.artist` property).
    pub artist: String,
    /// Member `content_hash`es, in ascending (input) order.
    pub members: Vec<String>,
    /// `content_hash` of the canonical (best) member.
    pub canonical_hash: String,
}

/// The result of grouping the library into songs: the emitted [`SongGroup`]s
/// (sorted by id) and a per-track `is_canonical` flag parallel to the input
/// records.
pub(super) struct SongGrouping {
    /// Version groups with ≥2 members, in sorted-id order. Singletons are
    /// excluded — a "song" of one recording is not a version group.
    pub songs: Vec<SongGroup>,
    /// `is_canonical[i]` for input record `i`: `true` for every track that is not
    /// a non-best member of a version group (so all singletons are `true`, and the
    /// best member of every group is `true`).
    pub is_canonical: Vec<bool>,
}

/// Group `sorted` (already `content_hash`-ordered) into songs by version key,
/// selecting the canonical member of each group from the parallel Last.fm-match
/// and `recording_quality` inputs.
pub(super) fn group_songs(
    sorted: &[&AnalysisRecord],
    quality: &[Option<f64>],
    has_lastfm_match: &[bool],
) -> SongGrouping {
    let keys = sorted.iter().map(|r| version_key(r)).collect();
    group_songs_by_keys(sorted, quality, has_lastfm_match, keys)
}

/// Refine the primary title+artist groups using the already-materialised audio
/// neighbour graph. Only explicitly junk-tagged tracks may move, and only to a
/// single non-junk `Song` that existed in `primary`. Confirmation is an edge in
/// either direction to one of that target's original members. The fixed primary
/// member sets prevent reassigned candidates from creating a cascade.
pub(super) fn refine_songs(
    sorted: &[&AnalysisRecord],
    quality: &[Option<f64>],
    has_lastfm_match: &[bool],
    primary: &SongGrouping,
    sim_edges: &[SimEdge],
) -> SongGrouping {
    let mut targets_by_title: BTreeMap<&str, Vec<&SongGroup>> = BTreeMap::new();
    for song in &primary.songs {
        if !is_junk_artist(&song.artist) {
            targets_by_title.entry(&song.title).or_default().push(song);
        }
    }

    let edge_pairs: BTreeSet<(&str, &str)> = sim_edges
        .iter()
        .map(|edge| ordered_pair(&edge.src, &edge.tgt))
        .collect();
    let mut assignments: BTreeMap<usize, String> = BTreeMap::new();
    for (idx, record) in sorted.iter().enumerate() {
        let artist = artist_id(record.tags.as_ref().and_then(|tags| tags.artist.as_deref()));
        if !is_junk_artist(&artist) {
            continue;
        }
        let title = normalized_record_title(record);
        let Some(targets) = targets_by_title.get(title.as_str()) else {
            continue;
        };
        if targets.len() != 1 {
            continue;
        }
        let target = targets[0];
        let candidate = record.source.content_hash.as_str();
        if target
            .members
            .iter()
            .any(|member| edge_pairs.contains(&ordered_pair(candidate, member)))
        {
            assignments.insert(idx, target.id.clone());
        }
    }

    let keys = sorted
        .iter()
        .enumerate()
        .map(|(idx, record)| {
            assignments
                .get(&idx)
                .cloned()
                .unwrap_or_else(|| version_key(record))
        })
        .collect();
    group_songs_by_keys(sorted, quality, has_lastfm_match, keys)
}

fn group_songs_by_keys(
    sorted: &[&AnalysisRecord],
    quality: &[Option<f64>],
    has_lastfm_match: &[bool],
    keys: Vec<String>,
) -> SongGrouping {
    let hashes: Vec<&str> = sorted
        .iter()
        .map(|r| r.source.content_hash.as_str())
        .collect();

    // Version key → member indices (kept in the caller's sorted order).
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, key) in keys.into_iter().enumerate() {
        groups.entry(key).or_default().push(i);
    }

    let mut is_canonical = vec![true; sorted.len()];
    let mut songs: Vec<SongGroup> = Vec::new();
    for (id, members) in groups {
        if members.len() < 2 {
            continue; // singleton → no Song node, stays canonical
        }
        let best = *members
            .iter()
            .reduce(|acc, idx| {
                if is_better(
                    has_lastfm_match[*idx],
                    quality[*idx],
                    hashes[*idx],
                    has_lastfm_match[*acc],
                    quality[*acc],
                    hashes[*acc],
                ) {
                    idx
                } else {
                    acc
                }
            })
            .expect("group has ≥2 members");
        for &idx in &members {
            is_canonical[idx] = idx == best;
        }
        // Reconstruct the key parts for the node properties. The id splits on the
        // FIRST '|' — artist ids may not contain '|' (album ids already rely on
        // this), so this round-trips the (artist, title) pair.
        let (artist, title) = id.split_once('|').unwrap_or((id.as_str(), ""));
        songs.push(SongGroup {
            id: id.clone(),
            title: title.to_string(),
            artist: artist.to_string(),
            members: members.iter().map(|&i| hashes[i].to_string()).collect(),
            canonical_hash: hashes[best].to_string(),
        });
    }

    SongGrouping {
        songs,
        is_canonical,
    }
}

fn ordered_pair<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Whether an artist tag is one of the empirically observed placeholders.
/// `TJT###` tags often carry a suffix (`TJT110 The Beatles`), hence the prefix
/// rule; it is deliberately ASCII-only and requires a digit after `TJT`.
fn is_junk_artist(artist: &str) -> bool {
    if artist == UNKNOWN_ARTIST || artist == "Artiest onbekend" {
        return true;
    }
    let bytes = artist.as_bytes();
    bytes.len() > 3 && bytes[..3].eq_ignore_ascii_case(b"TJT") && bytes[3].is_ascii_digit()
}

/// The version key `"<artist_id>|<normalized_title>"` for a record. Title is the
/// tag title (trimmed, non-empty) or the file name — the same resolution the
/// `Track` node uses — run through [`normalized_title`].
fn version_key(r: &AnalysisRecord) -> String {
    let artist = artist_id(r.tags.as_ref().and_then(|t| t.artist.as_deref()));
    format!("{artist}|{}", normalized_record_title(r))
}

fn normalized_record_title(r: &AnalysisRecord) -> String {
    let raw_title = r
        .tags
        .as_ref()
        .and_then(|t| t.title.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| filename_from_path(&r.source.path));
    normalized_title(&raw_title)
}

/// Whether candidate `(matched_a, q_a, h_a)` is a strictly better canonical
/// pick than the incumbent: a Last.fm match wins first, then higher
/// `recording_quality` (nulls lowest), then the smaller `content_hash`.
fn is_better(
    matched_a: bool,
    q_a: Option<f64>,
    h_a: &str,
    matched_b: bool,
    q_b: Option<f64>,
    h_b: &str,
) -> bool {
    match matched_a.cmp(&matched_b) {
        Ordering::Greater => return true,
        Ordering::Less => return false,
        Ordering::Equal => {}
    }
    match cmp_quality(q_a, q_b) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => h_a < h_b,
    }
}

/// Compare two `recording_quality` values with **nulls lowest**.
fn cmp_quality(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => x.total_cmp(&y),
    }
}

/// Add the `Song` nodes and `Track -[:VERSION_OF]-> Song` edges for every version
/// group. A no-op when no key grouped ≥2 tracks. Nodes are emitted in sorted-id
/// order and edges in `(song id, member order)` order, so the output is
/// deterministic.
pub(super) fn add_songs(graph: &mut DirGraph, grouping: &SongGrouping) -> Result<()> {
    if grouping.songs.is_empty() {
        return Ok(());
    }
    let songs = &grouping.songs;
    let ids: Vec<Option<String>> = songs.iter().map(|s| Some(s.id.clone())).collect();
    let titles: Vec<Option<String>> = songs.iter().map(|s| Some(s.title.clone())).collect();
    let artists: Vec<Option<String>> = songs.iter().map(|s| Some(s.artist.clone())).collect();
    let n_versions: Vec<Option<i64>> = songs.iter().map(|s| Some(s.members.len() as i64)).collect();
    let canonical_hash: Vec<Option<String>> = songs
        .iter()
        .map(|s| Some(s.canonical_hash.clone()))
        .collect();

    let df = build_df(vec![
        ("id", ColumnType::String, ColumnData::String(ids)),
        ("title", ColumnType::String, ColumnData::String(titles)),
        ("artist", ColumnType::String, ColumnData::String(artists)),
        (
            "n_versions",
            ColumnType::Int64,
            ColumnData::Int64(n_versions),
        ),
        (
            "canonical_hash",
            ColumnType::String,
            ColumnData::String(canonical_hash),
        ),
    ]);
    add(graph, df, SONG, "id", "title")?;

    let mut specs = Vec::new();
    for s in songs {
        for member in &s.members {
            specs.push(edge(TRACK, member, SONG, &s.id, VERSION_OF));
        }
    }
    check_edges(add_edges_from_specs(graph, specs), "VERSION_OF")
}

/// Partially update only the canonical flag after audio-confirmed refinement.
/// `conflict_handling=update` writes the two supplied columns and preserves the
/// full-width Track row, including its title alias and every analysis property.
pub(super) fn update_canonical_flags(
    graph: &mut DirGraph,
    sorted: &[&AnalysisRecord],
    is_canonical: &[bool],
) -> Result<()> {
    if sorted.is_empty() {
        return Ok(());
    }
    let ids = sorted
        .iter()
        .map(|record| Some(record.source.content_hash.clone()))
        .collect();
    let flags = is_canonical.iter().map(|&flag| Some(flag)).collect();
    let mut df = DataFrame::new(Vec::new());
    df.add_column(
        "content_hash".to_string(),
        ColumnType::String,
        ColumnData::String(ids),
    )
    .map_err(crate::SonagramError::Graph)?;
    df.add_column(
        "is_canonical".to_string(),
        ColumnType::Boolean,
        ColumnData::Boolean(flags),
    )
    .map_err(crate::SonagramError::Graph)?;
    add_nodes(
        graph,
        df,
        TRACK.to_string(),
        "content_hash".to_string(),
        None,
        Some("update".to_string()),
    )
    .map(|_| ())
    .map_err(crate::SonagramError::Graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{AnalysisDto, ProvenanceDto, SourceInfo, TagsDto};

    /// Minimal record with a title/artist and content hash — the only fields the
    /// grouping reads. Everything else is left null/zero.
    fn rec(hash: &str, artist: Option<&str>, title: Option<&str>) -> AnalysisRecord {
        AnalysisRecord {
            record_version: 1,
            source: SourceInfo {
                content_hash: hash.to_string(),
                hash_kind: "whole-file-v0".to_string(),
                path: format!("{hash}.mp3"),
                file_size: 0,
                format: "mp3".to_string(),
            },
            tags: Some(TagsDto {
                title: title.map(String::from),
                artist: artist.map(String::from),
                album: None,
                genre: None,
                year: None,
                original_year: None,
                track_no: None,
            }),
            analysis: minimal_analysis(),
        }
    }

    fn minimal_analysis() -> AnalysisDto {
        // A record with every optional analysis field left None/empty; only the
        // grouping-relevant identity fields matter here.
        AnalysisDto {
            provenance: ProvenanceDto {
                schema_version: 3,
                sample_rate: 22050,
                hop_length: 512,
                mode: "playlist".to_string(),
                requested_features: None,
                genre_model_id: None,
                vocalness_model_id: None,
                aggression_model_id: None,
            },
            duration_sec: 1.0,
            bpm: 0.0,
            bpm_raw: 0.0,
            bpm_confidence: 0.0,
            bpm_candidates: vec![],
            beats: vec![],
            onset_frames: vec![],
            rms_mean: 0.0,
            rms_max: 0.0,
            loudness_lufs: 0.0,
            dynamic_range_db: 0.0,
            true_peak_db: None,
            replaygain_db: None,
            loudness_curve: None,
            loudness_momentary_max_db: None,
            loudness_range_lu: None,
            spectral_centroid_mean: 0.0,
            zero_crossing_rate: 0.0,
            onset_density: 0.0,
            spectral_bandwidth_mean: None,
            spectral_rolloff_mean: None,
            spectral_flatness_mean: None,
            spectral_contrast_mean: None,
            mfcc_mean: None,
            chroma_mean: None,
            tempo_curve: None,
            tempo_variability: None,
            time_signature: None,
            time_signature_confidence: None,
            chord_sequence: None,
            chord_events: None,
            chord_change_rate: None,
            predominant_chord: None,
            dissonance: None,
            energy: None,
            danceability: None,
            key: None,
            key_confidence: None,
            key_camelot: None,
            valence: None,
            acousticness: None,
            embedding: None,
            aggression_score: None,
            aggression_confidence: None,
            aggression_forcefulness: None,
            aggression_harshness: None,
            aggression_tension: None,
            aggression_rhythm: None,
            mood_happy: None,
            mood_aggressive: None,
            mood_relaxed: None,
            mood_sad: None,
            instrumentalness: None,
            genre: None,
            genre_confidence: None,
            grid_offset_sec: None,
            downbeats: None,
            grid_stability: None,
            energy_curve: None,
            energy_curve_hop_sec: None,
            segments: None,
            intro_end_sec: None,
            outro_start_sec: None,
            energy_level: None,
            leading_silence_sec: None,
            trailing_silence_sec: None,
            key_candidates: None,
            vocalness: None,
            fingerprint: None,
            embedding_version: None,
        }
    }

    fn group(recs: &[AnalysisRecord], quality: &[Option<f64>]) -> SongGrouping {
        let refs: Vec<&AnalysisRecord> = recs.iter().collect();
        let has_lastfm_match = vec![false; recs.len()];
        group_songs(&refs, quality, &has_lastfm_match)
    }

    fn group_with_matches(
        recs: &[AnalysisRecord],
        quality: &[Option<f64>],
        has_lastfm_match: &[bool],
    ) -> SongGrouping {
        let refs: Vec<&AnalysisRecord> = recs.iter().collect();
        group_songs(&refs, quality, has_lastfm_match)
    }

    #[test]
    fn singletons_get_no_song_and_stay_canonical() {
        let recs = vec![
            rec("h1", Some("A"), Some("Alpha")),
            rec("h2", Some("A"), Some("Beta")),
        ];
        let g = group(&recs, &[Some(0.5), Some(0.9)]);
        assert!(g.songs.is_empty(), "distinct titles form no version group");
        assert_eq!(g.is_canonical, vec![true, true]);
    }

    #[test]
    fn version_group_picks_highest_quality_canonical() {
        // Three takes of one song; the middle index has the best quality.
        let recs = vec![
            rec("h1", Some("The Beatles"), Some("Yesterday")),
            rec("h2", Some("The Beatles"), Some("Yesterday - Live")),
            rec(
                "h3",
                Some("The Beatles"),
                Some("Yesterday (Remastered 2009)"),
            ),
        ];
        let g = group(&recs, &[Some(0.2), Some(0.9), Some(0.5)]);
        assert_eq!(g.songs.len(), 1);
        let song = &g.songs[0];
        assert_eq!(song.id, "The Beatles|yesterday");
        assert_eq!(song.title, "yesterday");
        assert_eq!(song.artist, "The Beatles");
        assert_eq!(song.members, vec!["h1", "h2", "h3"]);
        assert_eq!(song.canonical_hash, "h2");
        // Only the best member (h2) is canonical.
        assert_eq!(g.is_canonical, vec![false, true, false]);
    }

    #[test]
    fn null_quality_sorts_lowest_and_ties_break_by_hash() {
        // Two members: one has a quality score, one is null → the scored one wins
        // even though its hash is larger.
        let recs = vec![
            rec("aaa", Some("X"), Some("Song")),
            rec("zzz", Some("X"), Some("Song")),
        ];
        let g = group(&recs, &[None, Some(0.1)]);
        assert_eq!(g.songs[0].canonical_hash, "zzz");
        assert_eq!(g.is_canonical, vec![false, true]);

        // All-null quality → tie-break on smallest content_hash.
        let g2 = group(&recs, &[None, None]);
        assert_eq!(g2.songs[0].canonical_hash, "aaa");
        assert_eq!(g2.is_canonical, vec![true, false]);

        // Equal non-null quality → smallest content_hash wins.
        let g3 = group(&recs, &[Some(0.5), Some(0.5)]);
        assert_eq!(g3.songs[0].canonical_hash, "aaa");
    }

    #[test]
    fn lastfm_match_precedes_quality_then_plain_build_falls_back() {
        let recs = vec![
            rec("aaa", Some("X"), Some("Song")),
            rec("zzz", Some("X"), Some("Song")),
        ];
        let enriched = group_with_matches(&recs, &[Some(0.1), Some(0.9)], &[true, false]);
        assert_eq!(enriched.songs[0].canonical_hash, "aaa");
        let plain = group_with_matches(&recs, &[Some(0.1), Some(0.9)], &[false, false]);
        assert_eq!(plain.songs[0].canonical_hash, "zzz");
    }

    #[test]
    fn grouping_is_order_independent() {
        let recs = vec![
            rec("h1", Some("X"), Some("Song")),
            rec("h2", Some("X"), Some("Song - Mono")),
            rec("h3", Some("X"), Some("Other")),
        ];
        let quality = [Some(0.3), Some(0.8), Some(0.5)];
        let base = group(&recs, &quality);

        // Reverse the records AND the parallel quality slice together.
        let mut rev: Vec<AnalysisRecord> = recs.clone();
        rev.reverse();
        let mut rq: Vec<Option<f64>> = quality.to_vec();
        rq.reverse();
        // Re-sort by content_hash as the builder does, carrying quality along.
        let mut paired: Vec<(AnalysisRecord, Option<f64>)> = rev.into_iter().zip(rq).collect();
        paired.sort_by(|a, b| a.0.source.content_hash.cmp(&b.0.source.content_hash));
        let (rr, rrq): (Vec<_>, Vec<_>) = paired.into_iter().unzip();
        let other = group(&rr, &rrq);

        assert_eq!(base.songs.len(), other.songs.len());
        assert_eq!(base.songs[0].id, other.songs[0].id);
        assert_eq!(base.songs[0].canonical_hash, other.songs[0].canonical_hash);
    }

    fn sim(src: &str, tgt: &str) -> SimEdge {
        SimEdge {
            src: src.to_string(),
            tgt: tgt.to_string(),
            score: 0.01,
        }
    }

    fn refine(recs: &[AnalysisRecord], quality: &[Option<f64>], edges: &[SimEdge]) -> SongGrouping {
        let refs: Vec<&AnalysisRecord> = recs.iter().collect();
        let matches = vec![false; recs.len()];
        let primary = group_songs(&refs, quality, &matches);
        refine_songs(&refs, quality, &matches, &primary, edges)
    }

    fn song<'a>(grouping: &'a SongGrouping, id: &str) -> Option<&'a SongGroup> {
        grouping.songs.iter().find(|song| song.id == id)
    }

    #[test]
    fn refinement_requires_junk_unique_target_and_accepts_either_direction() {
        let recs = vec![
            rec("k1", Some("Known"), Some("Focus")),
            rec("k2", Some("Known"), Some("Focus - Live")),
            rec("u1", None, Some("Focus")),
            rec("u2", Some("Artiest onbekend"), Some("Focus")),
            rec("cover", Some("Cover Artist"), Some("Focus")),
            rec("noedge", None, Some("Focus")),
        ];
        let grouping = refine(
            &recs,
            &[Some(0.5); 6],
            &[sim("u1", "k1"), sim("k2", "u2"), sim("cover", "k1")],
        );
        let target = song(&grouping, "Known|focus").unwrap();
        assert_eq!(target.members, vec!["k1", "k2", "u1", "u2"]);
        assert!(song(&grouping, "Cover Artist|focus").is_none());
        assert!(
            grouping.is_canonical[4],
            "known cover stays a canonical singleton"
        );
        assert!(
            grouping.is_canonical[5],
            "junk candidate without an edge stays singleton"
        );
    }

    #[test]
    fn refinement_rejects_ambiguous_title_and_accepts_tjt_prefix() {
        let recs = vec![
            rec("a1", Some("Artist A"), Some("Shared")),
            rec("a2", Some("Artist A"), Some("Shared - Live")),
            rec("b1", Some("Artist B"), Some("Shared")),
            rec("b2", Some("Artist B"), Some("Shared - Demo")),
            rec("amb", None, Some("Shared")),
            rec("t1", Some("Artist A"), Some("Unique")),
            rec("t2", Some("Artist A"), Some("Unique - Live")),
            rec("tjt", Some("tJt110 The Beatles"), Some("Unique")),
        ];
        let grouping = refine(
            &recs,
            &[Some(0.5); 8],
            &[sim("amb", "a1"), sim("tjt", "t1")],
        );
        assert!(
            grouping.is_canonical[4],
            "two non-junk Songs make the title ambiguous"
        );
        assert_eq!(
            song(&grouping, "Artist A|unique").unwrap().members,
            vec!["t1", "t2", "tjt"]
        );
    }

    #[test]
    fn refinement_reassigns_junk_group_without_cascade_and_recomputes_canonical() {
        let recs = vec![
            rec("k1", Some("Known"), Some("Song")),
            rec("k2", Some("Known"), Some("Song - Live")),
            rec("j1", None, Some("Song")),
            rec("j2", None, Some("Song - Demo")),
            rec("j3", None, Some("Song - Mono")),
            rec("j4", None, Some("Song - Stereo")),
        ];
        let grouping = refine(
            &recs,
            &[
                Some(0.1),
                Some(0.2),
                Some(0.3),
                Some(0.9),
                Some(0.4),
                Some(0.5),
            ],
            &[sim("j1", "k1"), sim("k2", "j2"), sim("j3", "j1")],
        );
        let target = song(&grouping, "Known|song").unwrap();
        assert_eq!(target.members, vec!["k1", "k2", "j1", "j2"]);
        assert_eq!(
            target.canonical_hash, "j2",
            "assigned member participates in repick"
        );
        assert_eq!(
            song(&grouping, "Unknown Artist|song").unwrap().members,
            vec!["j3", "j4"],
            "edge to a reassigned candidate must not cascade"
        );
        assert_eq!(
            grouping.is_canonical,
            vec![false, false, false, true, false, true]
        );

        let collapsed = refine(
            &recs[..5],
            &[Some(0.1), Some(0.2), Some(0.3), Some(0.9), Some(0.4)],
            &[sim("j1", "k1"), sim("k2", "j2")],
        );
        assert!(song(&collapsed, "Unknown Artist|song").is_none());
        assert!(
            collapsed.is_canonical[4],
            "one residual member collapses to singleton"
        );
    }

    #[test]
    fn refinement_is_order_independent_after_builder_sorting() {
        let mut recs = vec![
            rec("k2", Some("Known"), Some("Song - Live")),
            rec("u1", None, Some("Song")),
            rec("k1", Some("Known"), Some("Song")),
        ];
        recs.sort_by(|a, b| a.source.content_hash.cmp(&b.source.content_hash));
        let base = refine(
            &recs,
            &[Some(0.2), Some(0.3), Some(0.1)],
            &[sim("u1", "k1")],
        );
        recs.reverse();
        recs.sort_by(|a, b| a.source.content_hash.cmp(&b.source.content_hash));
        let other = refine(
            &recs,
            &[Some(0.2), Some(0.3), Some(0.1)],
            &[sim("u1", "k1")],
        );
        assert_eq!(base.songs[0].members, other.songs[0].members);
        assert_eq!(base.songs[0].canonical_hash, other.songs[0].canonical_hash);
        assert_eq!(base.is_canonical, other.is_canonical);
    }
}
