# AGENT-GUIDE — working a sonagram music graph

You are an AI agent with MCP access to a **sonagram** knowledge graph: a music
library mapped into [kglite](https://github.com/kkollsga/kglite) and served by
`sonagram-mcp-server`, a thin KGLite 0.16.18 frontend. Generic exploration uses:

- **`cypher_query`** — run one openCypher query, get up to ~15 rows inline.
  It takes a single `query` string and **nothing else**: there is no parameter
  binding over MCP, so **inline every literal** (`{title:'Marry You'}`,
  `[0.1, ...]`) — a `$param` reference errors with `Missing parameter`.
- **`graph_overview`** — the node/edge inventory with live counts and sample
  ids. Call it first on an unfamiliar graph to see the library's shape.
- **`music_library_profile`** — fast eligible counts, per-axis coverage, and
  p25/median/p75 distributions before unusual curation requests.

Typed library operations are **`music_curation_policy`**,
**`music_curate_playlist`**, **`music_audit_playlist`**,
**`music_explain_playlist`**, and the `music_playlist*` list/show/update/delete
store tools. They call the same Rust methods as CLI/Python against KGLite's live
graph; MCP-only agents never need to hand-author IDs.

These graph tools are for exploration and explanation. For a final playlist,
the agent translates intent into a `PlaylistBrief` / preset and invokes
Sonagram's curation engine. Sonagram—not the agent—owns candidate selection,
Song/version deduplication, sequencing, repair, audit, and stored provenance.
Never hand-select or reorder final IDs from Cypher rows.

## Playlist curation contract

Use `music_library_profile` for calibration, resolve a preset with
`music_curation_policy`, then call `music_curate_playlist` with a typed brief
and optional `store`. CLI `sonagram curate` and Python expose the same contract.

Resolve the complete versioned preset with `sonagram policy --preset <name>
--format json` or Python `sonagram.curation_policy(name)`. The typed request
contract supports:

- seed roles `pinned` (default), `reference`, and `pinned_and_reference`;
- positive/negative seed similarity, optional minimum similarity, and
  `lower|similar|any|higher` targets for energy, arousal, tension, and vocalness;
  optional per-feature margins require a measurable relative change;
- explicit fused-aggression eligibility (`min_aggression` / `max_aggression`),
  absolute target, and seed-relative target/margin. Every preset leaves these
  neutral; profile the library distribution and exact model counts before
  setting them;
- include/exclude filters for artists, genres, detected styles, and decades,
  plus exact year bounds.

Reference-only seeds anchor ranking and hard relative gates but are never
exported. Curated output is re-audited against the brief, so the request cannot
disappear after selection. Unknown JSON fields are rejected; put a known
unenforceable constraint in `brief.unsupported_intents` to receive a structured
`unsupported_intent` failure instead of improvising IDs.

A result is deliverable only when `exportable` and `audit.passed` are true.
Never silently relax hard constraints or repair a poor result with private
agent logic. If a library-curated result is weak despite passing, capture the
measurable defect as a Sonagram library issue.

## Mental model
Every audio file is one **`Track`** node — the fat hub. Every signal an agent
filters or sorts by (bpm, energy, valence, key, loudness, mood_*, …) is a **flat
scalar property on `Track`**, so filtering never needs a join. Around the hub sit
small **dimension nodes** you group and traverse by (`Artist`, `Album`, `Genre`,
`Key`, `TempoBand`, `EnergyLevel`, `Decade`) plus two **derived** structures:
`Track-[:SIMILAR_TO]->Track` (top-10 nearest neighbours by audio embedding) and
`Style` community nodes (tracks that cluster tightly in similarity, with a
readable profile). A `Song` node groups distinct recordings of one song and
points back to its preferred recording via `canonical_hash`.
`Key-[:CAMELOT_ADJACENT]->Key` encodes the harmonic-mixing wheel.

## Node reference

Every node also exposes its id as `.id` and a display string as `.title`.

### `Track` — one per audio file (id = `content_hash`)
Grouped by family; **Null?** marks properties that can be null (opt-in analysis
that wasn't run, or a tag the file lacked).

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
| `era_source` | str | yes | which year fed `Decade`/`FROM_DECADE`: `"original_year"` (era-true) or `"file_year"` (file tag, treat era with care). Null only when no year tag at all |
| `track_no` | int | yes | track number (tag) |
| `file_size` | int | no | bytes |
| `duration_sec` | float | no | length in seconds |
| `bpm` | float | no | tempo, typical 60–180. **Gate on `bpm_confidence` before trusting it** (see below) |
| `bpm_raw` | float | no | pre-octave-correction tempo |
| `bpm_confidence` | float | no | 0–1 trust signal for `bpm`: high (≥0.7) on steady percussive music, low (~0.4) on ambient/rubato/sparse-onset material where BPM is unreliable |
| `energy` | float | yes | perceptual energy 0–1 |
| `valence` | float | yes | musical positivity 0–1 |
| `danceability` | float | yes | 0–1 |
| `acousticness` | float | yes | 0–1, **absolute scale (recalibrated schema v3)**: electronic ≈ 0.11, acoustic ≈ 0.71 (was compressed to ~0.42–0.93). Roughly: `>= 0.60` acoustic, `<= 0.30` electric |
| `vocalness` | float | yes | 0–1, **higher = more vocal (FIXED in graph schema v3)**: harsh/screamed > clean singing > instrumental. Usable as a low=instrumental / high=vocal filter (see Pitfalls) |
| `instrumentalness` | float | yes | 0–1, exactly `1 − vocalness` (collinear — no added signal over `vocalness`) |
| `dissonance` | float | yes | 0–1 |
| `mood_happy` / `mood_aggressive` / `mood_relaxed` / `mood_sad` | float | yes | 0–1 heuristic v1 (see Pitfalls) |
| `aggression` | float | yes | Sonara fused perceptual aggression rank `[0,1]`; **not a probability**. Null can be a valid abstention |
| `aggression_confidence` | float | yes | content/evidence support `[0,1]`; **not certainty about the rank** |
| `aggression_forcefulness` | float | yes | diagnostic component `[0,1]` |
| `aggression_harshness` | float | yes | diagnostic component `[0,1]` |
| `aggression_tension` | float | yes | diagnostic component `[0,1]`; distinct from library-relative `tension_index` |
| `aggression_rhythm` | float | yes | diagnostic component `[0,1]` |
| `aggression_model_id` | str | yes | exact model identity; current comparable ranks use `aggression-rank-v3-sr22050` |
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
| `is_music` | bool | no | false when `spectral_flatness > 0.10` identifies noise, speech, silence, or another non-musical fragment. Missing flatness stays true. |
| `zero_crossing_rate` | float | no | timbre proxy |
| `analysis_schema_version` | int | no | provenance |
| `embedding_version` | int | yes | provenance |
| `genre_model_id` | str | yes | exact Sonara genre model identity; null when no genre model ran |
| `vocalness_model_id` | str | yes | exact Sonara vocalness model identity; current scans require `sonara-vocalness-v2` |

The seven aggression fields are the graph-schema-v3 addition. Compare ranks
only when `aggression_model_id` matches exactly. A null score with complete
bounded support/components is a valid abstention, not zero. Sonara 0.3.4
uses an optimized canonical 22.05 kHz aggression lane, so rank-v3 is
comparable across source sample rates while other analysis stays in its native
rate domain. Schema-5/rank-v2 caches need an audio rescan because rank-v3 cannot
be reconstructed from the similarity embedding. `mood_aggressive` is a
separate legacy heuristic, while `tension_index` is library-relative
harmonic/musical tension; neither is a fallback for unavailable aggression.

**P21 curve-derived features** (graph schema v2 — computed from the full
analysis curves at build time):

| Property | Type | Null? | Meaning |
|---|---|---|---|
| `macro_dynamics` | float | yes | loudness-curve spread: arrangement-level quiet/loud architecture. High = deliberate dynamics (real masters); low = flat/monotone |
| `energy_arc_range` | float | yes | how far the song travels dynamically ((p95−p5)/mean of energy curve) |
| `energy_builds_per_min` | float | yes | sustained crescendo count per minute — "songs that go somewhere" |
| `flow_smoothness` | float | yes | 0–1, high = steady flow (focus-friendly), low = jittery |
| `chord_vocab` | int | yes | distinct chords in the track |
| `chord_entropy` | float | yes | harmonic richness/unpredictability (bits) |
| `chord_churn` | float | yes | chord events/min — on clean audio = harmonic rate; inflated on murky recordings (read with `recording_quality`) |
| `tempo_steadiness` | float | yes | 0–1 performance tightness |
| `seg_density` | float | yes | structural sections per minute |

**P21 composite axes** (library-relative percentile ranks in [0,1] — 0.5 is the
library median by construction; filter with percentile semantics, e.g.
`tension_index > 0.7` = top 30%):

| Property | Type | Null? | Meaning |
|---|---|---|---|
| `arousal_index` | float | yes | energy/brightness/rhythm intensity — the well-predicted mood axis. Null for non-music; also require `is_music = true` in mood queries. |
| `valence_index` | float | yes | musical positiveness — **WEAK PRIOR** (literature R² 0.12–0.28): rank with it, never hard-filter. Null for non-music. |
| `tension_index` | float | yes | dissonance/minor/harmonic-complexity — "deep thinking" material = low-mid `arousal_index` × mid-high `tension_index` × high `chord_entropy`. Null for non-music. |
| `recording_quality` | float | yes | audio-only production/provenance quality (validated: separates studio masters from bootlegs, AUC 0.75 scalar-only, d=0.93 with curves) |
| `quality_tier` | str | yes | `"high"` / `"mid"` / `"low"` — percentile thirds of `recording_quality`, for cheap WHERE clauses |
| `is_canonical` | bool | no | false only for inferior members of a version group — `WHERE t.is_canonical` is the universal "skip duplicate/inferior takes" filter |

**P21b recognition + popularity** (the columns always exist; a plain or
unmatched build gives null counts/popularity and `has_lastfm_match = false`):

| Property | Type | Null? | Meaning |
|---|---|---|---|
| `lastfm_listeners` | int | yes | Last.fm listener count for the matched song |
| `lastfm_playcount` | int | yes | Last.fm play count for the matched song |
| `has_lastfm_match` | bool | no | true when enrichment fetched a usable Last.fm track match; a recognized release beats an unmatched take during canonical selection |
| `popularity` | float | yes | listener-count percentile in `[0,1]` among tracks with listener counts in this library; tied listener counts share a midrank |

Popularity is principally **song-level**, not recording-level: recognized
versions of the same song commonly receive identical listener/play counts.
Use it to prefer familiar songs, not to claim that one recognized release is
the master rather than an alternate take.

### Dimension + derived nodes

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
| `Song` (P21, v2) | `artist_id\|normalized_title` | `title`, `artist`, `n_versions`, `canonical_hash` — exists only when ≥2 recordings share a version key; `canonical_hash` is selected by Last.fm match, then `recording_quality`, then ascending content hash |
| `Source` | absolute source root | `path`, `n_tracks`, `scan_fingerprint`, `build_input_fingerprint` — the latter identifies exact cached analysis/model inputs, not only file stats |
| `Library` (1) | label | `path`, `n_tracks`, `schema_version`, `build_input_fingerprint` (deterministic combination of sorted sources) |

## Edge reference

| Edge | Direction | Props | Notes |
|---|---|---|---|
| `BY_ARTIST` | `Track`→`Artist`, `Album`→`Artist` | — | always for Track |
| `ON_ALBUM` | `Track`→`Album` | — | only if album tag present |
| `IN_GENRE` | `Track`→`Genre` | — | only if genre tag present |
| `IN_KEY` | `Track`→`Key` | — | only if key detected |
| `IN_TEMPO_BAND` | `Track`→`TempoBand` | — | always (bpm always present) |
| `AT_ENERGY` | `Track`→`EnergyLevel` | — | only if `energy_level` present |
| `FROM_DECADE` | `Track`→`Decade` | — | only if a year tag present; uses `original_year` when set, else file `year` (see `era_source`) |
| `FROM_SOURCE` | `Track`→`Source` | — | always; resolves the owning source in multi-library graphs |
| `SIMILAR_TO` | `Track`→`Track` | `score` 0–1 | top-10 kNN; **directed** (A→B ≠ B→A) |
| `VERSION_OF` | `Track`→`Song` | — | recordings of the same composition (P21); primary grouping is artist + normalized title, with the conservative junk-tag repair described below |
| `IN_STYLE` | `Track`→`Style` | `membership` (1.0 in v1) | tracks in a similarity community |
| `CAMELOT_ADJACENT` | `Key`→`Key` | `transition` = `energy_up`/`energy_down`/`mode_switch` | the Camelot wheel (72 edges) |

### Version grouping and canonical choice

The primary version key is `(artist, normalized_title)`. A second, deliberately
conservative pass can repair a junk artist tag (`Unknown Artist`, `Artiest
onbekend`, or a `TJT` tag followed by a digit): it attaches the track only when
there is exactly one non-junk `Song` with the same normalized title **and** a
`SIMILAR_TO` edge in either direction connects it to one of that Song's original
members. Known-artist covers never move, ambiguous titles stay separate, and an
attached track never becomes an anchor for a cascading reassignment.

Within each resulting Song, canonical choice is a total order:
`has_lastfm_match DESC`, `recording_quality DESC` (null lowest), then
`content_hash ASC`. This finds a recognized, good-quality release; it does not
prove master versus alternate take when both are recognized. In that case the
song-level Last.fm statistics tie and audio quality, then hash, decide.

## Query cookbook
All four archetypes, copy-paste runnable (inline literals — no `$params`).

**1 — Filter + order** ("house-tempo tracks, calmest first"):
```cypher
MATCH (t:Track)
WHERE t.is_music AND t.is_canonical
  AND t.bpm >= 110 AND t.bpm < 125 AND t.vocalness < 0.5
RETURN t.title, t.artist_name, t.bpm, t.energy
ORDER BY t.energy ASC LIMIT 30
```

**2 — Discover / group** — what's in the library:
```cypher
-- genre histogram
MATCH (t:Track)-[:IN_GENRE]->(g:Genre)
RETURN g.name AS genre, count(*) AS n, round(avg(t.bpm),1) AS avg_bpm
ORDER BY n DESC, genre
```
```cypher
-- detected styles, with their readable profile (read this, don't guess)
MATCH (s:Style)
RETURN s.name, s.n_tracks, round(s.mean_bpm,1) AS bpm,
       round(s.mean_energy,2) AS energy, s.top_genres, s.exemplar_titles
ORDER BY s.n_tracks DESC
```

**3 — Similarity** ("like this, but calmer") — the primary path is the
`SIMILAR_TO` hop, which composes with any `WHERE`:
```cypher
MATCH (s:Track {title:'Marry You'})-[r:SIMILAR_TO]->(t:Track)
WHERE t.is_music AND t.is_canonical AND t.energy < s.energy
RETURN t.title, t.artist_name, round(r.score,3) AS score
ORDER BY score DESC LIMIT 20
```
For neighbours-of-neighbours beyond the top-10 horizon, chain the hop
(`-[:SIMILAR_TO*1..2]->`). **`vector_score` is an advanced escape hatch, not the
usual path**: it needs a 48-dim pre-weighted vector inlined into the query
(`vector_score(t, 'similarity', [0.1, 0.2, ...])`), and there is **no Cypher way
to read a Track's stored embedding** (embeddings live in kglite's embedding
store, not as a property — `t.embedding` is null; only `embedding_norm(t,
'similarity')` exposes a scalar). Obtain a seed vector out-of-band (e.g. the
Python API) or just use `SIMILAR_TO`.

**4 — Inspect sequence evidence** — a Camelot-adjacent, energy-rising query is
useful for explaining possible transitions. It is not a final sequencer:
```cypher
MATCH (a:Track)-[:IN_KEY]->(k1:Key)-[e:CAMELOT_ADJACENT]->(k2:Key)<-[:IN_KEY]-(b:Track)
WHERE a.title = 'Just Like a Woman' AND b.is_music AND b.is_canonical
  AND b.energy > a.energy AND abs(b.bpm - a.bpm) < 15
RETURN b.title, b.artist_name, b.bpm, b.energy, e.transition
ORDER BY b.energy LIMIT 10
```

**Curation queries: name the id column.** The four archetypes above are for
*reading*. A query you hand to `sonagram playlist --cypher` (or
`export_m3u(cypher=...)`) has to return Track ids, and the resolver takes the
first column named `content_hash` or `id` (bare or in the qualified
`t.content_hash` form). Only when no column is named that does it fall back to
**position** — the first column whose first row is a Track node or a string that
resolves to a `Track`. That fallback depends on the order your `RETURN` happens
to produce, so make the column explicit and it never fires:
`RETURN t.content_hash AS content_hash` (any other columns may follow, in any
order).

## Creating and materializing a playlist

For agent-created playlists, use the curation engine directly:

```sh
sonagram curate --preset focus --tracks 25 \
  --name "Focused Thinking" --description "focused work" --format json
```

The stored `.m3u8` preserves the returned order and its `.meta.json` records the
brief, resolved policy, audit, explanation, repair attempts, graph path, and
ordered IDs. Use `sonagram playlists show <slug>` to retrieve that evidence.

The lower-level `sonagram playlist --ids/--cypher` and Python `export_m3u`
remain available for explicit human-authored/manual exports. They preserve
caller order but do not curate it, so agents must not use them as a substitute
for `curate_playlist`.

## Pitfalls
- **Keys are trustworthy** (sonara ≥ 0.2.3 / graph schema v2; the old F-major
  bias is fixed and validated). Interpreting `key_confidence`: the scale runs
  low — library mean ≈ 0.17; **≥ 0.3 is solid, ≥ 0.45 is strong**. Note that
  high-energy dense mixes tend to score lower confidence, so a strict energy
  floor and a strict confidence floor fight each other — relax one.
- **`vocalness`/`instrumentalness` require model provenance, not only schema
  v3.** Current Sonagram scans use Sonara's calibrated bundled model; require
  `t.vocalness_model_id = 'sonara-vocalness-v2'`. It scores **low =
  instrumental, high = vocal** and is suitable for instrumental filtering: use
  `vocalness < ~0.35` to find instrumentals and `>= ~0.55` to require vocals.
  Two caveats remain: (1) a **voice-mimicking solo instrument** (nylon-guitar
  flamenco, solo violin, cathedral organ) can still read mid-high — the
  documented ambiguous case; cross-check with genre/artist for pure-instrumental
  asks. (2) `vocalness` measures *presence*, not *singability* — a high score
  says "a voice is prominent", not "great singalong". `instrumentalness` is
  exactly `1 − vocalness` (collinear — no added signal; prefer `vocalness`).
  Records with a null/different model id are stale even when
  `analysis_schema_version = 3`; do not mix their heuristic scores with current
  model-derived values.
- **Gate `bpm` on `bpm_confidence` before trusting it.** New in schema v3:
  `bpm_confidence` (0–1) flags when the tempo estimate is solid. Steady dance/pop
  reads 0.7–0.9; ambient/rubato/sparse-onset material (classical, drone,
  fingerpicked ballads) reads ~0.4 and its `bpm` is often octave-wrong or
  meaningless. Add `WHERE t.bpm_confidence >= 0.6` before ordering or filtering by
  `bpm`, and for calm/acoustic sets order by `energy` or `duration_sec`, never
  `bpm`.
- **Use fused aggression, never `mood_aggressive`, for explicit aggression
  intent.** The legacy mood heuristic can invert on extreme material; it is not
  a compatibility fallback. Profile `aggression` coverage, p25/median/p75, and
  `aggression_models`, then express intent through the typed policy. The score
  is a rank, confidence is content/evidence support, and only an exact current
  model id is comparable. Null score + complete diagnostics is a valid
  abstention, but an active directive fails closed as `aggression_unknown`.
  `aggression_tension` is one model diagnostic; `tension_index` remains a
  separate harmonic/musical axis. Never hand-rank with loudness, energy, genre,
  artist knowledge, or any combination when the library reports unknown.
- **`genre_tag` is spotty real-world data** — it's whatever the file's ID3 tag
  says (or null). It is not an audio-derived classification. Expect missing,
  inconsistent, and idiosyncratic values (`"rap & hip-hop"` vs `"hip-hop"`).
- **`SIMILAR_TO` is directed** kNN: `A→B` does **not** imply `B→A`. To find
  mutual neighbours, match both directions explicitly.
- **`Style` nodes are similarity communities, not human genres.** A style name
  like `upbeat-electric-dance` is a deterministic template over the cluster's
  mean features — **read `top_genres`, `mean_*`, and `exemplar_titles`** to know
  what it actually contains, rather than trusting the name.
- **`mood_*` are heuristic v1** — directional, not calibrated ground truth. Good
  for coarse ranking, not hard thresholds. Vocalness is separately model-derived
  when its required model id is present; see the pitfall above.
- **Mood axes are music-only and nullable.** Non-music is excluded from their
  library calibration and carries null `arousal_index`, `valence_index`, and
  `tension_index`. Start mood queries with `t.is_music AND t.is_canonical` and
  require each axis you use to be non-null; do not mistake a null for a low
  score.
- **Popularity recognizes songs, not masters.** `popularity` is the library
  percentile of Last.fm listeners and is useful for a familiar/mainstream
  brief. It commonly ties across recognized versions of the same song, so it
  cannot establish master versus alternate take. `has_lastfm_match` says the
  release was recognized; canonical selection then falls back to
  `recording_quality` and finally ascending `content_hash`.
- **Cypher null semantics**: `WHERE t.energy < 0.5` silently **excludes** rows
  where `energy` is null (a null comparison is not true). Filter explicitly with
  `t.energy IS NOT NULL` when a null-able property must be present, and
  remember every "Null? yes" property above can drop rows this way.
- **No query parameters over MCP**: `cypher_query` takes only the query string.
  Inline all literals; `$seed`-style placeholders fail. (Quote-heavy titles like
  `O'Neal`: use double-quoted Cypher strings to avoid shell/Cypher `'` fights.)

## Field notes (from live agent validation, 456-track library, 2026-07-17)
- **Similarity is "same vibe", not "same style".** The embedding ranks on
  energy/acousticness-like character; country seeds pull crooner jazz and film
  scores as top neighbours. For a stylistic/genre-character request ("western",
  "twangy") lead with *specific* genre tags + audio scalars
  (`spectral_centroid` ~2000–3500 for guitar-range brightness, `acousticness`,
  `vocalness`) and use `SIMILAR_TO` only to confirm.
- **One `SIMILAR_TO` hop spans only ±~0.06 energy.** For "like this but
  calmer/wilder", chain: `-[:SIMILAR_TO*1..2]->` with a `DISTINCT` and your
  energy bound — that's the real wind-down archetype.
- **Multi-constraint ordering isn't a Cypher `ORDER BY`.** This is why final
  ordering belongs to Sonagram's pairwise sequencer: it combines embeddings,
  feature deltas, reliable tempo/key evidence, artist spacing, and an explicit
  arc under one deterministic audit.
- **Python results are a `kglite.ResultView`**: use `.to_dicts()` /
  `.to_df()` / `.scalar()` / `.one()`. Printed reprs TRUNCATE rows and long
  strings (content hashes become `"8fdb…d63"`) — never copy values from a repr;
  always `.to_dicts()` (or narrow with LIMIT/OFFSET).
- **`danceability` now spreads and IS a usable filter** (schema v3 /
  sonara 0.2.4 recalibration): it runs roughly p5 0.20 → p95 0.90 (was
  compressed ~0.69–0.98, near-useless). Steady dance/house sits high, ambient and
  rubato low. Still cross it with genre/energy for a stylistic ask, but a
  `danceability >= ~0.6` floor now meaningfully separates groove from drift.
- **`bpm` is unreliable on low-onset material** (ambient/classical can read
  145–157) — and now you can SEE it: those tracks carry a low `bpm_confidence`
  (~0.4). Gate with `WHERE t.bpm_confidence >= 0.6`, and order calm sets by
  `energy`, not `bpm`.
- **Style quality degrades on heterogeneous libraries**: expect one cap-sized
  catch-all community plus small tight slivers. Styles are a lead, not a
  survey — for "what's in here", aggregate over `Genre`/`Decade`/`TempoBand`
  and read Style `top_genres`/`exemplar_titles` skeptically.
- **Close values need full precision**: two tracks can both display `0.462`
  yet differ at the 5th decimal — don't judge strict ordering from rounded
  output.
- **Never hand-build a final arc.** A ramp, breather, harmonic chain, or mood
  dip cannot be expressed as one `ORDER BY`; express the requested arc in the
  typed policy and let Sonagram sequence, repair, and audit it.
- **Mood-playlist recipe** (feel-good, chill, angry…): begin with
  `t.is_music AND t.is_canonical`, require the mood axes you use to be non-null,
  and curate from `arousal_index`/`tension_index` percentiles plus curve features
  such as `flow_smoothness` and `chord_entropy`. Treat `valence_index` as a
  ranking hint, then cross-check with `mood_*`, `energy`, and `vocalness` when
  diagnosing results. The curation policy owns the arc and duration bounds.
  For explicit aggressive/non-aggressive intent, use the fused aggression
  policy separately; do not infer it from mood or tension.
- **Vibe-over-time recipe**: cross-tab Genre × Decade to find a vibe spanning
  eras, hold it with a tight `acousticness`/`energy` band, and read
  `avg(loudness_lufs)` per decade for production evolution. HARD RULE, now
  graph-auditable: `Decade`/`FROM_DECADE` prefers `original_year` when the file
  carries one, else falls back to the file `year` — and `t.era_source` tells you
  which. **When `era_source = 'original_year'` the decade is era-true** (trust it).
  **When `era_source = 'file_year'` it's the year printed on the FILE** — on real
  libraries that's the compilation/reissue date by default for pre-2000 material
  (whole reissue blocks get tagged 2015+), so still date those era claims from
  your own artist/recording knowledge. Filter the trustworthy rows with
  `WHERE t.era_source = 'original_year'` when era precision matters. In practice
  `original_year` is often absent (this library: 0 of 15 sample tracks had it),
  so expect to lean on knowledge for most pre-2000 material regardless.
- **Finding versions/covers**: identical audio deduplicates to ONE Track node
  (id = content-hash of the audio), so two Track nodes are different recordings.
  Start with `MATCH (t:Track)-[:VERSION_OF]->(s:Song)` and read
  `s.canonical_hash`; the graph already normalizes edition suffixes and applies
  the conservative audio-confirmed repair for explicit junk artist tags. It
  never moves known-artist covers or ambiguous same-title songs. For covers
  across known artists, group candidates client-side and use duration plus your
  own knowledge; do not weaken the graph's conservative identity rule.
- **Read the folder structure early** (`RETURN t.path`): compilation/folder
  names ("29 Disco Fever", "Deep Focus") are often the strongest single
  signal for vibe grouping — stronger than any scalar.
- **Energy is a filter, not an ordering axis, for mood playlists** — feel-good
  pop clusters at 0.50–0.63, too flat to carry an arc; build arcs from musical
  character. And the scalar UNDERSELLS sparse guitar-band anthems (Mr.
  Brightside 0.58, Don't Stop Me Now 0.55 — the biggest felt peaks in the
  room). A poor peak slot is a measurable library-selection defect, not a cue
  to hand-rank the result. For intra-track builds
  ("a song that swells"), cross high `dynamic_range_db` (>20) with your own
  knowledge of the song — whole-track energy averages hide crescendos.
- **`duration_sec` is the trustworthy workhorse** — reliable everywhere, the
  cleanest pacing and version signal. On slow/acoustic material `bpm` is
  actively misleading (a fingerpicked ballad reads 157) — never sequence calm
  material by bpm.
- **Compilation folders can beat scalars**: `genre_tag` and album names often
  encode pre-curated mood buckets ("Deep Focus", "Morning Coffee"). If the
  library engine misses that signal, record it as a selection-feature gap.

## Acceptance evidence (diagnose; never hand-fix)

The library audit—not an agent checklist—is the export gate. It enforces music
and canonical eligibility, quality/duration bounds, Track/Song deduplication,
artist/album concentration and spacing, transitions, and requested arc. Inspect
its metrics and the explanation before claiming quality.

An explicit aggression directive also validates exact model provenance and all
diagnostics. Missing, abstained, incompatible, or invalid evidence produces the
hard `aggression_unknown` issue; explanation status preserves which case
occurred. Report that evidence honestly instead of substituting another signal.

Some human-facing claims remain useful diagnostics: an era request should be
supported by `era_source = 'original_year'`; a genre-critical peak should belong
to the requested style world; an unexpectedly long cut can still feel wrong for
the brief. Sanity-read artist/title/order and report concrete mismatches. If a
passing playlist is poor, do **not** drop, swap, or reorder tracks—file the
missing constraint/signal as a Sonagram library defect and improve the engine.
