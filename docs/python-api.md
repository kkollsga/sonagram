# Python API

`import sonagram` exposes the same pipeline as the [CLI](cli.md), as a handful of
functions. The pure-Rust core is compiled into the `_sonagram` extension; this
module is a thin re-export of its surface.

```bash
pip install sonagram        # builds the Rust core (sdist) — needs a Rust toolchain
```

```python
import sonagram

report = sonagram.scan("~/Music")
g = sonagram.build("~/Music", "music.kgl")   # returns a real kglite.KnowledgeGraph
g.cypher("MATCH (t:Track) RETURN t.title LIMIT 10")
```

## The `.kgl`-bytes handoff

`build()` and `scan_and_build()` return a live `kglite.KnowledgeGraph`. The
`sonagram` and `kglite` wheels are two separate compiled extensions and cannot
share a live Rust graph object, so `build()`:

1. constructs the graph with sonagram's native builder,
2. serializes it to a `.kgl`, and
3. calls the **installed** `kglite` wheel's `load()` and returns *that* object.

The result is a genuine `kglite.KnowledgeGraph`, so every downstream kglite API
(`.cypher()`, `.describe()`, persistence, …) works unchanged. When `out_path` is
given the `.kgl` is persisted (that is the file `kglite-mcp-server` serves);
otherwise a temp file carries the bytes and is deleted once the graph is
materialized. This is why `sonagram` depends on `kglite` at runtime.

## `scan`

```python
sonagram.scan(
    library_root,
    *,
    progress=None,          # callable: progress(stage: str, done: int, total: int)
) -> dict
```

Walk `library_root` for MP3s, reuse cached analysis wherever the content hash is
unchanged, and analyze only unseen files. `progress`, if given, must be callable
and is invoked as `progress(stage, done, total)` where `stage` is one of
`"walk"`, `"hash"`, `"analyze"`, `"done"`. Returns a dict: `total_files`,
`analyzed`, `reused_hash_match`, `reused_stat_match`, `failed` (a list of
`(path, message)` tuples), and `elapsed_sec`.

## `enrich`

```python
sonagram.enrich(
    library_root,
    *,
    api_key=None,           # overrides LASTFM_API_KEY env / .env
) -> dict
```

Fetch Last.fm enrichment and cache it under `<library_root>/.sonagram/lastfm/`.
Resolves the API key from `api_key=`, else `LASTFM_API_KEY`, else a `.env` file;
a missing key raises `RuntimeError`. Fetches popularity, folksonomy tags, MBIDs,
similar artists/tracks (with match weights), and original-album mapping for every
entity not already cached (incremental). Per-entity failures are soft, never
fatal. Returns a dict of per-entity `*_fetched` / `*_skipped` / `*_failed` counts
plus `elapsed_sec`. Afterward, `build()` / `scan_and_build()` pick the cache up
automatically.

## `build`

```python
sonagram.build(
    library_root,
    out_path=None,          # also write the graph to this .kgl path
) -> kglite.KnowledgeGraph
```

Build the graph from `library_root`'s cached analysis records and return a live
`kglite.KnowledgeGraph`. Run `scan()` first (this reads the cache under
`<library_root>/.sonagram/`). If `out_path` is given the `.kgl` is written there
and kept — that is the file `kglite-mcp-server --graph` serves. Auto-folds in the
Last.fm enrichment cache when present.

## `scan_and_build`

```python
sonagram.scan_and_build(
    library_root,
    out_path=None,
    *,
    progress=None,
) -> kglite.KnowledgeGraph
```

Convenience composition of `scan()` then `build()` over one library — scans
(forwarding `progress`), then builds and returns the `kglite.KnowledgeGraph`,
persisting the `.kgl` to `out_path` when given.

## `export_m3u`

```python
sonagram.export_m3u(
    kgl_path,
    library_root,
    out_path,
    *,
    cypher=None,            # a read-only query → Track-node or content-hash column
    track_ids=None,         # content hashes, order preserved
    copy_to=None,           # also export a portable folder here
) -> str
```

Export a playlist from a saved graph. Loads the graph from `kgl_path`, resolves a
track set — pass **exactly one** of `cypher=` or `track_ids=` — and joins each
track's relative path onto `library_root`. Always writes a UTF-8 extended-M3U
playlist (absolute paths) to `out_path`. When `copy_to=` is given, also exports a
self-contained **portable folder** there: the tracks copied as `NN - Artist -
Title.<ext>` next to a relative-path `.m3u8` (named after `out_path`'s stem).
Copies only — source files are never moved, retagged, or modified. Returns the
`copy_to` playlist path when `copy_to=` is set, else `str(out_path)`.

```python
# from a query:
sonagram.export_m3u("music.kgl", "~/Music", "house.m3u8",
                    cypher="MATCH (t:Track) WHERE t.bpm>=110 AND t.bpm<125 "
                           "RETURN t.content_hash ORDER BY t.energy DESC")
# or from ids:
sonagram.export_m3u("music.kgl", "~/Music", "set.m3u8",
                    track_ids=["<hash1>", "<hash2>"])
```

## Errors

Bad-argument / bad-input failures (playlist resolution: missing ids, empty set,
unusable Cypher result) raise `ValueError`; scan / graph / IO failures raise
`RuntimeError`.

## Reference stubs

The full signatures live in the shipped type stubs (`sonagram/__init__.pyi`) —
your editor and `help(sonagram.build)` surface them directly.
