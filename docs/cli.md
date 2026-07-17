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
a rescan only analyzes changed files. Prints a scan report: total files, analyzed
(new), reused (hash match / stat match), failed (with per-file messages), and
elapsed time.

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

```bash
sonagram playlist <library_root> <graph.kgl> \
    (--cypher '<query>' | --ids <hash1,hash2,...>) \
    (--out <file.m3u8> and/or --copy-to <dir>)
```

Resolve a track set from the graph and materialize it. Track order is preserved
verbatim — never re-sorted.

Pass **exactly one** selector:

- `--cypher '<query>'` — a read-only query whose result is a `Track`-node or
  `content_hash` column (`RETURN t.content_hash` or `RETURN t`).
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
