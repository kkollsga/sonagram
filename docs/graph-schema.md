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
| `zero_crossing_rate` | float | no | timbre proxy |
| `analysis_schema_version` | int | no | provenance |
| `embedding_version` | int | yes | provenance |

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
| `Library` (1) | label | `path`, `n_tracks`, `schema_version` |

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
| `SIMILAR_TO` | `Track`→`Track` | `score` 0–1 | top-10 kNN by audio embedding; **directed** (A→B ≠ B→A) |
| `IN_STYLE` | `Track`→`Style` | `membership` | tracks in a similarity community |
| `CAMELOT_ADJACENT` | `Key`→`Key` | `transition` = `energy_up`/`energy_down`/`mode_switch` | the Camelot wheel (72 edges) |

## Enrichment additions (after `enrich`)

Running [`sonagram enrich`](cli.md#sonagram-enrich) before `build` folds Last.fm
data into the same schema: popularity, MBID, and original-album properties on
`Track` / `Artist` / `Album`; extra folksonomy-tag `IN_GENRE` edges; and
`CROWD_SIMILAR` edges (weighted `Track`→`Track` and `source="lastfm"`
`Artist`→`Artist`) alongside the audio-derived `SIMILAR_TO` web. A plain build
(no enrichment cache) omits these.

## Determinism

The mapping is deterministic: the same library scanned twice produces a
byte-identical graph — same node ids, same ordering, same canonical digest. That
is a tested contract, not an accident; see [Determinism](determinism.md).
