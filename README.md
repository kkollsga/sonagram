# sonagram

Scan a music library, analyze every track with [sonara](https://github.com/kkollsga/sonara),
and structure the results into a queryable [kglite](https://github.com/kkollsga/kglite)
knowledge graph — so AI agents can reason over your music collection: filter it,
group it, find what's *similar but calmer*, detect styles nobody tagged, and
materialize the answer as a playable `.m3u8` playlist.

**Status: under construction** — bootstrap in progress.

## The idea

- **sonara** supplies per-track analysis: tempo, key/Camelot, energy, valence,
  danceability, acousticness, mood, loudness, structure, and a versioned 48-dim
  similarity embedding.
- **kglite** supplies the graph engine: storage, Cypher, vector search, MCP
  exposure.
- **sonagram** owns the mapping and the schema between them: fat `Track` nodes
  with every filterable signal flat, dimension nodes (`Artist`, `Genre`, `Key`,
  `TempoBand`, `Style`, …) for grouping and discovery, materialized
  `SIMILAR_TO` edges for traversable similarity, Camelot-wheel edges for
  harmonic set-building — a graph designed from agent playlist-queries
  backward, deterministic byte-for-byte across rescans.

## Quick start

Install from PyPI. sonagram ships as an **sdist** (a source distribution that
compiles the native core on install), so you need a **Rust toolchain**
([rustup](https://rustup.rs)) on the machine:

```bash
pip install sonagram          # builds the Rust core — needs a Rust toolchain
export LASTFM_API_KEY=...      # optional: enables `enrich`
```

`pip install sonagram` gives you the same `sonagram` command as the standalone
binary — one shared code path, so the CLI and the library cannot drift. The
end-to-end flow:

```bash
sonagram scan  ~/Music                 # walk, hash, analyze → .sonagram/ cache
sonagram enrich ~/Music                # optional: fold in Last.fm metadata
sonagram build ~/Music music.kgl       # cached analysis → queryable .kgl graph
kglite-mcp-server --graph music.kgl    # serve the graph to an AI agent over MCP
```

The agent then queries the graph over MCP (Cypher, vector similarity, grouping)
and turns an answer into a playable playlist:

```bash
sonagram playlist ~/Music music.kgl \
    --cypher 'MATCH (t:Track) WHERE t.bpm > 120 RETURN t.content_hash ORDER BY t.energy' \
    --copy-to ~/Desktop/roadtrip       # portable folder: copied tracks + .m3u8
```

Everything is **incremental and read-only where it can be**: `scan` re-analyzes
only changed files, and `enrich` skips already-fetched entities.

### Freshness probe for automation

`sonagram status <library_root>` is a **read-only** probe (it mutates nothing)
that a skill or CI step can chain before deciding whether to rescan:

```bash
sonagram status ~/Music --format json   # one stable JSON object
# exit code: 0 = fresh, 1 = needs scan, 2 = no cache
```

It compares the files on disk against the `.sonagram/` cache — counting fresh /
stale / newly-added / deleted tracks and checking each record against the
current sonara analysis schema — without hashing a file or running analysis.
Chain it as: `status` → `scan`/`build` if needed → query via MCP.

## Planned shape

```
sonagram/          pure-Rust core (scan, hash, cache, map, m3u export)
sonagram-python/   PyO3 bindings
python/sonagram/   Python wrapper
tests/fixtures/    frozen TrackAnalysis records (never audio)
```

License: MIT
