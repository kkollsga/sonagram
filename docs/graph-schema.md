# Graph schema

A sonagram graph is a kglite knowledge graph with one shape: a **`Track` hub**,
small **dimension nodes** to group and traverse by, and two **derived**
structures — a `SIMILAR_TO` nearest-neighbour web and `Style` community nodes.
`Key-[:CAMELOT_ADJACENT]->Key` encodes the harmonic-mixing wheel. This page is
the node/edge/property reference; for *how to query it*, see the
[Agent guide](agent-guide.md).

Every node exposes its id as `.id` and a display string as `.title`.

## `Track` — one per audio file

Id = `content_hash`. Every signal an agent filters or sorts by is a flat scalar
on `Track`, so filtering never needs a join. **Null?** marks properties that can
be null (opt-in analysis that wasn't run, or a tag the file lacked).

| Property | Type | Null? | Meaning / range |
|---|---|---|---|
| `content_hash` | str | no | blake3 audio hash = stable identity (survives retag/move). Also `.id`. |
| `title` | str | no | tag title, else filename. Also `.title`. |
| `path` | str | no | library-root-**relative** path (mutable) |
| `filename` | str | no | file name only |
| `artist_name` | str | no | artist tag, else `"Unknown Artist"` |
| `album_name` | str | yes | album tag |
| `genre_tag` | str | yes | raw genre tag (case preserved; the `Genre` node id is lowercased) |
| `format` | str | no | container, e.g. `"mp3"` |
| `year` | int | yes | **file/edition** year tag — on reissues/compilations this is the reissue date, not the recording date |
| `original_year` | int | yes | original release year (ID3 `TDOR`/`TORY`/Vorbis `ORIGINALDATE`); the reissue-safe era signal. Often null in the wild |
| `era_source` | str | yes | which year fed `Decade`/`FROM_DECADE`: `"original_year"` (era-true) or `"file_year"` (file tag). Null only when no year tag at all |
| `track_no` | int | yes | track number (tag) |
| `file_size` | int | no | bytes |
| `duration_sec` | float | no | length in seconds |
| `bpm` | float | no | tempo, typical 60–180. Gate on `bpm_confidence` before trusting it |
| `bpm_raw` | float | no | pre-octave-correction tempo |
| `bpm_confidence` | float | no | 0–1 trust signal for `bpm`: high (≥0.7) on steady percussive music, low (~0.4) on ambient/rubato/sparse-onset material |
| `energy` | float | yes | perceptual energy 0–1 |
| `valence` | float | yes | musical positivity 0–1 |
| `danceability` | float | yes | 0–1 |
| `acousticness` | float | yes | 0–1, absolute scale: electronic ≈ 0.11, acoustic ≈ 0.71 |
| `vocalness` | float | yes | 0–1, higher = more vocal (harsh/screamed > clean singing > instrumental) |
| `instrumentalness` | float | yes | 0–1, exactly `1 − vocalness` (collinear) |
| `dissonance` | float | yes | 0–1 |
| `mood_happy` / `mood_aggressive` / `mood_relaxed` / `mood_sad` | float | yes | 0–1 heuristic |
| `aggression` | float | yes | Sonara fused perceptual aggression rank `[0,1]`; **not a probability**. Null can be a valid abstention |
| `aggression_confidence` | float | yes | content/evidence support `[0,1]`; **not certainty about the rank** |
| `aggression_forcefulness` | float | yes | diagnostic component `[0,1]` |
| `aggression_harshness` | float | yes | diagnostic component `[0,1]` |
| `aggression_tension` | float | yes | diagnostic aggression component `[0,1]`; distinct from library-relative `tension_index` |
| `aggression_rhythm` | float | yes | diagnostic component `[0,1]` |
| `aggression_model_id` | str | yes | exact model provenance; current comparable records use `aggression-rank-v3-sr22050` |
| `energy_level` | int | yes | coarse energy bucket 1–10 |
| `key` | str | yes | e.g. `"A minor"` |
| `camelot` | str | yes | Camelot code, e.g. `"8A"` |
| `key_confidence` | float | yes | 0–1 |
| `predominant_chord` | str | yes | most-present chord |
| `chord_change_rate` | float | yes | chords per second |
| `time_signature` | str | yes | e.g. `"4/4"` |
| `tempo_variability` / `grid_stability` / `grid_offset_sec` | float | yes | beat-grid detail |
| `onset_density` | float | no | onsets per second |
| `loudness_lufs` | float | no | integrated loudness |
| `dynamic_range_db` | float | no | dynamic range |
| `loudness_range_lu` / `true_peak_db` / `replaygain_db` | float | yes | opt-in loudness |
| `intro_end_sec` / `outro_start_sec` / `leading_silence_sec` / `trailing_silence_sec` | float | yes | structure/silence |
| `n_segments` | int | yes | number of structural sections |
| `spectral_centroid` | float | no | brightness (Hz) |
| `spectral_flatness` | float | yes | noisiness 0–1 |
| `is_music` | bool | no | false when `spectral_flatness > 0.10`; missing flatness stays true |
| `zero_crossing_rate` | float | no | timbre proxy |
| `analysis_schema_version` | int | no | provenance |
| `embedding_version` | int | yes | provenance |
| `genre_model_id` | str | yes | exact Sonara genre-model identity; null when no model ran |
| `vocalness_model_id` | str | yes | exact Sonara vocalness-model identity; current scans require `sonara-vocalness-v2` |

The seven aggression fields above are the graph-schema-v3 addition. Compare
ranks only under the exact same non-null `aggression_model_id`. A null score
with a model id and complete bounded confidence/components is a responsible
Sonara abstention, not a zero. `mood_aggressive` remains a separate heuristic;
`tension_index` measures harmonic/musical tension relative to this library.
Neither is a fallback for unavailable aggression. Sonara 0.3.3 computes rank-v3
in a canonical 22.05 kHz aggression lane while preserving the source-rate
domain for the rest of the analysis. Schema-5/rank-v2 caches need an audio
rescan because rank-v3 cannot be reconstructed from the stored similarity
embedding.

### Graph-derived Track properties (schema v2)

| Property | Type | Null? | Meaning |
|---|---|---|---|
| `macro_dynamics` | float | yes | loudness-curve population spread |
| `energy_arc_range` | float | yes | `(p95−p5)/mean` of the energy curve |
| `energy_builds_per_min` | float | yes | sustained crescendo count per minute |
| `flow_smoothness` | float | yes | 0–1 steadiness of the energy curve |
| `chord_vocab` | int | yes | number of distinct chord labels |
| `chord_entropy` | float | yes | harmonic-label Shannon entropy in bits |
| `chord_churn` | float | yes | chord events per minute |
| `tempo_steadiness` | float | yes | 0–1 tempo consistency |
| `seg_density` | float | yes | structural sections per minute |
| `arousal_index` | float | yes | music-only library percentile for energy/brightness/rhythm intensity |
| `valence_index` | float | yes | music-only library percentile for musical positiveness; a weak ranking prior, not a hard filter |
| `tension_index` | float | yes | music-only library percentile for dissonance/minor/harmonic complexity |
| `recording_quality` | float | yes | library percentile for audio production/provenance quality |
| `quality_tier` | str | yes | `"low"`, `"mid"`, or `"high"` third of `recording_quality` |
| `is_canonical` | bool | no | true for singletons and the preferred member of each version group |

Non-music is excluded from calibration of the three mood axes and carries null
for all three. Mood queries should require `t.is_music` and require every axis
they use to be non-null. Curve features and recording quality remain available.

### Track recognition + popularity (schema v2)

These columns exist in both plain and enriched graphs. Without a usable Last.fm
match, counts and popularity are null and `has_lastfm_match` is false.

| Property | Type | Null? | Meaning |
|---|---|---|---|
| `lastfm_listeners` | int | yes | Last.fm listener count for the matched song |
| `lastfm_playcount` | int | yes | Last.fm play count for the matched song |
| `has_lastfm_match` | bool | no | whether enrichment fetched a usable track match |
| `popularity` | float | yes | listener-count percentile `[0,1]` within this library; equal counts share a midrank |

Popularity is song-level: recognized versions of the same song commonly tie.
It is useful for preferring familiar songs, but cannot prove master versus
alternate take among recognized releases.

## Dimension + derived nodes

| Node | id | Properties |
|---|---|---|
| `Artist` | artist name | `name`, `n_tracks` |
| `Album` | `artist\|album` | `name`, `artist`, `year` (null) |
| `Genre` | lowercased tag | `name` |
| `Key` (24, static) | key name | `name`, `camelot`, `mode` (`minor`/`major`) |
| `TempoBand` (7, static) | band name | `name` — `glacial`<70, `downtempo` 70–90, `mid` 90–110, `house` 110–125, `upbeat` 125–140, `fast` 140–160, `frantic` ≥160 bpm |
| `EnergyLevel` (10, static) | `"1"`–`"10"` | `name`, `level` |
| `Decade` | e.g. `"1970s"` | `name` |
| `Style` (detected) | `"style-000"` (id prop is `unique_id`) | `name` (derived `<band>-<acoustic\|electric>-<top-genre>`), `mean_bpm`, `mean_energy`, `mean_valence`, `mean_acousticness`, `n_tracks`, `top_genres` (list), `top_artists` (list), `exemplar_titles` (list) |
| `Song` | `artist_id\|normalized_title` | `title`, `artist`, `n_versions`, `canonical_hash`; exists for version groups with at least two members |
| `Source` | absolute source root | `path`, `n_tracks`, optional stat-only `scan_fingerprint`, exact-cache `build_input_fingerprint` |
| `Library` (1) | label | `path`, `n_tracks`, `schema_version`, combined `build_input_fingerprint` |

## Edges

| Edge | Direction | Props | Notes |
|---|---|---|---|
| `BY_ARTIST` | `Track`→`Artist`, `Album`→`Artist` | — | always for Track |
| `ON_ALBUM` | `Track`→`Album` | — | only if album tag present |
| `IN_GENRE` | `Track`→`Genre` | — | only if genre tag present |
| `IN_KEY` | `Track`→`Key` | — | only if key detected |
| `IN_TEMPO_BAND` | `Track`→`TempoBand` | — | always (bpm always present) |
| `AT_ENERGY` | `Track`→`EnergyLevel` | — | only if `energy_level` present |
| `FROM_DECADE` | `Track`→`Decade` | — | only if a year tag present; uses `original_year` when set, else file `year` (see `era_source`) |
| `FROM_SOURCE` | `Track`→`Source` | — | always; identifies the owning configured source |
| `SIMILAR_TO` | `Track`→`Track` | `score` 0–1 | top-10 kNN by audio embedding; **directed** (A→B ≠ B→A) |
| `VERSION_OF` | `Track`→`Song` | — | distinct recordings in a version group |
| `IN_STYLE` | `Track`→`Style` | `membership` | tracks in a similarity community |
| `CAMELOT_ADJACENT` | `Key`→`Key` | `transition` = `energy_up`/`energy_down`/`mode_switch` | the Camelot wheel (72 edges) |

## Enrichment additions (after `enrich`)

Running [`sonagram enrich`](cli.md#sonagram-enrich) before `build` fills the
always-present Track recognition/popularity columns above and adds MBID and
original-album metadata on `Track` / `Artist` / `Album`, extra folksonomy-tag
`IN_GENRE` edges, and `CROWD_SIMILAR` edges (weighted `Track`→`Track` and
`source="lastfm"` `Artist`→`Artist`) alongside the audio-derived `SIMILAR_TO`
web. A plain build keeps the four Track columns but uses null/null/false/null.

## Version grouping and canonical selection

Primary groups share artist + normalized title. An explicitly junk-tagged track
(`Unknown Artist`, `Artiest onbekend`, or `TJT` followed by a digit) can attach
to a non-junk Song only when exactly one primary Song has the same normalized
title and a `SIMILAR_TO` edge in either direction reaches one of its original
members. Known-artist covers never move, ambiguous titles remain separate, and
reassigned tracks cannot cause cascading assignments.

Within a Song, canonical selection orders by `has_lastfm_match` descending,
`recording_quality` descending (null lowest), then `content_hash` ascending.
Last.fm recognition can favor a recognized release over an unmatched outtake;
because popularity is song-level, it cannot distinguish two recognized takes.

## Determinism

The mapping is deterministic: the same library scanned twice produces a
byte-identical graph — same node ids, same ordering, same canonical digest. That
is a tested contract, not an accident; see [Determinism](determinism.md).
