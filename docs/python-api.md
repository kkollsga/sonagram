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
given the `.kgl` is persisted (that is the file `sonagram-mcp-server` serves);
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
`analyzed`, `migrated_analysis`, `reused_hash_match`, `reused_stat_match`, `failed` (a list of
`(path, message)` tuples), and `elapsed_sec`.

The default Sonara 0.3.3 feature set includes fused aggression analysis. Sonara
normalizes the aggression branch to a canonical 22.05 kHz lane so rank-v3 is
comparable across source sample rates; the rest of the analysis and provenance
remain in the source-rate domain. Schema-5/rank-v2 caches require an audio
rescan because rank-v3 cannot be reconstructed from a stored embedding. A null
rank with complete diagnostics is a valid abstention, not a scan failure.

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
and kept — that is the file `sonagram-mcp-server --graph` serves. Auto-folds in the
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

`export_m3u` is a lower-level materializer: it preserves caller order but does
not curate it. Agent-created playlists should use `curate_playlist`.

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

## Curation: `profile_library`, `curate_playlist`, `audit_playlist`, `explain_playlist`

```python
profile = sonagram.profile_library("music.kgl")
policy = sonagram.curation_policy("focus")
brief = {
    "preset": "focus",
    "target_tracks": 25,
    "target_duration_sec": None,
    "seed_ids": [],
    "seed_role": "pinned",
    "unsupported_intents": [],
}
result = sonagram.curate_playlist("music.kgl", brief, policy)
assert result["exportable"] and result["audit"]["passed"]

audit = sonagram.audit_playlist(
    "music.kgl", result["track_ids"], result["policy"], brief=brief
)
explanation = sonagram.explain_playlist(
    "music.kgl", result["track_ids"], result["policy"], brief=brief
)
```

All inputs/outputs are plain JSON-compatible Python values serialized from the
same Rust DTOs as the CLI. A missing policy resolves from the brief preset for
curation and to `general` for standalone audit/explain. Malformed values,
brief/policy preset mismatches, and empty ID lists raise `ValueError`; graph/IO
failures raise `RuntimeError`. A non-exportable curation result is returned
normally with structured audit issues.

`curation_policy(preset)` returns the complete versioned DTO before you amend
typed seed-relative targets or categorical eligibility. Unknown keys are
rejected; `brief.unsupported_intents` records constraints the library cannot
enforce and deliberately produces a non-exportable result.

All presets are aggression-neutral. For an explicit request, inspect
`profile["stats"]` coverage and p25/median/p75 plus
`profile["aggression_models"]`, then amend
either `policy["eligibility"]["min_aggression"]` or
`policy["eligibility"]["max_aggression"]`,
`policy["targets"]["aggression"]`, or
`policy["targets"]["relative_aggression"]` and
`relative_aggression_margin`. Scores are ranks, confidence is content/evidence
support, and comparability requires the exact same model id. Missing, abstained,
incompatible, or invalid evidence produces the hard audit issue
`aggression_unknown`; no preset or API falls back to mood/tension/energy.

## Errors

Bad-argument / bad-input failures raise `ValueError`; scan / graph / IO failures
raise `RuntimeError`.

## Reference stubs

The full signatures live in the shipped type stubs (`sonagram/__init__.pyi`) —
your editor and `help(sonagram.build)` surface them directly.
