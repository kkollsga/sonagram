# AGENT-GUIDE — working a sonagram music graph

You are an AI agent with MCP access to a **sonagram** knowledge graph: a music
library mapped into [kglite](https://github.com/kkollsga/kglite) and served by
`kglite-mcp-server`. You have two tools that matter:

- **`cypher_query`** — run one openCypher query, get up to ~15 rows inline.
  It takes a single `query` string and **nothing else**: there is no parameter
  binding over MCP, so **inline every literal** (`{title:'Marry You'}`,
  `[0.1, ...]`) — a `$param` reference errors with `Missing parameter`.
- **`graph_overview`** — the node/edge inventory with live counts and sample
  ids. Call it first on an unfamiliar graph to see the library's shape.

## Mental model
Every audio file is one **`Track`** node — the fat hub. Every signal an agent
filters or sorts by (bpm, energy, valence, key, loudness, mood_*, …) is a **flat
scalar property on `Track`**, so filtering never needs a join. Around the hub sit
small **dimension nodes** you group and traverse by (`Artist`, `Album`, `Genre`,
`Key`, `TempoBand`, `EnergyLevel`, `Decade`) plus two **derived** structures:
`Track-[:SIMILAR_TO]->Track` (top-10 nearest neighbours by audio embedding) and
`Style` community nodes (tracks that cluster tightly in similarity, with a
readable profile). `Key-[:CAMELOT_ADJACENT]->Key` encodes the harmonic-mixing
wheel. That is the whole graph: hub + dimensions + similarity + styles.

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
| `arousal_index` | float | yes | energy/brightness/rhythm intensity — the well-predicted mood axis |
| `valence_index` | float | yes | musical positiveness — **WEAK PRIOR** (literature R² 0.12–0.28): rank with it, never hard-filter |
| `tension_index` | float | yes | dissonance/minor/harmonic-complexity — "deep thinking" material = low-mid `arousal_index` × mid-high `tension_index` × high `chord_entropy` |
| `recording_quality` | float | yes | audio-only production/provenance quality (validated: separates studio masters from bootlegs, AUC 0.75 scalar-only, d=0.93 with curves) |
| `quality_tier` | str | yes | `"high"` / `"mid"` / `"low"` — percentile thirds of `recording_quality`, for cheap WHERE clauses |
| `is_canonical` | bool | no | false only for inferior members of a version group — `WHERE t.is_canonical` is the universal "skip duplicate/inferior takes" filter |

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
| `Song` (P21, v2) | `artist_id\|normalized_title` | `title`, `artist`, `n_versions`, `canonical_hash` — exists only when ≥2 recordings share a version key; the members link in via `VERSION_OF` |
| `Library` (1) | label | `path`, `n_tracks`, `schema_version` |

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
| `SIMILAR_TO` | `Track`→`Track` | `score` 0–1 | top-10 kNN; **directed** (A→B ≠ B→A) |
| `VERSION_OF` | `Track`→`Song` | — | recordings of the same composition (P21); the Song's `canonical_hash` names the best take |
| `IN_STYLE` | `Track`→`Style` | `membership` (1.0 in v1) | tracks in a similarity community |
| `CAMELOT_ADJACENT` | `Key`→`Key` | `transition` = `energy_up`/`energy_down`/`mode_switch` | the Camelot wheel (72 edges) |

## Query cookbook
All four archetypes, copy-paste runnable (inline literals — no `$params`).

**1 — Filter + order** ("house-tempo tracks, calmest first"):
```cypher
MATCH (t:Track)
WHERE t.bpm >= 110 AND t.bpm < 125 AND t.vocalness < 0.5
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
WHERE t.energy < s.energy
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

**4 — Sequence** (harmonic step + energy ramp, for a DJ set) — walk to a
Camelot-adjacent key and pick a higher-energy, tempo-close track:
```cypher
MATCH (a:Track)-[:IN_KEY]->(k1:Key)-[e:CAMELOT_ADJACENT]->(k2:Key)<-[:IN_KEY]-(b:Track)
WHERE a.title = 'Just Like a Woman' AND b.energy > a.energy AND abs(b.bpm - a.bpm) < 15
RETURN b.title, b.artist_name, b.bpm, b.energy, e.transition
ORDER BY b.energy LIMIT 10
```

## Materializing a playlist
A query answers *which tracks in what order*; you turn that into a playable
`.m3u8` outside the graph, via the sonagram CLI or Python. Both take the
`content_hash` values a query returns (`RETURN t.content_hash`, or `RETURN t`).

CLI — pass the query directly (order is preserved verbatim, never re-sorted):
```sh
sonagram playlist <library_root> music.kgl \
  --cypher 'MATCH (t:Track) WHERE t.bpm >= 110 AND t.bpm < 125 RETURN t.content_hash ORDER BY t.energy DESC' \
  --out house.m3u8
# or from an explicit id list you already have:
sonagram playlist <library_root> music.kgl --ids <hash1>,<hash2> --out set.m3u8
```

Python:
```python
import sonagram
# from a query:
sonagram.export_m3u("music.kgl", "<library_root>", "house.m3u8",
                    cypher="MATCH (t:Track) WHERE t.bpm>=110 AND t.bpm<125 "
                           "RETURN t.content_hash ORDER BY t.energy DESC")
# or from ids:
sonagram.export_m3u("music.kgl", "<library_root>", "set.m3u8",
                    track_ids=["<hash1>", "<hash2>"])
```
`content_hash` is the stable join key end to end: a query returns hashes → export
resolves each hash's on-disk path (library_root + relative path) into the
playlist. A hash matching no Track is reported, not silently dropped.

## Pitfalls
- **Keys are trustworthy** (sonara ≥ 0.2.3 / graph schema v2; the old F-major
  bias is fixed and validated). Interpreting `key_confidence`: the scale runs
  low — library mean ≈ 0.17; **≥ 0.3 is solid, ≥ 0.45 is strong**. Note that
  high-energy dense mixes tend to score lower confidence, so a strict energy
  floor and a strict confidence floor fight each other — relax one.
- **`vocalness`/`instrumentalness` are FIXED as of analysis schema v3** (sonara
  0.2.4; check `t.analysis_schema_version >= 3`). The v2 heuristic now scores
  **low = instrumental, high = vocal (harsh/screamed highest, clean singing mid,
  solo instruments low)** — trustworthy for instrumental filtering: use
  `vocalness < ~0.35` to find instrumentals and `>= ~0.55` to require vocals.
  Two caveats remain: (1) a **voice-mimicking solo instrument** (nylon-guitar
  flamenco, solo violin, cathedral organ) can still read mid-high — the
  documented ambiguous case; cross-check with genre/artist for pure-instrumental
  asks. (2) `vocalness` measures *presence*, not *singability* — a high score
  says "a voice is prominent", not "great singalong". `instrumentalness` is
  exactly `1 − vocalness` (collinear — no added signal; prefer `vocalness`).
  **On a pre-v3 graph the OLD values INVERT** (screamed metal lowest, solo sax
  highest) — if you see `analysis_schema_version < 3`, do not trust vocalness for
  instrumental filtering; lead with genre/compilation tags instead.
- **Gate `bpm` on `bpm_confidence` before trusting it.** New in schema v3:
  `bpm_confidence` (0–1) flags when the tempo estimate is solid. Steady dance/pop
  reads 0.7–0.9; ambient/rubato/sparse-onset material (classical, drone,
  fingerpicked ballads) reads ~0.4 and its `bpm` is often octave-wrong or
  meaningless. Add `WHERE t.bpm_confidence >= 0.6` before ordering or filtering by
  `bpm`, and for calm/acoustic sets order by `energy` or `duration_sec`, never
  `bpm`.
- **`mood_aggressive` inverts on genuinely extreme material** (same heuristic
  family): scalar-top "aggressive" tracks were Bros and Paula Abdul while
  Slayer reads 0.25–0.34. For hard/metal asks lead with artist knowledge +
  **`loudness_lufs`** (−9 to −11 = modern brickwalled extreme vs −15 vintage)
  + `energy` + `onset_density`. `mood_happy`, by contrast, has proven a
  trustworthy primary sorter for feel-good pool ranking.
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
  for coarse ranking, not hard thresholds. (`vocalness`/`instrumentalness` are v2
  and now trustworthy — see the vocalness pitfall above.)
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
- **Multi-constraint ordering isn't a Cypher `ORDER BY`.** A dual ramp (smooth
  BPM steps AND strictly climbing energy) needs candidate export + client-side
  selection (the two signals are typically uncorrelated). Fetch rows, plan
  outside, then export with `--ids` (order is preserved verbatim).
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
- **For any hand-built arc (ramp, breather, harmonic chain), `--ids` is the
  primary export path**, not `--cypher`: export candidates (`.to_dicts()`),
  order client-side, feed the hash list — order is preserved verbatim. A
  12-track Camelot chain or a mood arc with a mid-set dip cannot be expressed
  as one `ORDER BY`.
- **Mood-playlist recipe** (feel-good, chill, angry…): filter on the scalar
  stack (`mood_*`, `valence`, `energy`, `vocalness` for singability,
  `mood_aggressive` as an exclusion), calibrate thresholds against library
  averages first, build the arc client-side. Check `duration_sec` — LP/extended
  cuts (7–8 min) hide in compilations and derail pacing.
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
  (id = content-hash of the audio), so two Track nodes sharing a song title
  are GUARANTEED different recordings. Recipe: pull title+artist+scalars,
  normalize titles client-side (case, parentheticals, "- Live/Remaster"
  suffixes), group, keep groups with 2+ nodes; `duration_sec` deltas are the
  cleanest version-discriminator (halved = solo cover, doubled =
  live/extended). Do string work in Python, not Cypher.
- **Read the folder structure early** (`RETURN t.path`): compilation/folder
  names ("29 Disco Fever", "Deep Focus") are often the strongest single
  signal for vibe grouping — stronger than any scalar.
- **Energy is a filter, not an ordering axis, for mood playlists** — feel-good
  pop clusters at 0.50–0.63, too flat to carry an arc; build arcs from musical
  character. And the scalar UNDERSELLS sparse guitar-band anthems (Mr.
  Brightside 0.58, Don't Stop Me Now 0.55 — the biggest felt peaks in the
  room): hand-rank the critical peak slots. For intra-track builds
  ("a song that swells"), cross high `dynamic_range_db` (>20) with your own
  knowledge of the song — whole-track energy averages hide crescendos.
- **`duration_sec` is the trustworthy workhorse** — reliable everywhere, the
  cleanest pacing and version signal. On slow/acoustic material `bpm` is
  actively misleading (a fingerpicked ballad reads 157) — never sequence calm
  material by bpm.
- **Compilation folders can beat scalars**: `genre_tag` and album names often
  encode pre-curated mood buckets ("Deep Focus", "Morning Coffee") — exploit
  them deliberately for mood/focus asks.

## Quality bar (every playlist, before you export)
QC audits grade on these; treat them as requirements, not suggestions:
0. **Filter `t.is_canonical` and prefer `quality_tier <> 'low'` by DEFAULT.**
   On collector libraries the candidate pool is otherwise flooded with session
   outtakes, bootlegs, and duplicate takes that pass every scalar filter. Only
   drop these guards when the brief explicitly wants rarities/outtakes. For
   mood asks, curate on the axes (`arousal_index`/`tension_index` percentiles +
   curve features like `flow_smoothness`, `chord_entropy`), not on raw `energy`
   thresholds alone — and treat `valence_index` as a ranking hint, never a
   hard filter.
1. **Duration check every pick.** Casual/mood playlists default to
   radio-length cuts (`duration_sec <= 330`) unless the brief wants epics —
   a 8:15 LP cut mid-road-trip reads as a pacing defect. Always `RETURN
   t.duration_sec` in your candidate query.
2. **Era claims: audit `era_source` first, then fall back to YOUR knowledge.**
   When `t.era_source = 'original_year'` the decade is the true release year —
   graph-auditable, trust it. When `era_source = 'file_year'` (the common case)
   `year` is the file tag, which on compilations/reissues is the reissue date (a
   1958 Sinatra recording tagged 2011); validate each such pick against what you
   know about the artist/recording and drop picks you can't vouch for.
3. **Critical slots need style-world cohesion, not just scalar fit.** The
   finale of a singalong set, the seed-side of a "like this" set, the peak of
   a genre set: check the pick belongs to the brief's musical world (genre
   family + your own knowledge of the track), because scalars will happily
   pass a rap track into a disco set.
4. **Sanity-read the final tracklist as a human would** — artist/title, in
   order, out loud. If any pick needs a defensive explanation, swap it.
