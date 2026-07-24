use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const CURATION_POLICY_VERSION: u32 = 1;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistPreset {
    #[default]
    General,
    Focus,
    Party,
    Workout,
    Chill,
    Discovery,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum FamiliarityPreference {
    Avoid,
    #[default]
    Neutral,
    Prefer,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum SeedSimilarityPreference {
    Avoid,
    #[default]
    Neutral,
    Prefer,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum SeedRole {
    /// Seeds are required playlist entries, preserving the v1 behaviour.
    #[default]
    Pinned,
    /// Seeds are references only and are never exported as playlist entries.
    Reference,
    /// Seeds are both required entries and references for relative targets.
    PinnedAndReference,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum RelativeDirection {
    Lower,
    Similar,
    #[default]
    Any,
    Higher,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistArc {
    None,
    #[default]
    Flat,
    Rise,
    Fall,
    RiseAndFall,
}

fn default_target_tracks() -> usize {
    25
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaylistBrief {
    #[serde(default)]
    pub preset: PlaylistPreset,
    #[serde(default = "default_target_tracks")]
    pub target_tracks: usize,
    #[serde(default)]
    pub target_duration_sec: Option<u64>,
    #[serde(default)]
    pub seed_ids: Vec<String>,
    #[serde(default)]
    pub seed_role: SeedRole,
    /// Intent the typed v1 contract cannot represent. Non-empty means the
    /// library must return a structured non-exportable result, never guess.
    #[serde(default)]
    pub unsupported_intents: Vec<String>,
}

impl Default for PlaylistBrief {
    fn default() -> Self {
        Self {
            preset: PlaylistPreset::General,
            target_tracks: default_target_tracks(),
            target_duration_sec: None,
            seed_ids: Vec::new(),
            seed_role: SeedRole::Pinned,
            unsupported_intents: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EligibilityPolicy {
    pub require_music: bool,
    pub require_canonical: bool,
    pub allow_low_quality: bool,
    pub min_duration_sec: Option<f64>,
    pub max_duration_sec: Option<f64>,
    pub max_vocalness: Option<f64>,
    #[serde(default)]
    pub min_aggression: Option<f64>,
    #[serde(default)]
    pub max_aggression: Option<f64>,
    pub min_energy: Option<f64>,
    pub max_energy: Option<f64>,
    pub min_arousal: Option<f64>,
    pub max_arousal: Option<f64>,
    pub min_tension: Option<f64>,
    pub max_tension: Option<f64>,
    #[serde(default)]
    pub include_artists: Vec<String>,
    #[serde(default)]
    pub exclude_artists: Vec<String>,
    #[serde(default)]
    pub include_genres: Vec<String>,
    #[serde(default)]
    pub exclude_genres: Vec<String>,
    #[serde(default)]
    pub include_styles: Vec<String>,
    #[serde(default)]
    pub exclude_styles: Vec<String>,
    #[serde(default)]
    pub include_decades: Vec<String>,
    #[serde(default)]
    pub exclude_decades: Vec<String>,
    #[serde(default)]
    pub min_year: Option<i64>,
    #[serde(default)]
    pub max_year: Option<i64>,
}

impl Default for EligibilityPolicy {
    fn default() -> Self {
        Self {
            require_music: true,
            require_canonical: true,
            allow_low_quality: false,
            min_duration_sec: Some(60.0),
            max_duration_sec: Some(600.0),
            max_vocalness: None,
            min_aggression: None,
            max_aggression: None,
            min_energy: None,
            max_energy: None,
            min_arousal: None,
            max_arousal: None,
            min_tension: None,
            max_tension: None,
            include_artists: Vec::new(),
            exclude_artists: Vec::new(),
            include_genres: Vec::new(),
            exclude_genres: Vec::new(),
            include_styles: Vec::new(),
            exclude_styles: Vec::new(),
            include_decades: Vec::new(),
            exclude_decades: Vec::new(),
            min_year: None,
            max_year: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiversityPolicy {
    pub max_per_artist: usize,
    pub max_per_album: usize,
    pub max_per_song: usize,
    pub min_artist_gap: usize,
}

impl Default for DiversityPolicy {
    fn default() -> Self {
        Self {
            max_per_artist: 2,
            max_per_album: 2,
            max_per_song: 1,
            min_artist_gap: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeatureTargets {
    pub energy: Option<f64>,
    pub arousal: Option<f64>,
    pub tension: Option<f64>,
    pub vocalness: Option<f64>,
    #[serde(default)]
    pub aggression: Option<f64>,
    pub familiarity: FamiliarityPreference,
    #[serde(default)]
    pub seed_similarity: SeedSimilarityPreference,
    #[serde(default)]
    pub min_seed_similarity: Option<f64>,
    #[serde(default)]
    pub relative_energy: RelativeDirection,
    #[serde(default)]
    pub relative_energy_margin: f64,
    #[serde(default)]
    pub relative_arousal: RelativeDirection,
    #[serde(default)]
    pub relative_arousal_margin: f64,
    #[serde(default)]
    pub relative_tension: RelativeDirection,
    #[serde(default)]
    pub relative_tension_margin: f64,
    #[serde(default)]
    pub relative_vocalness: RelativeDirection,
    #[serde(default)]
    pub relative_vocalness_margin: f64,
    #[serde(default)]
    pub relative_aggression: RelativeDirection,
    #[serde(default)]
    pub relative_aggression_margin: f64,
}

impl Default for FeatureTargets {
    fn default() -> Self {
        Self {
            energy: None,
            arousal: None,
            tension: None,
            vocalness: None,
            aggression: None,
            familiarity: FamiliarityPreference::Neutral,
            seed_similarity: SeedSimilarityPreference::Neutral,
            min_seed_similarity: None,
            relative_energy: RelativeDirection::Any,
            relative_energy_margin: 0.0,
            relative_arousal: RelativeDirection::Any,
            relative_arousal_margin: 0.0,
            relative_tension: RelativeDirection::Any,
            relative_tension_margin: 0.0,
            relative_vocalness: RelativeDirection::Any,
            relative_vocalness_margin: 0.0,
            relative_aggression: RelativeDirection::Any,
            relative_aggression_margin: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransitionPolicy {
    pub embedding_weight: f64,
    pub feature_weight: f64,
    pub tempo_weight: f64,
    pub key_weight: f64,
    pub same_artist_penalty: f64,
    pub arc: PlaylistArc,
}

impl Default for TransitionPolicy {
    fn default() -> Self {
        Self {
            embedding_weight: 0.55,
            feature_weight: 0.25,
            tempo_weight: 0.10,
            key_weight: 0.10,
            same_artist_penalty: 0.20,
            arc: PlaylistArc::Flat,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditThresholds {
    pub min_unique_artist_ratio: f64,
    pub max_artist_share: f64,
    pub max_album_share: f64,
    pub min_mean_transition_score: f64,
    pub min_worst_transition_score: f64,
    pub max_mean_arc_error: f64,
}

impl Default for AuditThresholds {
    fn default() -> Self {
        Self {
            min_unique_artist_ratio: 0.50,
            max_artist_share: 0.15,
            max_album_share: 0.20,
            min_mean_transition_score: 0.45,
            min_worst_transition_score: 0.20,
            max_mean_arc_error: 0.25,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaylistPolicy {
    pub version: u32,
    pub preset: PlaylistPreset,
    pub eligibility: EligibilityPolicy,
    pub diversity: DiversityPolicy,
    pub targets: FeatureTargets,
    pub transition: TransitionPolicy,
    pub audit: AuditThresholds,
}

impl PlaylistPolicy {
    pub fn for_preset(preset: PlaylistPreset) -> Self {
        let mut policy = Self {
            version: CURATION_POLICY_VERSION,
            preset,
            eligibility: EligibilityPolicy::default(),
            diversity: DiversityPolicy::default(),
            targets: FeatureTargets::default(),
            transition: TransitionPolicy::default(),
            audit: AuditThresholds::default(),
        };
        match preset {
            PlaylistPreset::General => {}
            PlaylistPreset::Focus => {
                policy.eligibility.max_duration_sec = Some(900.0);
                policy.eligibility.max_vocalness = Some(0.35);
                policy.eligibility.max_energy = Some(0.60);
                policy.eligibility.max_arousal = Some(0.65);
                policy.targets.energy = Some(0.36);
                policy.targets.arousal = Some(0.38);
                policy.targets.tension = Some(0.58);
                policy.targets.vocalness = Some(0.10);
                policy.targets.familiarity = FamiliarityPreference::Avoid;
                policy.transition.arc = PlaylistArc::RiseAndFall;
                policy.audit.min_mean_transition_score = 0.50;
            }
            PlaylistPreset::Party => {
                policy.eligibility.min_energy = Some(0.40);
                policy.targets.energy = Some(0.68);
                policy.targets.arousal = Some(0.70);
                policy.targets.familiarity = FamiliarityPreference::Prefer;
                policy.transition.arc = PlaylistArc::Rise;
                policy.audit.min_unique_artist_ratio = 0.45;
            }
            PlaylistPreset::Workout => {
                policy.eligibility.min_energy = Some(0.45);
                policy.eligibility.min_arousal = Some(0.45);
                policy.targets.energy = Some(0.75);
                policy.targets.arousal = Some(0.78);
                policy.transition.arc = PlaylistArc::Rise;
            }
            PlaylistPreset::Chill => {
                policy.eligibility.max_energy = Some(0.52);
                policy.eligibility.max_arousal = Some(0.58);
                policy.eligibility.max_vocalness = Some(0.60);
                policy.targets.energy = Some(0.32);
                policy.targets.arousal = Some(0.32);
                policy.transition.arc = PlaylistArc::Fall;
            }
            PlaylistPreset::Discovery => {
                policy.targets.familiarity = FamiliarityPreference::Avoid;
                policy.diversity.max_per_artist = 1;
                policy.diversity.max_per_album = 1;
                policy.audit.min_unique_artist_ratio = 0.80;
                policy.audit.max_artist_share = 0.08;
                policy.audit.max_album_share = 0.08;
                policy.transition.arc = PlaylistArc::None;
            }
        }
        policy
    }
}

impl Default for PlaylistPolicy {
    fn default() -> Self {
        Self::for_preset(PlaylistPreset::General)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatSummary {
    pub present: usize,
    pub total: usize,
    pub mean: Option<f64>,
    pub min: Option<f64>,
    pub p25: Option<f64>,
    pub median: Option<f64>,
    pub p75: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryProfile {
    pub tracks: usize,
    pub music_tracks: usize,
    pub canonical_tracks: usize,
    pub eligible_default_tracks: usize,
    pub unique_artists: usize,
    pub unique_albums: usize,
    pub unique_songs: usize,
    pub unique_styles: usize,
    pub quality_tiers: BTreeMap<String, usize>,
    /// Counts by exact aggression model id, including incompatible models.
    /// This lets callers establish comparability before choosing thresholds.
    #[serde(default)]
    pub aggression_models: BTreeMap<String, usize>,
    pub stats: BTreeMap<String, StatSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditIssue {
    pub severity: AuditSeverity,
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub positions: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitionScore {
    pub from_position: usize,
    pub to_position: usize,
    pub from_id: String,
    pub to_id: String,
    pub total: f64,
    pub embedding: Option<f64>,
    pub features: Option<f64>,
    pub tempo: Option<f64>,
    pub key: Option<f64>,
    pub same_artist_penalty: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistAudit {
    pub passed: bool,
    pub track_count: usize,
    pub total_duration_sec: f64,
    pub unique_artists: usize,
    pub unique_albums: usize,
    pub unique_songs: usize,
    pub unique_artist_ratio: f64,
    pub max_artist_share: f64,
    pub max_album_share: f64,
    pub duplicate_ids: usize,
    pub mean_transition_score: Option<f64>,
    pub worst_transition_score: Option<f64>,
    pub mean_arc_error: Option<f64>,
    pub transitions: Vec<TransitionScore>,
    pub issues: Vec<AuditIssue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreContribution {
    pub component: String,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggressionStatus {
    Available,
    Abstained,
    Missing,
    IncompatibleModel,
    InvalidDiagnostics,
}

/// Complete fused-aggression evidence for a track.
///
/// `confidence` is content/evidence support, not score certainty. The optional
/// score is comparable only when `status == available`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggressionEvidence {
    pub status: AggressionStatus,
    pub model_id: Option<String>,
    pub score: Option<f64>,
    pub confidence: Option<f64>,
    pub forcefulness: Option<f64>,
    pub harshness: Option<f64>,
    pub tension: Option<f64>,
    pub rhythm: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackExplanation {
    pub position: usize,
    pub content_hash: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub contributions: Vec<ScoreContribution>,
    /// Present only when the policy explicitly uses aggression, preserving
    /// byte-compatible neutral-policy explanations and old stored metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggression: Option<AggressionEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistExplanation {
    pub tracks: Vec<TrackExplanation>,
    pub transitions: Vec<TransitionScore>,
    pub summary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CuratedPlaylist {
    pub exportable: bool,
    pub track_ids: Vec<String>,
    /// Immutable identity of the exact analysis/model cache inputs behind the
    /// graph used for selection. Absent only for legacy/external graphs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_input_fingerprint: Option<String>,
    pub brief: PlaylistBrief,
    pub policy: PlaylistPolicy,
    pub audit: PlaylistAudit,
    pub explanation: PlaylistExplanation,
    pub repair_attempts: usize,
}
