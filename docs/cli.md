# CLI

The `sonagram` command drives the whole pipeline: scan a library, optionally
enrich it, build the graph, probe freshness, and materialize playlists. The same
command is available two ways from one shared Rust code path — the `pip install
sonagram` console script and the standalone binary run identical parsing, output
strings, and exit codes, so they cannot drift.

Progress and stage lines go to **stderr**; results (reports, paths, counts) go
to **stdout**, so the CLI composes in a pipeline.

**Global flags** (any position): `-h`/`--help` prints usage, `-V`/`--version`
prints the version.

**Exit codes**: `0` on success, `1` on error. `status` is the exception — it
returns a freshness code (`0` fresh, `1` needs scan, `2` no cache).

## `sonagram scan`

```bash
sonagram scan <library_root>
```

Walk `<library_root>` for MP3s, content-hash each file, and run sonara analysis
on anything unseen, caching per-track records under `<library_root>/.sonagram/`.
Reuses cached analysis wherever the content hash (or file stats) is unchanged, so
a rescan only analyzes changed content. Same-hash paths share one analysis.
Prints a scan report: total files, unique hashes analyzed (new), compatible
cached analyses migrated without decoding audio, reused
(hash match / stat match), failed (with per-file messages), and
elapsed time.

Sonara 0.3.4 supplies Sonagram's fused aggression evidence. Its rank-v3 model
uses an optimized canonical 22.05 kHz aggression lane, making
the rank comparable across source sample rates while leaving the main analysis
and its provenance in the source-rate domain. Schema-5/rank-v2 caches require
an audio rescan: rank-v3 and its diagnostics cannot be regenerated from the
stored 48-D embedding. A null rank after that scan can be a valid model
abstention and remains cached with its support/components.

## `sonagram enrich`

```bash
sonagram enrich <library_root>
```

Fetch Last.fm metadata — popularity, folksonomy tags, MBIDs, similar
artists/tracks, and original-album mapping — for the library's artists, tracks,
and albums, caching it under `<library_root>/.sonagram/lastfm/`. Needs a
`LASTFM_API_KEY` (environment variable or a `.env` file in the current directory
or the library root). Re-runs skip already-fetched entities (incremental); a
per-entity fetch failure is recorded, never fatal. After `enrich`, `build` folds
the cache in automatically. Prints per-entity fetched / skipped / failed counts.

## `sonagram build`

```bash
sonagram build <library_root> <out.kgl>
```

Build the knowledge graph from the cached analysis records and save it to
`<out.kgl>`. Run `scan` first — with no cached records this errors. Auto-loads
the Last.fm enrichment cache when present (run `enrich` to populate it),
producing an *enriched* build; otherwise a plain build. Prints the track count
and the output path. See the [graph schema](graph-schema.md) for what the `.kgl`
contains.

## `sonagram playlist`

This is the lower-level materializer for explicit human-authored IDs/queries.
It preserves caller order but does not curate it. Agents should use `sonagram
curate` below for final playlists.

```bash
sonagram playlist <library_root> <graph.kgl> \
    (--cypher '<query>' | --ids <hash1,hash2,...>) \
    (--out <file.m3u8> and/or --copy-to <dir>)
```

Resolve a track set from the graph and materialize it. Track order is preserved
verbatim — never re-sorted.

Pass **exactly one** selector:

- `--cypher '<query>'` — a read-only query whose result is a `Track`-node or
  `content_hash` column (`RETURN t.content_hash` or `RETURN t`). It is worth
  giving that column a name — end the query with `RETURN t.content_hash AS
  content_hash`. Otherwise sonagram has to work out for itself which column
  holds the songs, and that guess follows the order you happened to list things
  in.
- `--ids <hash1,hash2,...>` — comma-separated content hashes directly.

Pass **at least one** destination (both are allowed together):

- `--out <file.m3u8>` — write a UTF-8 extended-M3U playlist with **absolute**
  paths.
- `--copy-to <dir>` — write a self-contained **portable** folder: the tracks
  copied as `NN - Artist - Title.<ext>` next to a **relative-path** `.m3u8`. The
  `.m3u8` is named after `--out`'s file stem when given, else the destination
  folder's own name, else `playlist`. Copies only — source files are never
  moved, retagged, or modified.

Each content hash resolves to its on-disk path (`library_root` + the track's
stored relative path). A hash matching no `Track` is reported, not silently
dropped.

**Central store (`--name`)**: pass `--name "<name>"` (with an optional
`--description "<text>"`) to save the playlist into the configured playlist store
(`~/.sonagram/playlists/`) as `<slug>.m3u8` + a `<slug>.meta.json` sidecar,
retrievable later with `sonagram playlists`. `--name` is a destination in its own
right — combine it with `--out`/`--copy-to` to also write those, or use it alone.
In the config-driven form (no path args), `--name` reads the configured graph.

## `sonagram profile` / `curate` / `audit` / `explain`

```bash
sonagram profile --format json
sonagram policy --preset focus --format json
sonagram curate --preset focus --tracks 25 \
    --name "Focused Thinking" --description "focused work" --format json
sonagram audit --ids h1,h2,h3 --preset focus --format json
sonagram explain --ids h1,h2,h3 --preset focus --format json
```

These are one deterministic library contract. `profile` reports curation
coverage/distributions. `curate` resolves a versioned preset policy, selects,
sequences, repairs, audits, explains, and stores only a passing playlist.
`audit` and `explain` independently evaluate an existing order. JSON callers
may pass complete `--brief-json` / `--policy-json` values; a preset mismatch is
rejected. A failed result is never silently relaxed or saved.

`policy` prints the complete preset DTO for safe typed amendments. Advanced
briefs distinguish pinned seeds from reference-only anchors; policies support
seed similarity/relative feature targets plus artist, genre, detected-style,
decade, and year eligibility. Unknown fields are rejected. Known constraints
that cannot be enforced belong in `brief.unsupported_intents` and make the
result structurally non-exportable.

Every preset leaves aggression neutral. Before setting
`eligibility.min_aggression` / `max_aggression`, `targets.aggression`, or
`targets.relative_aggression` and its margin, inspect `sonagram profile --format
json` for coverage, p25/median/p75, and exact model counts. Active aggression
directives validate the exact model and complete bounded diagnostics; missing,
abstained, incompatible, or invalid evidence yields `aggression_unknown`. The
rank is not a probability and `aggression_confidence` is evidence support, not
certainty. Audit/explain surface the status; the CLI never substitutes mood or
tension and never hand-ranks around it.

## `sonagram status`

```bash
sonagram status <library_root> [--format json]
```

A **read-only** freshness probe (mutates nothing): report how the cache under
`<library_root>/.sonagram/` compares to the files on disk, without hashing a file
or running analysis.

**Exit code** is the result: `0` = fresh, `1` = needs scan, `2` = no cache. The
default output is human-readable lines; `--format json` emits one stable object
with these keys:

| Key | Type | Meaning |
|---|---|---|
| `library_root` | string | the probed root |
| `has_cache` | bool | `.sonagram/index.json` exists |
| `total_files` | int | `*.mp3` files on disk |
| `fresh` | int | indexed, stats + record still fresh |
| `stale` | int | stats changed or record stale/missing |
| `missing_from_index` | int | on disk, never scanned |
| `deleted_in_index` | int | indexed, file now gone |
| `has_enrichment` | bool | Last.fm cache present & non-empty |
| `schema_version` | int | current sonara analysis schema |
| `similarity_version` | int | current sonara embedding version |
| `needs_scan` | bool | any stale/missing/deleted |
| `status` | string | `fresh` \| `needs_scan` \| `no_cache` |
| `exit_code` | int | `0` \| `1` \| `2`, matching the exit status |

**Config-driven form** (`sonagram status`, no path): probes every configured
source and also reports **graph freshness** — independently from source scan
work. Each source's currently usable, fresh indexed records are compared against
the graph's `Source.build_input_fingerprint`, which covers exact analysis values,
Sonara schema/similarity versions, and analysis model IDs. The aggregate JSON
adds:

| Key | Type | Meaning |
|---|---|---|
| `sources[].graph_current_for_cache` | bool\|null | exact usable cache inputs match the graph (null if no graph) |
| `sources[].graph_current` | bool\|null | compatibility alias for `graph_current_for_cache` |
| `graph` | string\|null | the configured graph path |
| `graph_present` | bool | the graph file exists |
| `graph_stale` | bool | the graph must be rebuilt (missing, unreadable, or a source drifted) |
| `graph_error` | string\|null | set when the file is there but this build cannot read it |

A stale graph is **action-worthy: exit `1`** even when every cache is fresh
(status `needs_build`) — the fix is a `sonagram build` (~1s from cache). The
inverse is also explicit: retryable source failures can make `needs_scan=true`
while `graph_current_for_cache=true`; the graph still exactly represents every
analysis currently usable from the cache.

## `sonagram sources`

```bash
sonagram sources add <dir>      # register a library folder (canonicalized, deduped)
sonagram sources remove <dir>   # unregister
sonagram sources list           # show the registry
```

Manage the configured source registry (`~/.sonagram/config.json`). Once a source
is registered, the bare config-driven forms (`sonagram scan` / `build` / `status`
/ `enrich`, and `playlist ... --name`) fan out over every source with no path
arguments.

## `sonagram config`

```bash
sonagram config                             # show the resolved config
sonagram config set graph <path>            # override the central graph location
sonagram config set playlists_dir <path>    # override the playlist-store location
```

Show the resolved config — sources, the central graph and playlist-store paths
(defaults under `~/.sonagram/`, flagged `[default]`), whether each file exists,
and whether a Last.fm key is configured (the location only, never the key).

## `sonagram playlists`

```bash
sonagram playlists              # list stored playlists (newest first)
sonagram playlists show <slug>  # full metadata + tracklist for one playlist
```

Read the central playlist store built by `playlist --name`.

## `sonagram skill`

```bash
sonagram skill show                          # print the bundled agent skill
sonagram skill install [--dir <skills_root>] [--force]
```

`show` prints the embedded `sonagram-playlist` skill to stdout. `install` writes
it to `<skills_root>/sonagram-playlist/SKILL.md` (default `~/.claude/skills`),
creating any missing directories. Install personalizes the file from your config
(substituting the library-root placeholder with your first configured source),
refuses to overwrite an existing file unless `--force`, and prints a reminder to
**read and follow the file now, in-session** — skills only auto-load at the next
session start.

## `sonagram mcp`

```bash
sonagram mcp install [--force]
```

Install an auto-detected `<graph>_mcp.yaml` plus five live-gated project skills
beside the configured graph. Re-running identical assets is a no-op; differing
operator files require `--force`. The manifest exposes only a dedicated empty
source sandbox, never the graph/config parent, and prints the absolute executable
`sonagram-mcp-server` sibling from the installed environment when available
(`RUN (PowerShell)` on Windows). This thin KGLite 0.16.5 frontend adds typed
profile/policy/curate/audit/explain and playlist-store tools while preserving
KGLite's generic server and live graph lifecycle.

## Examples

```bash
sonagram scan   ~/Music
sonagram build  ~/Music music.kgl
sonagram status ~/Music --format json
sonagram playlist ~/Music music.kgl \
    --cypher 'MATCH (t:Track) WHERE t.bpm > 120 RETURN t.content_hash ORDER BY t.energy' \
    --out set.m3u8
sonagram playlist ~/Music music.kgl \
    --ids h1,h2,h3 --copy-to ~/Desktop/roadtrip
```
