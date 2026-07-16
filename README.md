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

## Planned shape

```
sonagram/          pure-Rust core (scan, hash, cache, map, m3u export)
sonagram-python/   PyO3 bindings
python/sonagram/   Python wrapper
tests/fixtures/    frozen TrackAnalysis records (never audio)
```

License: MIT
