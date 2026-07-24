//! The `AnalysisRecord` DTO — sonagram's persistable, serde-friendly superset of
//! sonara's [`TrackAnalysis`].
//!
//! `TrackAnalysis` deliberately derives nothing (no serde, no `Clone`), so
//! sonagram owns the DTO that makes an analysis result *persistable*. This one
//! type is used for two things at once:
//!
//! - the **analysis cache** — one JSON record per audio content hash under
//!   `<library>/.sonagram/analysis/` (P3), so the graph is always rebuilt from
//!   records and never from live analysis; and
//! - the **frozen fixtures** — captured records under `tests/fixtures/analyses/`
//!   that feed the deterministic graph gate (P5).
//!
//! Because both uses demand byte-identical output for identical input, the DTO
//! contains **no `HashMap`** anywhere: every collection is a `Vec` (sorted as
//! sonara emits it — `requested_features` in particular) or a scalar, so struct
//! field order alone fixes the serialization. `f32` is used throughout to match
//! sonara's [`Float`](sonara::Float).

use serde::{Deserialize, Serialize};
use sonara::analyze::TrackAnalysis;

use crate::{Result, SonagramError};

/// Version of *this* DTO / cache format. Distinct from sonara's
/// `ANALYSIS_SCHEMA_VERSION` (which describes the analysis semantics and lives
/// in [`ProvenanceDto::schema_version`]). Bump when the record layout changes in
/// a way that invalidates previously written caches or fixtures.
pub const RECORD_VERSION: u32 = 1;

/// A persistable analysis result: sonara's [`TrackAnalysis`] plus the file tags,
/// content hash and source metadata sonagram needs to build the graph.
///
/// Serialization is deterministic (struct field order, no maps), which is what
/// makes it safe to use as both the on-disk cache and the frozen gate fixtures.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AnalysisRecord {
    /// The DTO format version — [`RECORD_VERSION`] at write time.
    pub record_version: u32,
    /// Where the audio came from and how it was identified.
    pub source: SourceInfo,
    /// Container/stream tags, hoisted out of the analysis so the graph builder
    /// reads them from one place. `None` when the `tags` feature was not
    /// requested or the file carried none.
    pub tags: Option<TagsDto>,
    /// The mirrored analysis payload.
    pub analysis: AnalysisDto,
}

/// Identity and provenance of the source audio file.
///
/// `content_hash`/`path` are filled by the scanner (P3) or the capture bin; the
/// hash is what gives a `Track` its stable graph identity across retag + move.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SourceInfo {
    /// Hex-encoded content hash of the audio (see `hash_kind` for what was
    /// hashed).
    pub content_hash: String,
    /// How `content_hash` was computed. `"whole-file-v0"` hashes the entire
    /// file bytes (P2). P3 introduces ID3-stripped hashing under a new kind;
    /// records tagged with the old kind get regenerated then.
    pub hash_kind: String,
    /// File name only — never a user directory (privacy). A mutable property in
    /// the graph.
    pub path: String,
    /// File size in bytes, from disk.
    pub file_size: u64,
    /// Container format, e.g. `"mp3"`.
    pub format: String,
}

/// File tags, mirroring sonara's `TrackTags`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TagsDto {
    /// Track title.
    pub title: Option<String>,
    /// Track artist.
    pub artist: Option<String>,
    /// Album name.
    pub album: Option<String>,
    /// Genre string as stored in the file.
    pub genre: Option<String>,
    /// Release year of *this* file/edition (sonara's `tags.year`). On a reissue
    /// or compilation this is the reissue date — see [`original_year`](Self::original_year).
    pub year: Option<u32>,
    /// Original release year (sonara's `tags.original_year`, from ID3v2.4 `TDOR` /
    /// v2.3 `TORY` / Vorbis `ORIGINALDATE`). `None` when the file carries no such
    /// tag (there is no fallback to `year`). Additive since sonara 0.2.4 —
    /// `#[serde(default)]` so pre-0.2.4 cached records (which lack the field) still
    /// deserialize as `None`.
    #[serde(default)]
    pub original_year: Option<u32>,
    /// Track number.
    pub track_no: Option<u32>,
}

/// How an analysis result was produced, mirroring sonara's
/// `AnalysisProvenance`. Carries the schema version, sample rate and hop length
/// needed to interpret frame-index fields (`beats`, `onset_frames`,
/// `downbeats`) as seconds, plus the mode and requested feature list.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProvenanceDto {
    /// Value of sonara's `ANALYSIS_SCHEMA_VERSION` at analysis time.
    pub schema_version: u32,
    /// Effective sample rate (Hz) the analyzed signal was at.
    pub sample_rate: u32,
    /// STFT hop length (samples) of the main pass.
    pub hop_length: usize,
    /// The configured analysis mode, canonical lowercase name
    /// (`"compact"`/`"playlist"`/`"full"`).
    pub mode: String,
    /// The explicit `features=[...]` request (sorted by sonara), if one was
    /// given.
    pub requested_features: Option<Vec<String>>,
    /// Identity of the genre model that produced `genre`/`genre_confidence`.
    /// `None` means no identified genre model ran. Additive since sonara 0.2.5;
    /// omitted for legacy/no-model records so existing fixture bytes remain
    /// stable while older records still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre_model_id: Option<String>,
    /// Identity of the vocalness model that produced
    /// `vocalness`/`instrumentalness`. `None` means sonara's schema-versioned
    /// built-in heuristic produced the scores. Additive since sonara 0.2.5;
    /// omitted for legacy/no-model records and defaulted when reading them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocalness_model_id: Option<String>,
    /// Identity of the fused aggression model that produced the aggression
    /// rank and diagnostics. Present exactly when `aggression` was requested.
    /// Additive since sonara 0.3.1; absent legacy records deserialize so the
    /// scanner can classify them stale and re-analyze their audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggression_model_id: Option<String>,
}

/// A time-spanned chord event, mirroring sonara's `ChordEvent`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ChordEventDto {
    /// Chord label (`chord_sequence` vocabulary; `"N"` = no chord).
    pub label: String,
    /// Start time in seconds.
    pub start_sec: f32,
    /// End time in seconds.
    pub end_sec: f32,
}

/// A structural section, mirroring sonara's `SegmentEvent`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SegmentEventDto {
    /// Start time in seconds.
    pub start_sec: f32,
    /// End time in seconds.
    pub end_sec: f32,
    /// Mean perceptual energy (0-1) over the span.
    pub energy: f32,
}

/// The full mirror of sonara's [`TrackAnalysis`], minus `tags` (hoisted to
/// [`AnalysisRecord::tags`]). Every field is mirrored so nothing an agent might
/// query is silently dropped; optional fields stay `Option` exactly as sonara
/// populates them per the requested feature set.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AnalysisDto {
    // -- Basic (always computed) --
    /// How this result was produced.
    pub provenance: ProvenanceDto,
    pub duration_sec: f32,
    pub bpm: f32,
    pub bpm_raw: f32,
    /// How firmly the tempo estimate is anchored in the audio ([0,1]) — sonara's
    /// always-present `bpm_confidence`. Low (<0.45) flags ambient/rubato material
    /// where BPM is unreliable. Additive since sonara 0.2.4 — `#[serde(default)]`
    /// so pre-0.2.4 cached records (which lack the field) still deserialize (to
    /// `0.0`, treated as "unknown/low trust").
    #[serde(default)]
    pub bpm_confidence: f32,
    /// Strongest tempo candidates as `(bpm, score)` pairs.
    pub bpm_candidates: Vec<(f32, f32)>,
    /// Beat positions as main-pass frame indices.
    pub beats: Vec<usize>,
    /// Onset positions as main-pass frame indices.
    pub onset_frames: Vec<usize>,
    pub rms_mean: f32,
    pub rms_max: f32,
    pub loudness_lufs: f32,
    pub dynamic_range_db: f32,

    // -- Extended loudness (opt-in "loudness") --
    pub true_peak_db: Option<f32>,
    pub replaygain_db: Option<f32>,
    pub loudness_curve: Option<Vec<f32>>,
    pub loudness_momentary_max_db: Option<f32>,
    pub loudness_range_lu: Option<f32>,

    pub spectral_centroid_mean: f32,
    pub zero_crossing_rate: f32,
    pub onset_density: f32,

    // -- Extended spectral --
    pub spectral_bandwidth_mean: Option<f32>,
    pub spectral_rolloff_mean: Option<f32>,
    pub spectral_flatness_mean: Option<f32>,
    pub spectral_contrast_mean: Option<Vec<f32>>,
    pub mfcc_mean: Option<Vec<f32>>,
    pub chroma_mean: Option<Vec<f32>>,

    // -- Rhythm --
    pub tempo_curve: Option<Vec<f32>>,
    pub tempo_variability: Option<f32>,
    pub time_signature: Option<String>,
    pub time_signature_confidence: Option<f32>,

    // -- Tonal --
    pub chord_sequence: Option<Vec<String>>,
    pub chord_events: Option<Vec<ChordEventDto>>,
    pub chord_change_rate: Option<f32>,
    pub predominant_chord: Option<String>,
    pub dissonance: Option<f32>,

    // -- Perceptual --
    pub energy: Option<f32>,
    pub danceability: Option<f32>,
    pub key: Option<String>,
    pub key_confidence: Option<f32>,
    pub key_camelot: Option<String>,
    pub valence: Option<f32>,
    pub acousticness: Option<f32>,

    // -- Embedding --
    pub embedding: Option<Vec<f32>>,

    // -- Fused aggression rank (opt-in "aggression") --
    /// Perceptual aggression rank in `[0, 1]`, not a probability. `None` with
    /// complete diagnostics means Sonara abstained for insufficient evidence.
    #[serde(default)]
    pub aggression_score: Option<f32>,
    /// Independent musical-evidence support in `[0, 1]`, not rank certainty.
    #[serde(default)]
    pub aggression_confidence: Option<f32>,
    #[serde(default)]
    pub aggression_forcefulness: Option<f32>,
    #[serde(default)]
    pub aggression_harshness: Option<f32>,
    #[serde(default)]
    pub aggression_tension: Option<f32>,
    #[serde(default)]
    pub aggression_rhythm: Option<f32>,

    // -- Mood + instrumentalness (heuristic v1, opt-in) --
    pub mood_happy: Option<f32>,
    pub mood_aggressive: Option<f32>,
    pub mood_relaxed: Option<f32>,
    pub mood_sad: Option<f32>,
    pub instrumentalness: Option<f32>,
    /// Predicted genre label — populated only when a user-supplied sonara genre
    /// model is set; `None` otherwise (sonara ships no model).
    pub genre: Option<String>,
    /// Confidence (softmax probability) of the predicted [`genre`](Self::genre);
    /// `None` when no genre was predicted. Additive since sonara 0.2.3 —
    /// `#[serde(default)]` so pre-0.2.3 cached records (which lack the field)
    /// still deserialize as `None`.
    #[serde(default)]
    pub genre_confidence: Option<f32>,

    // -- Beat grid (opt-in "beatgrid") --
    pub grid_offset_sec: Option<f32>,
    pub downbeats: Option<Vec<usize>>,
    pub grid_stability: Option<f32>,

    // -- Structure (opt-in "structure") --
    pub energy_curve: Option<Vec<f32>>,
    pub energy_curve_hop_sec: Option<f32>,
    pub segments: Option<Vec<SegmentEventDto>>,
    pub intro_end_sec: Option<f32>,
    pub outro_start_sec: Option<f32>,
    pub energy_level: Option<u8>,

    // -- Silence (opt-in "silence") --
    pub leading_silence_sec: Option<f32>,
    pub trailing_silence_sec: Option<f32>,

    // -- Key candidates (opt-in "key_candidates") --
    /// Top-3 key candidates as `(key, camelot, score)`.
    pub key_candidates: Option<Vec<(String, String, f32)>>,

    // -- Vocalness (opt-in "vocalness") --
    pub vocalness: Option<f32>,

    // -- Fingerprint (opt-in "fingerprint") --
    pub fingerprint: Option<Vec<u32>>,

    // -- Embedding version (Some iff embedding is Some) --
    pub embedding_version: Option<u32>,
}

impl AnalysisRecord {
    /// Build a record by **moving** a `TrackAnalysis` into it (the type has no
    /// `Clone`), pairing it with the `source` identity the scanner/capture bin
    /// computed. Tags are extracted from `a.tags` into [`AnalysisRecord::tags`].
    ///
    /// Every `TrackAnalysis` field is named here, so the compiler flags any
    /// upstream field that stops being mirrored.
    pub fn from_analysis(a: TrackAnalysis, source: SourceInfo) -> Self {
        let TrackAnalysis {
            provenance,
            duration_sec,
            bpm,
            bpm_raw,
            bpm_confidence,
            bpm_candidates,
            beats,
            onset_frames,
            rms_mean,
            rms_max,
            loudness_lufs,
            dynamic_range_db,
            true_peak_db,
            replaygain_db,
            loudness_curve,
            loudness_momentary_max_db,
            loudness_range_lu,
            spectral_centroid_mean,
            zero_crossing_rate,
            onset_density,
            spectral_bandwidth_mean,
            spectral_rolloff_mean,
            spectral_flatness_mean,
            spectral_contrast_mean,
            mfcc_mean,
            chroma_mean,
            tempo_curve,
            tempo_variability,
            time_signature,
            time_signature_confidence,
            chord_sequence,
            chord_events,
            chord_change_rate,
            predominant_chord,
            dissonance,
            energy,
            danceability,
            key,
            key_confidence,
            key_camelot,
            valence,
            acousticness,
            embedding,
            aggression_score,
            aggression_confidence,
            aggression_forcefulness,
            aggression_harshness,
            aggression_tension,
            aggression_rhythm,
            mood_happy,
            mood_aggressive,
            mood_relaxed,
            mood_sad,
            instrumentalness,
            genre,
            genre_confidence,
            grid_offset_sec,
            downbeats,
            grid_stability,
            energy_curve,
            energy_curve_hop_sec,
            segments,
            intro_end_sec,
            outro_start_sec,
            energy_level,
            leading_silence_sec,
            trailing_silence_sec,
            key_candidates,
            vocalness,
            fingerprint,
            embedding_version,
            tags,
        } = a;

        let tags = tags.map(|t| TagsDto {
            title: t.title,
            artist: t.artist,
            album: t.album,
            genre: t.genre,
            year: t.year,
            original_year: t.original_year,
            track_no: t.track_no,
        });

        let provenance = ProvenanceDto {
            schema_version: provenance.schema_version,
            sample_rate: provenance.sample_rate,
            hop_length: provenance.hop_length,
            mode: provenance.mode.as_str().to_string(),
            requested_features: provenance.requested_features,
            genre_model_id: provenance.genre_model_id,
            vocalness_model_id: provenance.vocalness_model_id,
            aggression_model_id: provenance.aggression_model_id,
        };

        let chord_events = chord_events.map(|evs| {
            evs.into_iter()
                .map(|e| ChordEventDto {
                    label: e.label,
                    start_sec: e.start_sec,
                    end_sec: e.end_sec,
                })
                .collect()
        });

        let segments = segments.map(|segs| {
            segs.into_iter()
                .map(|s| SegmentEventDto {
                    start_sec: s.start_sec,
                    end_sec: s.end_sec,
                    energy: s.energy,
                })
                .collect()
        });

        let analysis = AnalysisDto {
            provenance,
            duration_sec,
            bpm,
            bpm_raw,
            bpm_confidence,
            bpm_candidates,
            beats,
            onset_frames,
            rms_mean,
            rms_max,
            loudness_lufs,
            dynamic_range_db,
            true_peak_db,
            replaygain_db,
            loudness_curve,
            loudness_momentary_max_db,
            loudness_range_lu,
            spectral_centroid_mean,
            zero_crossing_rate,
            onset_density,
            spectral_bandwidth_mean,
            spectral_rolloff_mean,
            spectral_flatness_mean,
            spectral_contrast_mean,
            mfcc_mean,
            chroma_mean,
            tempo_curve,
            tempo_variability,
            time_signature,
            time_signature_confidence,
            chord_sequence,
            chord_events,
            chord_change_rate,
            predominant_chord,
            dissonance,
            energy,
            danceability,
            key,
            key_confidence,
            key_camelot,
            valence,
            acousticness,
            embedding,
            aggression_score,
            aggression_confidence,
            aggression_forcefulness,
            aggression_harshness,
            aggression_tension,
            aggression_rhythm,
            mood_happy,
            mood_aggressive,
            mood_relaxed,
            mood_sad,
            instrumentalness,
            genre,
            genre_confidence,
            grid_offset_sec,
            downbeats,
            grid_stability,
            energy_curve,
            energy_curve_hop_sec,
            segments,
            intro_end_sec,
            outro_start_sec,
            energy_level,
            leading_silence_sec,
            trailing_silence_sec,
            key_candidates,
            vocalness,
            fingerprint,
            embedding_version,
        };

        AnalysisRecord {
            record_version: RECORD_VERSION,
            source,
            tags,
            analysis,
        }
    }

    /// Serialize to pretty-printed JSON (the on-disk cache / fixture form).
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| SonagramError::Cache(e.to_string()))
    }

    /// Parse a record from JSON.
    pub fn from_json(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(|e| SonagramError::Cache(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record with every optional field populated, for exercising the full
    /// serialization surface.
    fn full_record() -> AnalysisRecord {
        AnalysisRecord {
            record_version: RECORD_VERSION,
            source: SourceInfo {
                content_hash: "abcdef0123456789".to_string(),
                hash_kind: "whole-file-v0".to_string(),
                path: "track.mp3".to_string(),
                file_size: 8_675_309,
                format: "mp3".to_string(),
            },
            tags: Some(TagsDto {
                title: Some("Tin Man".to_string()),
                artist: Some("America".to_string()),
                album: Some("Homecoming".to_string()),
                genre: Some("Rock".to_string()),
                year: Some(1974),
                original_year: Some(1972),
                track_no: Some(2),
            }),
            analysis: AnalysisDto {
                provenance: ProvenanceDto {
                    schema_version: 1,
                    sample_rate: 22050,
                    hop_length: 512,
                    mode: "playlist".to_string(),
                    requested_features: Some(vec![
                        "chords".to_string(),
                        "embedding".to_string(),
                        "energy".to_string(),
                    ]),
                    genre_model_id: Some("genre-model-v1".to_string()),
                    vocalness_model_id: Some("sonara-vocalness-v2".to_string()),
                    aggression_model_id: Some("aggression-rank-v3-sr22050".to_string()),
                },
                duration_sec: 210.5,
                bpm: 119.7,
                bpm_raw: 119.7,
                bpm_confidence: 0.83,
                bpm_candidates: vec![(119.7, 0.9), (59.85, 0.4)],
                beats: vec![10, 22, 34, 46],
                onset_frames: vec![5, 12, 20],
                rms_mean: 0.13,
                rms_max: 0.41,
                loudness_lufs: -12.3,
                dynamic_range_db: 9.5,
                true_peak_db: Some(-0.8),
                replaygain_db: Some(-5.7),
                loudness_curve: Some(vec![-14.0, -13.2, -12.9]),
                loudness_momentary_max_db: Some(-8.1),
                loudness_range_lu: Some(6.4),
                spectral_centroid_mean: 2200.0,
                zero_crossing_rate: 0.08,
                onset_density: 1.7,
                spectral_bandwidth_mean: Some(1800.0),
                spectral_rolloff_mean: Some(4200.0),
                spectral_flatness_mean: Some(0.02),
                spectral_contrast_mean: Some(vec![12.0, 14.5, 10.1]),
                mfcc_mean: Some(vec![-120.0, 50.0, -3.0]),
                chroma_mean: Some(vec![0.5, 0.1, 0.2]),
                tempo_curve: Some(vec![119.0, 120.0, 119.5]),
                tempo_variability: Some(0.4),
                time_signature: Some("4/4".to_string()),
                time_signature_confidence: Some(0.88),
                chord_sequence: Some(vec!["C".to_string(), "Am".to_string(), "N".to_string()]),
                chord_events: Some(vec![
                    ChordEventDto {
                        label: "C".to_string(),
                        start_sec: 0.0,
                        end_sec: 4.2,
                    },
                    ChordEventDto {
                        label: "Am".to_string(),
                        start_sec: 4.2,
                        end_sec: 8.4,
                    },
                ]),
                chord_change_rate: Some(0.3),
                predominant_chord: Some("C".to_string()),
                dissonance: Some(0.22),
                energy: Some(0.55),
                danceability: Some(0.61),
                key: Some("A minor".to_string()),
                key_confidence: Some(0.77),
                key_camelot: Some("8A".to_string()),
                valence: Some(0.42),
                acousticness: Some(0.71),
                embedding: Some(vec![0.1; 48]),
                aggression_score: Some(0.63),
                aggression_confidence: Some(0.91),
                aggression_forcefulness: Some(0.74),
                aggression_harshness: Some(0.52),
                aggression_tension: Some(0.67),
                aggression_rhythm: Some(0.58),
                mood_happy: Some(0.3),
                mood_aggressive: Some(0.1),
                mood_relaxed: Some(0.7),
                mood_sad: Some(0.4),
                instrumentalness: Some(0.2),
                genre: None,
                genre_confidence: None,
                grid_offset_sec: Some(0.12),
                downbeats: Some(vec![10, 46]),
                grid_stability: Some(0.9),
                energy_curve: Some(vec![0.4, 0.5, 0.6]),
                energy_curve_hop_sec: Some(1.0),
                segments: Some(vec![
                    SegmentEventDto {
                        start_sec: 0.0,
                        end_sec: 30.0,
                        energy: 0.3,
                    },
                    SegmentEventDto {
                        start_sec: 30.0,
                        end_sec: 210.5,
                        energy: 0.6,
                    },
                ]),
                intro_end_sec: Some(12.0),
                outro_start_sec: Some(190.0),
                energy_level: Some(5),
                leading_silence_sec: Some(0.5),
                trailing_silence_sec: Some(1.2),
                key_candidates: Some(vec![
                    ("A minor".to_string(), "8A".to_string(), 0.77),
                    ("C major".to_string(), "8B".to_string(), 0.71),
                ]),
                vocalness: Some(0.8),
                fingerprint: Some(vec![1, 2, 3, 4]),
                embedding_version: Some(1),
            },
        }
    }

    /// A record with every optional field left `None` / empty.
    fn minimal_record() -> AnalysisRecord {
        AnalysisRecord {
            record_version: RECORD_VERSION,
            source: SourceInfo {
                content_hash: "00".to_string(),
                hash_kind: "whole-file-v0".to_string(),
                path: "x.mp3".to_string(),
                file_size: 0,
                format: "mp3".to_string(),
            },
            tags: None,
            analysis: AnalysisDto {
                provenance: ProvenanceDto {
                    schema_version: 1,
                    sample_rate: 22050,
                    hop_length: 512,
                    mode: "compact".to_string(),
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
            },
        }
    }

    #[test]
    fn full_record_json_round_trip() {
        let rec = full_record();
        let json = rec.to_json_pretty().unwrap();
        let back = AnalysisRecord::from_json(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn minimal_record_json_round_trip() {
        let rec = minimal_record();
        let json = rec.to_json_pretty().unwrap();
        let back = AnalysisRecord::from_json(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn model_ids_round_trip_losslessly() {
        let rec = full_record();
        let json = rec.to_json_pretty().unwrap();
        assert!(json.contains(r#""genre_model_id": "genre-model-v1""#));
        assert!(json.contains(r#""vocalness_model_id": "sonara-vocalness-v2""#));
        assert!(json.contains(r#""aggression_model_id": "aggression-rank-v3-sr22050""#));

        let back = AnalysisRecord::from_json(&json).unwrap();
        assert_eq!(
            back.analysis.provenance.genre_model_id.as_deref(),
            Some("genre-model-v1")
        );
        assert_eq!(
            back.analysis.provenance.vocalness_model_id.as_deref(),
            Some("sonara-vocalness-v2")
        );
        assert_eq!(
            back.analysis.provenance.aggression_model_id.as_deref(),
            Some("aggression-rank-v3-sr22050")
        );
    }

    #[test]
    fn legacy_provenance_without_model_ids_deserializes_and_stays_compact() {
        let mut value = serde_json::to_value(minimal_record()).unwrap();
        let provenance = value["analysis"]["provenance"].as_object_mut().unwrap();
        assert!(!provenance.contains_key("genre_model_id"));
        assert!(!provenance.contains_key("vocalness_model_id"));
        assert!(!provenance.contains_key("aggression_model_id"));

        let legacy: AnalysisRecord = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.analysis.provenance.genre_model_id, None);
        assert_eq!(legacy.analysis.provenance.vocalness_model_id, None);
        assert_eq!(legacy.analysis.provenance.aggression_model_id, None);
    }

    #[test]
    fn serialization_is_stable() {
        // Same record serialized twice is byte-identical (no map iteration).
        let rec = full_record();
        assert_eq!(rec.to_json_pretty().unwrap(), rec.to_json_pretty().unwrap());
    }

    #[test]
    fn v1_record_without_genre_confidence_deserializes() {
        // A pre-sonara-0.2.3 cached record has no `genre_confidence` key at all.
        // RECORD_VERSION stays 1 (the field is additive), so such a record MUST
        // still parse — the missing field defaults to `None` (serde default).
        let json = r#"{
            "record_version": 1,
            "source": {
                "content_hash": "deadbeef",
                "hash_kind": "mp3-audio-v1",
                "path": "old.mp3",
                "file_size": 42,
                "format": "mp3"
            },
            "tags": null,
            "analysis": {
                "provenance": {
                    "schema_version": 1,
                    "sample_rate": 22050,
                    "hop_length": 512,
                    "mode": "playlist",
                    "requested_features": null
                },
                "duration_sec": 1.0,
                "bpm": 0.0,
                "bpm_raw": 0.0,
                "bpm_candidates": [],
                "beats": [],
                "onset_frames": [],
                "rms_mean": 0.0,
                "rms_max": 0.0,
                "loudness_lufs": 0.0,
                "dynamic_range_db": 0.0,
                "true_peak_db": null,
                "replaygain_db": null,
                "loudness_curve": null,
                "loudness_momentary_max_db": null,
                "loudness_range_lu": null,
                "spectral_centroid_mean": 0.0,
                "zero_crossing_rate": 0.0,
                "onset_density": 0.0,
                "spectral_bandwidth_mean": null,
                "spectral_rolloff_mean": null,
                "spectral_flatness_mean": null,
                "spectral_contrast_mean": null,
                "mfcc_mean": null,
                "chroma_mean": null,
                "tempo_curve": null,
                "tempo_variability": null,
                "time_signature": null,
                "time_signature_confidence": null,
                "chord_sequence": null,
                "chord_events": null,
                "chord_change_rate": null,
                "predominant_chord": null,
                "dissonance": null,
                "energy": null,
                "danceability": null,
                "key": null,
                "key_confidence": null,
                "key_camelot": null,
                "valence": null,
                "acousticness": null,
                "embedding": null,
                "mood_happy": null,
                "mood_aggressive": null,
                "mood_relaxed": null,
                "mood_sad": null,
                "instrumentalness": null,
                "genre": null,
                "grid_offset_sec": null,
                "downbeats": null,
                "grid_stability": null,
                "energy_curve": null,
                "energy_curve_hop_sec": null,
                "segments": null,
                "intro_end_sec": null,
                "outro_start_sec": null,
                "energy_level": null,
                "leading_silence_sec": null,
                "trailing_silence_sec": null,
                "key_candidates": null,
                "vocalness": null,
                "fingerprint": null,
                "embedding_version": null
            }
        }"#;
        let rec = AnalysisRecord::from_json(json).expect("v1 record must still parse");
        assert_eq!(rec.analysis.genre_confidence, None);
    }

    #[test]
    fn pre_0_2_4_record_without_new_fields_deserializes() {
        // A pre-sonara-0.2.4 cached record has no `analysis.bpm_confidence` key and
        // no `tags.original_year` key. RECORD_VERSION stays 1 (both fields are
        // additive), so such a record MUST still parse — the missing analysis
        // scalar defaults to `0.0` and the missing tag to `None` (serde defaults).
        // Tags ARE present here (unlike the genre_confidence test) so the missing
        // `original_year` key is exercised in a real tag block.
        let json = r#"{
            "record_version": 1,
            "source": {
                "content_hash": "cafef00d",
                "hash_kind": "mp3-audio-v1",
                "path": "old.mp3",
                "file_size": 42,
                "format": "mp3"
            },
            "tags": {
                "title": "Old Song",
                "artist": "Old Artist",
                "album": "Old Album",
                "genre": "Rock",
                "year": 1985,
                "track_no": 3
            },
            "analysis": {
                "provenance": {
                    "schema_version": 2,
                    "sample_rate": 22050,
                    "hop_length": 512,
                    "mode": "playlist",
                    "requested_features": null
                },
                "duration_sec": 200.0,
                "bpm": 120.0,
                "bpm_raw": 120.0,
                "bpm_candidates": [],
                "beats": [],
                "onset_frames": [],
                "rms_mean": 0.1,
                "rms_max": 0.3,
                "loudness_lufs": -10.0,
                "dynamic_range_db": 8.0,
                "true_peak_db": null,
                "replaygain_db": null,
                "loudness_curve": null,
                "loudness_momentary_max_db": null,
                "loudness_range_lu": null,
                "spectral_centroid_mean": 2000.0,
                "zero_crossing_rate": 0.05,
                "onset_density": 1.2,
                "spectral_bandwidth_mean": null,
                "spectral_rolloff_mean": null,
                "spectral_flatness_mean": null,
                "spectral_contrast_mean": null,
                "mfcc_mean": null,
                "chroma_mean": null,
                "tempo_curve": null,
                "tempo_variability": null,
                "time_signature": null,
                "time_signature_confidence": null,
                "chord_sequence": null,
                "chord_events": null,
                "chord_change_rate": null,
                "predominant_chord": null,
                "dissonance": null,
                "energy": null,
                "danceability": null,
                "key": null,
                "key_confidence": null,
                "key_camelot": null,
                "valence": null,
                "acousticness": null,
                "embedding": null,
                "mood_happy": null,
                "mood_aggressive": null,
                "mood_relaxed": null,
                "mood_sad": null,
                "instrumentalness": null,
                "genre": null,
                "grid_offset_sec": null,
                "downbeats": null,
                "grid_stability": null,
                "energy_curve": null,
                "energy_curve_hop_sec": null,
                "segments": null,
                "intro_end_sec": null,
                "outro_start_sec": null,
                "energy_level": null,
                "leading_silence_sec": null,
                "trailing_silence_sec": null,
                "key_candidates": null,
                "vocalness": null,
                "fingerprint": null,
                "embedding_version": null
            }
        }"#;
        let rec = AnalysisRecord::from_json(json).expect("pre-0.2.4 record must still parse");
        assert_eq!(rec.analysis.bpm_confidence, 0.0);
        assert_eq!(rec.tags.unwrap().original_year, None);
    }

    #[test]
    fn record_version_is_pinned() {
        assert_eq!(RECORD_VERSION, 1);
        assert_eq!(full_record().record_version, 1);
    }

    #[test]
    fn provenance_carries_analysis_context() {
        let p = full_record().analysis.provenance;
        assert_eq!(p.schema_version, 1);
        assert_eq!(p.sample_rate, 22050);
        assert_eq!(p.hop_length, 512);
        assert_eq!(p.mode, "playlist");
        assert_eq!(
            p.requested_features,
            Some(vec![
                "chords".to_string(),
                "embedding".to_string(),
                "energy".to_string(),
            ])
        );
    }
}
