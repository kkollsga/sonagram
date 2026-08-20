//! P21 statistics layer: curve-derived flat `Track` features (Stage A) and the
//! percentile-calibrated composite mood/quality axes (Stage B).
//!
//! The graph historically ingested only the scalar summaries of each analysis
//! record and discarded the curves (energy/loudness/tempo/chords/segments), where
//! recording quality and musical character actually live. This module reads those curves
//! — already cached on disk for every track — at build time and projects them
//! into flat, queryable `Track` properties. It is a **pure mapper**: no scan or
//! analysis is re-run.
//!
//! ## Two stages, two shapes
//! - **Stage A** ([`curve_features`]) is per-track and local: each property is a
//!   pure function of one record's curves, computed independently. A property is
//!   `null` (returned `None`) when its source curve is absent or too short to be
//!   meaningful — the same null-property policy the rest of the mapper follows.
//! - **Stage B** ([`composite_axes`]) is library-relative and needs two passes:
//!   first the raw per-track components across the whole library, then a z-score
//!   of each component against the library mean/stdev, a signed-mean composite,
//!   and finally a percentile rank of that composite within the library. It is
//!   therefore calibrated *per library*, not against absolute constants.
//!
//! ## Determinism
//! Every computation here is deterministic given the sorted record set: the
//! per-track functions are pure arithmetic, and the Stage-B percentile pass sorts
//! strictly by `(composite score, content_hash)` — `content_hash` is the total
//! tie-break, so equal scores never reorder across runs or input permutations.
//! Values are stored as raw `f64` (no rounding), matching the existing mapper
//! convention in `derive.rs` and `mod.rs`, so the golden digest is reproducible.

use crate::record::{AnalysisRecord, ChordEventDto};

/// Minimum consecutive strictly-increasing samples that count as one energy
/// "build" (crescendo). Anchored in the design: eight rising frames is a
/// deliberate arc, not curve jitter.
const BUILD_RUN_MIN: usize = 8;

/// Guard against dividing by a mean that is effectively zero (an all-silent
/// curve). Below this the ratio features are undefined and return `None`.
const MEAN_EPS: f64 = 1e-9;

/// Seconds per minute — curve counts are normalised to per-minute rates so they
/// are comparable across track lengths.
const SECS_PER_MIN: f64 = 60.0;

/// Spectral-flatness values above this threshold are strong evidence that a
/// file is noise, silence, speech, or another non-music fragment. The live
/// library's p99 is 0.049; 0.10 is deliberately conservative.
const NON_MUSIC_FLATNESS_THRESHOLD: f32 = 0.10;

// ───────────────────────────── Stage A: curve features ──────────────────────

/// The nine curve-derived flat `Track` properties for one record. Every field is
/// `Option` — `None` when the source curve is absent or too short — and the
/// caller renders `None` as a null graph cell.
pub(super) struct CurveFeatures {
    /// Population stdev of `loudness_curve` — arrangement-level quiet/loud
    /// architecture; the strongest local recording-quality signal (d=0.81).
    pub macro_dynamics: Option<f64>,
    /// `(p95 − p5) / mean` of `energy_curve` — how far the song travels
    /// dynamically.
    pub energy_arc_range: Option<f64>,
    /// Count of maximal strictly-increasing `energy_curve` runs of at least
    /// [`BUILD_RUN_MIN`] samples, per minute — the crescendo rate.
    pub energy_builds_per_min: Option<f64>,
    /// `1 − mean|Δenergy| / mean(energy)`, clamped to `[0,1]` — steady flow vs
    /// jitter.
    pub flow_smoothness: Option<f64>,
    /// Distinct chord labels in `chord_events` — harmonic breadth.
    pub chord_vocab: Option<i64>,
    /// Shannon entropy (bits) of the chord-label distribution — harmonic
    /// richness/unpredictability.
    pub chord_entropy: Option<f64>,
    /// `chord_events` per minute — detector instability on murky audio (quality
    /// signal, d=0.65) and genuine harmonic rate on clean audio.
    pub chord_churn: Option<f64>,
    /// `1 − cv(tempo_curve)`, clamped to `[0,1]` — performance tightness.
    pub tempo_steadiness: Option<f64>,
    /// Structure `segments` per minute — structural busyness.
    pub seg_density: Option<f64>,
}

/// Compute the Stage-A curve features for one record.
pub(super) fn curve_features(r: &AnalysisRecord) -> CurveFeatures {
    let a = &r.analysis;
    let dur = a.duration_sec as f64;
    CurveFeatures {
        macro_dynamics: macro_dynamics(a.loudness_curve.as_deref()),
        energy_arc_range: energy_arc_range(a.energy_curve.as_deref()),
        energy_builds_per_min: energy_builds_per_min(a.energy_curve.as_deref(), dur),
        flow_smoothness: flow_smoothness(a.energy_curve.as_deref()),
        chord_vocab: chord_vocab(a.chord_events.as_deref()),
        chord_entropy: chord_entropy(a.chord_events.as_deref()),
        chord_churn: chord_churn(a.chord_events.as_deref(), dur),
        tempo_steadiness: tempo_steadiness(a.tempo_curve.as_deref()),
        seg_density: seg_density(a.segments.as_deref(), dur),
    }
}

/// Population stdev of the loudness curve. `None` when the curve is absent or has
/// fewer than two samples (a stdev of one point is degenerate).
fn macro_dynamics(loudness_curve: Option<&[f32]>) -> Option<f64> {
    let curve = loudness_curve?;
    if curve.len() < 2 {
        return None;
    }
    Some(population_stdev(&to_f64(curve)))
}

/// Dynamic travel of the energy curve: `(p95 − p5) / mean`. `None` when the curve
/// is absent, has fewer than two samples, or its mean is ~0 (all-silent).
fn energy_arc_range(energy_curve: Option<&[f32]>) -> Option<f64> {
    let curve = energy_curve?;
    if curve.len() < 2 {
        return None;
    }
    let v = to_f64(curve);
    let m = mean(&v);
    if m.abs() < MEAN_EPS {
        return None;
    }
    let mut sorted = v;
    sorted.sort_by(f64::total_cmp);
    Some((percentile(&sorted, 0.95) - percentile(&sorted, 0.05)) / m)
}

/// Crescendo rate: the number of maximal strictly-increasing runs of at least
/// [`BUILD_RUN_MIN`] samples, divided by the track duration in minutes. `None`
/// when the curve is absent or the duration is non-positive; a present-but-short
/// curve simply yields `0.0` (no build can form). A "run" is a maximal ascending
/// stretch — a rising stretch of length `L ≥ 8` counts once, not `L − 7` times.
fn energy_builds_per_min(energy_curve: Option<&[f32]>, duration_sec: f64) -> Option<f64> {
    let curve = energy_curve?;
    if duration_sec <= 0.0 {
        return None;
    }
    let mut builds = 0usize;
    let mut run = 1usize; // samples in the current ascending run (min one)
    for w in curve.windows(2) {
        if w[1] > w[0] {
            run += 1;
        } else {
            if run >= BUILD_RUN_MIN {
                builds += 1;
            }
            run = 1;
        }
    }
    if run >= BUILD_RUN_MIN {
        builds += 1;
    }
    Some(builds as f64 / (duration_sec / SECS_PER_MIN))
}

/// Steadiness of the energy flow: `1 − mean|Δ| / mean`, clamped to `[0,1]`. `None`
/// when the curve is absent, has fewer than two samples (no delta), or its mean
/// is ~0.
fn flow_smoothness(energy_curve: Option<&[f32]>) -> Option<f64> {
    let curve = energy_curve?;
    if curve.len() < 2 {
        return None;
    }
    let v = to_f64(curve);
    let m = mean(&v);
    if m.abs() < MEAN_EPS {
        return None;
    }
    let mean_abs_delta =
        v.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (v.len() - 1) as f64;
    Some((1.0 - mean_abs_delta / m).clamp(0.0, 1.0))
}

/// Distinct chord labels present in `chord_events`. `None` when the field is
/// absent or empty (no harmonic content to measure). Labels are counted verbatim
/// per the design spec — the no-chord sentinel `"N"` is one such label.
fn chord_vocab(chord_events: Option<&[ChordEventDto]>) -> Option<i64> {
    let evs = chord_events?;
    if evs.is_empty() {
        return None;
    }
    let mut labels: Vec<&str> = evs.iter().map(|e| e.label.as_str()).collect();
    labels.sort_unstable();
    labels.dedup();
    Some(labels.len() as i64)
}

/// Shannon entropy (bits) of the chord-label distribution. `None` when the field
/// is absent or empty; a single distinct label yields `0.0`. Labels are counted
/// verbatim (see [`chord_vocab`]).
fn chord_entropy(chord_events: Option<&[ChordEventDto]>) -> Option<f64> {
    let evs = chord_events?;
    if evs.is_empty() {
        return None;
    }
    // Count per label in a sorted structure so the fold order is fixed.
    let mut counts: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    for e in evs {
        *counts.entry(e.label.as_str()).or_insert(0) += 1;
    }
    let total = evs.len() as f64;
    let h = counts
        .values()
        .map(|&c| {
            let p = c as f64 / total;
            -p * p.log2()
        })
        .sum::<f64>();
    Some(h)
}

/// Chord-change rate: `chord_events` per minute. `None` when the field is absent
/// or the duration is non-positive; an empty (but present) event list is `0.0`.
fn chord_churn(chord_events: Option<&[ChordEventDto]>, duration_sec: f64) -> Option<f64> {
    let evs = chord_events?;
    if duration_sec <= 0.0 {
        return None;
    }
    Some(evs.len() as f64 / (duration_sec / SECS_PER_MIN))
}

/// Tempo tightness: `1 − cv(tempo_curve)`, clamped to `[0,1]`, where the
/// coefficient of variation is population stdev over mean. `None` when the curve
/// is absent, has fewer than two samples, or its mean is ~0.
fn tempo_steadiness(tempo_curve: Option<&[f32]>) -> Option<f64> {
    let curve = tempo_curve?;
    if curve.len() < 2 {
        return None;
    }
    let v = to_f64(curve);
    let m = mean(&v);
    if m.abs() < MEAN_EPS {
        return None;
    }
    Some((1.0 - population_stdev(&v) / m).clamp(0.0, 1.0))
}

/// Structural busyness: `segments` per minute. `None` when the field is absent or
/// the duration is non-positive; an empty (but present) segment list is `0.0`.
fn seg_density(
    segments: Option<&[crate::record::SegmentEventDto]>,
    duration_sec: f64,
) -> Option<f64> {
    let segs = segments?;
    if duration_sec <= 0.0 {
        return None;
    }
    Some(segs.len() as f64 / (duration_sec / SECS_PER_MIN))
}

// ───────────────────────────── Stage B: composite axes ──────────────────────

/// The five percentile-calibrated composite properties for one track. The four
/// index axes are percentile ranks in `[0,1]`; `quality_tier` is the coarse
/// three-way cut of `recording_quality`. Each is `None` when every component that
/// feeds it is absent for this track (so no signed z-score could be formed).
pub(super) struct CompositeAxes {
    /// Conservative music-content gate. Missing flatness is not evidence of
    /// non-music, so it remains `true`; only values strictly above 0.10 reject.
    pub is_music: bool,
    /// Energy/brightness/attack composite — the well-predicted mood axis
    /// (R²≈0.6–0.85 in the literature).
    pub arousal_index: Option<f64>,
    /// Pleasantness composite. **Weak prior**: valence is poorly predicted from
    /// audio alone (literature R² 0.12–0.28). Exposed for completeness, but an
    /// agent must cross-check it rather than trust it.
    pub valence_index: Option<f64>,
    /// Harmonic/timbral tension composite — the better-predicted third axis
    /// (Eerola 2009: tension R²=0.79, above valence).
    pub tension_index: Option<f64>,
    /// Recording-quality composite, validated locally (canon-vs-bootleg
    /// AUC≈0.75): +macro_dynamics, −chord_churn, −dissonance, +bpm_confidence.
    pub recording_quality: Option<f64>,
    /// `"high"`/`"mid"`/`"low"` by percentile thirds of `recording_quality` — a
    /// cheap `WHERE` handle. `None` exactly when `recording_quality` is `None`.
    pub quality_tier: Option<String>,
}

/// One z-composite component: the per-track raw values (`None` = absent for that
/// track) and the sign with which the component enters the composite.
struct Component {
    values: Vec<Option<f64>>,
    sign: f64,
}

impl Component {
    fn new(values: Vec<Option<f64>>, sign: f64) -> Self {
        Component { values, sign }
    }
}

/// Compute the Stage-B composite axes for the whole (sorted) library.
///
/// `sorted` and `feats` are parallel and in the deterministic `content_hash`
/// order the builder already imposes; `feats[i]` are the Stage-A features of
/// `sorted[i]`. The result is parallel to both. Two passes: build each axis's raw
/// components, z-composite them, then percentile-rank the composite across the
/// library (tie-broken by `content_hash`).
pub(super) fn composite_axes(
    sorted: &[&AnalysisRecord],
    feats: &[CurveFeatures],
) -> Vec<CompositeAxes> {
    let n = sorted.len();
    let hashes: Vec<&str> = sorted
        .iter()
        .map(|r| r.source.content_hash.as_str())
        .collect();

    // Small per-track projections used across several axes.
    let dissonance: Vec<Option<f64>> = fo(sorted, |a| a.dissonance);
    let chord_entropy: Vec<Option<f64>> = feats.iter().map(|f| f.chord_entropy).collect();
    let is_music: Vec<bool> = sorted.iter().map(|r| is_music_record(r)).collect();

    // arousal_index: energy(+), spectral_centroid(+), onset_density(+),
    // danceability(+), spectral_contrast_mean(+ if present), MFCC-1(+ if present).
    let arousal = z_composite(&[
        Component::new(music_only(fo(sorted, |a| a.energy), &is_music), 1.0),
        Component::new(
            music_only(f_always(sorted, |a| a.spectral_centroid_mean), &is_music),
            1.0,
        ),
        Component::new(
            music_only(f_always(sorted, |a| a.onset_density), &is_music),
            1.0,
        ),
        Component::new(music_only(fo(sorted, |a| a.danceability), &is_music), 1.0),
        Component::new(
            music_only(
                fo(sorted, |a| vec_mean(a.spectral_contrast_mean.as_deref())),
                &is_music,
            ),
            1.0,
        ),
        Component::new(
            music_only(
                fo(sorted, |a| {
                    a.mfcc_mean.as_ref().and_then(|m| m.first().copied())
                }),
                &is_music,
            ),
            1.0,
        ),
    ]);

    // valence_index (WEAK PRIOR): key_confidence(+), dissonance(−), major(+),
    // chord_entropy(−).
    let valence = z_composite(&[
        Component::new(music_only(fo(sorted, |a| a.key_confidence), &is_music), 1.0),
        Component::new(music_only(dissonance.clone(), &is_music), -1.0),
        Component::new(music_only(mode_indicator(sorted, "major"), &is_music), 1.0),
        Component::new(music_only(chord_entropy.clone(), &is_music), -1.0),
    ]);

    // tension_index: dissonance(+), minor(+), chord_entropy(+). Spectral
    // flatness is a non-music detector, not a musical-tension component.
    let tension = z_composite(&[
        Component::new(music_only(dissonance.clone(), &is_music), 1.0),
        Component::new(music_only(mode_indicator(sorted, "minor"), &is_music), 1.0),
        Component::new(music_only(chord_entropy.clone(), &is_music), 1.0),
    ]);

    // recording_quality: macro_dynamics(+), chord_churn(−), dissonance(−),
    // bpm_confidence(+).
    let quality = z_composite(&[
        Component::new(feats.iter().map(|f| f.macro_dynamics).collect(), 1.0),
        Component::new(feats.iter().map(|f| f.chord_churn).collect(), -1.0),
        Component::new(dissonance, -1.0),
        Component::new(f_always(sorted, |a| a.bpm_confidence), 1.0),
    ]);

    let arousal_pct = percentile_rank(&arousal, &hashes);
    let valence_pct = percentile_rank(&valence, &hashes);
    let tension_pct = percentile_rank(&tension, &hashes);
    let quality_pct = percentile_rank(&quality, &hashes);

    (0..n)
        .map(|i| CompositeAxes {
            is_music: is_music[i],
            arousal_index: arousal_pct[i],
            valence_index: valence_pct[i],
            tension_index: tension_pct[i],
            recording_quality: quality_pct[i],
            quality_tier: quality_pct[i].map(|p| quality_tier(p).to_string()),
        })
        .collect()
}

fn is_music_record(r: &AnalysisRecord) -> bool {
    r.analysis
        .spectral_flatness_mean
        .is_none_or(|v| v <= NON_MUSIC_FLATNESS_THRESHOLD)
}

/// Remove non-music rows before z-score calibration. Masking only the final
/// percentile would still let outliers distort each component's mean/stdev.
fn music_only(mut values: Vec<Option<f64>>, is_music: &[bool]) -> Vec<Option<f64>> {
    debug_assert_eq!(values.len(), is_music.len());
    for (value, &keep) in values.iter_mut().zip(is_music) {
        if !keep {
            *value = None;
        }
    }
    values
}

/// Percentile thirds of a `recording_quality` percentile rank.
fn quality_tier(p: f64) -> &'static str {
    if p < 1.0 / 3.0 {
        "low"
    } else if p < 2.0 / 3.0 {
        "mid"
    } else {
        "high"
    }
}

/// The z-score composite: for each component, standardise the present values
/// against the component's own library mean/stdev, apply the component sign, then
/// average the available signed z-scores per track. A track is `None` only when
/// **every** component is absent for it. A component with zero library variance
/// contributes `0.0` (all tracks equal — no information, no NaN).
fn z_composite(components: &[Component]) -> Vec<Option<f64>> {
    let n = components.first().map(|c| c.values.len()).unwrap_or(0);
    // Per-component library mean/stdev over the present values only.
    let stats: Vec<(f64, f64)> = components
        .iter()
        .map(|c| {
            let present: Vec<f64> = c.values.iter().flatten().copied().collect();
            (mean(&present), population_stdev(&present))
        })
        .collect();

    (0..n)
        .map(|i| {
            let mut sum = 0.0;
            let mut count = 0usize;
            for (c, &(mu, sd)) in components.iter().zip(&stats) {
                if let Some(v) = c.values[i] {
                    let z = if sd > 0.0 { (v - mu) / sd } else { 0.0 };
                    sum += c.sign * z;
                    count += 1;
                }
            }
            if count == 0 {
                None
            } else {
                Some(sum / count as f64)
            }
        })
        .collect()
}

/// Percentile rank of each non-null score within the library, in `[0,1]`,
/// deterministically tie-broken by `content_hash`. Tracks are ranked by
/// `(score, hash)` ascending; the rank at 0-based position `i` of `m` non-null
/// tracks is `i / (m − 1)`. A single non-null track ranks at `0.5` (no spread to
/// place it against); null scores stay null.
fn percentile_rank(scores: &[Option<f64>], hashes: &[&str]) -> Vec<Option<f64>> {
    let mut idx: Vec<usize> = (0..scores.len()).filter(|&i| scores[i].is_some()).collect();
    idx.sort_by(|&a, &b| {
        scores[a]
            .unwrap()
            .total_cmp(&scores[b].unwrap())
            .then_with(|| hashes[a].cmp(hashes[b]))
    });
    let m = idx.len();
    let mut out = vec![None; scores.len()];
    for (rank, &i) in idx.iter().enumerate() {
        out[i] = Some(if m <= 1 {
            0.5
        } else {
            rank as f64 / (m as f64 - 1.0)
        });
    }
    out
}

/// A binary mode indicator from each record's key string: `1.0` when the key
/// names the wanted mode (`"major"`/`"minor"`), `0.0` when it names the other,
/// `None` when no key is present.
fn mode_indicator(sorted: &[&AnalysisRecord], mode: &str) -> Vec<Option<f64>> {
    sorted
        .iter()
        .map(|r| {
            r.analysis.key.as_deref().map(|k| {
                if k.to_lowercase().contains(mode) {
                    1.0
                } else {
                    0.0
                }
            })
        })
        .collect()
}

// ─────────────────────────────── column projectors ──────────────────────────

/// Project an always-present `f32` analysis scalar into a per-track `f64` column.
fn f_always(
    sorted: &[&AnalysisRecord],
    f: impl Fn(&crate::record::AnalysisDto) -> f32,
) -> Vec<Option<f64>> {
    sorted.iter().map(|r| Some(f(&r.analysis) as f64)).collect()
}

/// Project an optional analysis value into a per-track nullable `f64` column.
fn fo<T: Into<f64>>(
    sorted: &[&AnalysisRecord],
    f: impl Fn(&crate::record::AnalysisDto) -> Option<T>,
) -> Vec<Option<f64>> {
    sorted
        .iter()
        .map(|r| f(&r.analysis).map(Into::into))
        .collect()
}

/// Mean of a slice, or `None` when it is empty — used for the vector-valued
/// `spectral_contrast_mean` component.
fn vec_mean(v: Option<&[f32]>) -> Option<f32> {
    let v = v?;
    if v.is_empty() {
        None
    } else {
        Some(v.iter().sum::<f32>() / v.len() as f32)
    }
}

// ───────────────────────────────── statistics ───────────────────────────────

/// Copy an `f32` curve into `f64` for stable accumulation.
fn to_f64(v: &[f32]) -> Vec<f64> {
    v.iter().map(|&x| x as f64).collect()
}

/// Arithmetic mean; `0.0` for an empty slice.
fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// Population standard deviation (divisor `N`); `0.0` for a slice of fewer than
/// two elements.
fn population_stdev(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let m = mean(v);
    let var = v.iter().map(|&x| (x - m) * (x - m)).sum::<f64>() / v.len() as f64;
    var.sqrt()
}

/// Linear-interpolated percentile (numpy's default "linear"/type-7 method) of an
/// **ascending-sorted** slice, `q ∈ [0,1]`. Empty → `0.0`.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = q * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = rank - lo as f64;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::ChordEventDto;

    fn chord(label: &str) -> ChordEventDto {
        ChordEventDto {
            label: label.to_string(),
            start_sec: 0.0,
            end_sec: 1.0,
        }
    }

    // ── statistics primitives ──

    #[test]
    fn population_stdev_matches_hand_computation() {
        // values [2,4,4,4,5,5,7,9]: mean 5, population variance 4, stdev 2.
        let v = vec![2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        assert!((population_stdev(&v) - 2.0).abs() < 1e-12);
        assert_eq!(population_stdev(&[3.0]), 0.0); // one element → 0
        assert_eq!(population_stdev(&[]), 0.0); // empty → 0
        assert_eq!(population_stdev(&[5.0, 5.0, 5.0]), 0.0); // constant → 0
    }

    #[test]
    fn percentile_interpolates_linearly() {
        let v = vec![0.0, 1.0, 2.0, 3.0, 4.0]; // already sorted
        assert!((percentile(&v, 0.0) - 0.0).abs() < 1e-12);
        assert!((percentile(&v, 1.0) - 4.0).abs() < 1e-12);
        assert!((percentile(&v, 0.5) - 2.0).abs() < 1e-12);
        // p95 of 0..=4 → rank 0.95*4=3.8 → 3 + 0.8*(4-3) = 3.8.
        assert!((percentile(&v, 0.95) - 3.8).abs() < 1e-12);
        assert_eq!(percentile(&[7.0], 0.5), 7.0); // single value
        assert_eq!(percentile(&[], 0.5), 0.0); // empty
    }

    // ── Stage A: macro_dynamics ──

    #[test]
    fn macro_dynamics_is_population_stdev_and_nulls_when_short() {
        assert!(macro_dynamics(None).is_none());
        assert!(macro_dynamics(Some(&[])).is_none());
        assert!(macro_dynamics(Some(&[-12.0])).is_none()); // one sample too short
        let md = macro_dynamics(Some(&[-14.0, -10.0])).unwrap();
        assert!((md - 2.0).abs() < 1e-6); // stdev of [-14,-10] = 2
        assert_eq!(macro_dynamics(Some(&[-10.0, -10.0, -10.0])).unwrap(), 0.0); // constant
    }

    // ── Stage A: energy_arc_range ──

    #[test]
    fn energy_arc_range_ratio_and_null_edges() {
        assert!(energy_arc_range(None).is_none());
        assert!(energy_arc_range(Some(&[0.4])).is_none()); // too short
        assert!(energy_arc_range(Some(&[0.0, 0.0, 0.0])).is_none()); // mean ~0 → null
                                                                     // [0,1,2,3,4]: mean 2, p95 3.8, p5 0.2 → (3.6)/2 = 1.8.
        let r = energy_arc_range(Some(&[0.0, 1.0, 2.0, 3.0, 4.0])).unwrap();
        assert!((r - 1.8).abs() < 1e-6);
    }

    // ── Stage A: energy_builds_per_min ──

    #[test]
    fn energy_builds_counts_maximal_runs_over_eight() {
        assert!(energy_builds_per_min(None, 60.0).is_none());
        assert!(energy_builds_per_min(Some(&[0.1, 0.2]), 0.0).is_none()); // no duration
                                                                          // Short present curve → zero builds, still a valid rate.
        assert_eq!(
            energy_builds_per_min(Some(&[0.1, 0.2, 0.3]), 60.0).unwrap(),
            0.0
        );
        // One ascending run of 8 samples (7 rises) over one minute → exactly 1/min.
        let rising: Vec<f32> = (0..8).map(|i| i as f32).collect();
        assert_eq!(energy_builds_per_min(Some(&rising), 60.0).unwrap(), 1.0);
        // A run of 7 samples does NOT qualify.
        let seven: Vec<f32> = (0..7).map(|i| i as f32).collect();
        assert_eq!(energy_builds_per_min(Some(&seven), 60.0).unwrap(), 0.0);
        // Two separate qualifying runs separated by a drop → 2 over 2 minutes = 1/min.
        let mut two = (0..8).map(|i| i as f32).collect::<Vec<_>>();
        two.push(0.0); // drop resets the run
        two.extend((0..8).map(|i| i as f32));
        assert_eq!(energy_builds_per_min(Some(&two), 120.0).unwrap(), 1.0);
    }

    // ── Stage A: flow_smoothness ──

    #[test]
    fn flow_smoothness_clamps_and_nulls() {
        assert!(flow_smoothness(None).is_none());
        assert!(flow_smoothness(Some(&[0.5])).is_none()); // too short
        assert!(flow_smoothness(Some(&[0.0, 0.0])).is_none()); // mean ~0 → null
                                                               // Constant non-zero curve: no jitter → smoothness 1.
        assert_eq!(flow_smoothness(Some(&[0.5, 0.5, 0.5])).unwrap(), 1.0);
        // Big swings clamp at 0 rather than going negative.
        assert_eq!(flow_smoothness(Some(&[0.1, 1.0, 0.1, 1.0])).unwrap(), 0.0);
    }

    // ── Stage A: chord features ──

    #[test]
    fn chord_vocab_counts_distinct_labels() {
        assert!(chord_vocab(None).is_none());
        assert!(chord_vocab(Some(&[])).is_none());
        let evs = vec![chord("C"), chord("Am"), chord("C"), chord("N")];
        assert_eq!(chord_vocab(Some(&evs)).unwrap(), 3); // C, Am, N distinct
    }

    #[test]
    fn chord_entropy_is_shannon_bits() {
        assert!(chord_entropy(None).is_none());
        assert!(chord_entropy(Some(&[])).is_none());
        // One label → 0 bits.
        assert_eq!(chord_entropy(Some(&[chord("C"), chord("C")])).unwrap(), 0.0);
        // Two equally likely labels → 1 bit.
        let two = vec![chord("C"), chord("G")];
        assert!((chord_entropy(Some(&two)).unwrap() - 1.0).abs() < 1e-12);
        // Four equally likely → 2 bits.
        let four = vec![chord("C"), chord("G"), chord("Am"), chord("F")];
        assert!((chord_entropy(Some(&four)).unwrap() - 2.0).abs() < 1e-12);
    }

    #[test]
    fn chord_churn_is_events_per_minute() {
        assert!(chord_churn(None, 60.0).is_none());
        assert!(chord_churn(Some(&[chord("C")]), 0.0).is_none()); // no duration
        assert_eq!(chord_churn(Some(&[]), 60.0).unwrap(), 0.0); // present-empty → 0
        let evs = vec![chord("C"), chord("G"), chord("Am")];
        assert_eq!(chord_churn(Some(&evs), 120.0).unwrap(), 1.5); // 3 / 2 min
    }

    // ── Stage A: tempo_steadiness / seg_density ──

    #[test]
    fn tempo_steadiness_clamps_and_nulls() {
        assert!(tempo_steadiness(None).is_none());
        assert!(tempo_steadiness(Some(&[120.0])).is_none()); // too short
        assert_eq!(tempo_steadiness(Some(&[120.0, 120.0, 120.0])).unwrap(), 1.0); // cv 0 → 1
                                                                                  // Non-trivial cv is < 1.
        let s = tempo_steadiness(Some(&[118.0, 122.0])).unwrap();
        assert!(s > 0.9 && s < 1.0);
    }

    #[test]
    fn seg_density_is_segments_per_minute() {
        use crate::record::SegmentEventDto;
        let seg = || SegmentEventDto {
            start_sec: 0.0,
            end_sec: 1.0,
            energy: 0.3,
        };
        assert!(seg_density(None, 60.0).is_none());
        assert!(seg_density(Some(&[seg()]), 0.0).is_none()); // no duration
        assert_eq!(seg_density(Some(&[]), 60.0).unwrap(), 0.0);
        assert_eq!(
            seg_density(Some(&[seg(), seg(), seg()]), 60.0).unwrap(),
            3.0
        );
    }

    // ── Stage B: z_composite ──

    #[test]
    fn z_composite_averages_signed_z_scores() {
        // One component, three tracks [1,2,3]: mean 2, stdev sqrt(2/3).
        // z = (x-2)/stdev; with sign +1 the composite equals the z-score.
        let c = Component::new(vec![Some(1.0), Some(2.0), Some(3.0)], 1.0);
        let out = z_composite(&[c]);
        assert!(out[1].unwrap().abs() < 1e-12); // the mean track → 0
        assert!(out[0].unwrap() < 0.0 && out[2].unwrap() > 0.0);
        assert!((out[0].unwrap() + out[2].unwrap()).abs() < 1e-12); // symmetric
    }

    #[test]
    fn z_composite_sign_flips_direction() {
        let c = Component::new(vec![Some(1.0), Some(3.0)], -1.0);
        let out = z_composite(&[c]);
        // Negative sign: the larger raw value gets the lower composite.
        assert!(out[0].unwrap() > out[1].unwrap());
    }

    #[test]
    fn z_composite_null_only_when_all_components_absent() {
        let a = Component::new(vec![Some(1.0), None, Some(3.0)], 1.0);
        let b = Component::new(vec![None, None, Some(9.0)], 1.0);
        let out = z_composite(&[a, b]);
        assert!(out[0].is_some()); // only `a` present → still a score
        assert!(out[1].is_none()); // both absent → null
        assert!(out[2].is_some());
    }

    #[test]
    fn z_composite_zero_variance_component_contributes_zero() {
        // Constant component (no variance) must not produce NaN; it adds 0.
        let flat = Component::new(vec![Some(5.0), Some(5.0)], 1.0);
        let out = z_composite(&[flat]);
        assert_eq!(out[0].unwrap(), 0.0);
        assert_eq!(out[1].unwrap(), 0.0);
    }

    #[test]
    fn non_music_mask_excludes_rows_before_calibration() {
        let base = vec![Some(1.0), Some(2.0), Some(3.0)];
        let with_extreme = vec![Some(1.0), Some(2.0), Some(3.0), Some(1_000.0)];
        let base_scores = z_composite(&[Component::new(base, 1.0)]);
        let masked_scores = z_composite(&[Component::new(
            music_only(with_extreme, &[true, true, true, false]),
            1.0,
        )]);
        assert_eq!(&masked_scores[..3], base_scores.as_slice());
        assert!(masked_scores[3].is_none());
    }

    #[test]
    fn non_music_threshold_is_strict_and_missing_is_music() {
        let classify =
            |flatness: Option<f32>| flatness.is_none_or(|v| v <= NON_MUSIC_FLATNESS_THRESHOLD);
        assert!(classify(None));
        assert!(classify(Some(0.10)));
        assert!(!classify(Some(0.100_001)));
    }

    // ── Stage B: percentile_rank determinism ──

    #[test]
    fn percentile_rank_spreads_and_tie_breaks_by_hash() {
        let scores = vec![Some(0.5), Some(0.1), Some(0.9)];
        let hashes = vec!["a", "b", "c"];
        let out = percentile_rank(&scores, &hashes);
        // Lowest score (b) → 0, highest (c) → 1, middle (a) → 0.5.
        assert_eq!(out[1].unwrap(), 0.0);
        assert_eq!(out[0].unwrap(), 0.5);
        assert_eq!(out[2].unwrap(), 1.0);
    }

    #[test]
    fn percentile_rank_null_scores_stay_null_and_single_is_half() {
        let out = percentile_rank(&[None, Some(1.0)], &["a", "b"]);
        assert!(out[0].is_none());
        assert_eq!(out[1].unwrap(), 0.5); // sole non-null → 0.5
    }

    #[test]
    fn percentile_rank_is_deterministic_under_permutation() {
        // Equal scores everywhere: only the hash tie-break decides the order, and
        // it must assign each hash the SAME percentile regardless of input order.
        let base_scores = vec![Some(0.7), Some(0.7), Some(0.7), Some(0.7)];
        let base_hashes = vec!["h0", "h1", "h2", "h3"];
        let base = percentile_rank(&base_scores, &base_hashes);
        // Map hash → percentile from the base ordering.
        let want: std::collections::BTreeMap<&str, f64> = base_hashes
            .iter()
            .zip(&base)
            .map(|(h, p)| (*h, p.unwrap()))
            .collect();

        // A reversed ordering of the same (score, hash) pairs.
        let scores = vec![Some(0.7), Some(0.7), Some(0.7), Some(0.7)];
        let hashes = vec!["h3", "h2", "h1", "h0"];
        let got = percentile_rank(&scores, &hashes);
        for (h, p) in hashes.iter().zip(&got) {
            assert_eq!(
                want[*h],
                p.unwrap(),
                "hash {h} must get a stable percentile"
            );
        }
    }
}
