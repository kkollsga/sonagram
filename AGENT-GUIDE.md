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
| `year` | int | yes | release year (tag) |
| `track_no` | int | yes | track number (tag) |
| `file_size` | int | no | bytes |
| `duration_sec` | float | no | length in seconds |
| `bpm` | float | no | tempo, typical 60–180 |
| `bpm_raw` | float | no | pre-octave-correction tempo |
| `energy` | float | yes | perceptual energy 0–1 |
| `valence` | float | yes | musical positivity 0–1 |
| `danceability` | float | yes | 0–1 |
| `acousticness` | float | yes | 0–1 |
| `vocalness` | float | yes | 0–1 (higher = more vocal) |
| `instrumentalness` | float | yes | 0–1 (heuristic v1) |
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
| `FROM_DECADE` | `Track`→`Decade` | — | only if year present |
| `SIMILAR_TO` | `Track`→`Track` | `score` 0–1 | top-10 kNN; **directed** (A→B ≠ B→A) |
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
- **Keys are unreliable right now** (dated 2026-07-17, remove when upstream
  fixes it): sonara's key detection has a strong **F-major bias** — on the
  15-track fixture set, 13 of 15 tracks read as `F major` / `7B`. Treat `key` /
  `camelot` / harmonic-mixing (archetype 4) as low-confidence until the upstream
  fix lands; cross-check with `key_confidence`.
- **`genre_tag` is spotty real-world data** — it's whatever the file's ID3 tag
  says (or null). It is not an audio-derived classification. Expect missing,
  inconsistent, and idiosyncratic values (`"rap & hip-hop"` vs `"hip-hop"`).
- **`SIMILAR_TO` is directed** kNN: `A→B` does **not** imply `B→A`. To find
  mutual neighbours, match both directions explicitly.
- **`Style` nodes are similarity communities, not human genres.** A style name
  like `upbeat-electric-dance` is a deterministic template over the cluster's
  mean features — **read `top_genres`, `mean_*`, and `exemplar_titles`** to know
  what it actually contains, rather than trusting the name.
- **`mood_*` and `instrumentalness` are heuristic v1** — directional, not
  calibrated ground truth. Good for coarse ranking, not hard thresholds.
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
- **`danceability` saturates high** (library mean ~0.89) — near-useless as a
  filter alone; cross it with genre or energy.
- **`bpm` is unreliable on low-onset material** (ambient/classical can read
  145–157). Order calm sets by `energy`, not `bpm`.
- **Style quality degrades on heterogeneous libraries**: expect one cap-sized
  catch-all community plus small tight slivers. Styles are a lead, not a
  survey — for "what's in here", aggregate over `Genre`/`Decade`/`TempoBand`
  and read Style `top_genres`/`exemplar_titles` skeptically.
- **Close values need full precision**: two tracks can both display `0.462`
  yet differ at the 5th decimal — don't judge strict ordering from rounded
  output.
