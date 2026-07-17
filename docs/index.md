# sonagram

Scan a music library, analyze every track with
[sonara](https://github.com/kkollsga/sonara), and structure the results into a
queryable [kglite](https://github.com/kkollsga/kglite) knowledge graph — so AI
agents can reason over a music collection: filter it, group it, find what's
*similar but calmer*, detect styles nobody tagged, and materialize the answer as
a playable `.m3u8` playlist.

## What sonagram is

sonagram is a **graph builder over two upstreams**. It owns the mapping and the
schema between them, and nothing else:

- **sonara** supplies per-track analysis — tempo, key/Camelot, energy, valence,
  danceability, acousticness, mood, loudness, structure, and a versioned 48-dim
  similarity embedding.
- **kglite** supplies the graph engine — storage, Cypher, vector search, and MCP
  exposure.
- **sonagram** owns the mapping and the schema between them: fat `Track` nodes
  with every filterable signal flat, dimension nodes (`Artist`, `Genre`, `Key`,
  `TempoBand`, `Style`, …) for grouping and discovery, materialized `SIMILAR_TO`
  edges for traversable similarity, and Camelot-wheel edges for harmonic
  set-building. The graph is designed backward from agent playlist-queries and
  is [deterministic](determinism.md) byte-for-byte across rescans.

## The pipeline

```text
  music library (*.mp3)
        │  sonagram scan      walk → hash → sonara analysis → .sonagram/ cache
        ▼
  per-track analysis cache
        │  sonagram enrich    (optional) Last.fm metadata → .sonagram/lastfm/
        ▼
  cache (+ enrichment)
        │  sonagram build     map records → nodes + edges + embeddings
        ▼
  music.kgl  (a kglite knowledge graph)
        │  kglite-mcp-server --graph music.kgl
        ▼
  AI agent  ──cypher_query / graph_overview──▶  a track set + order
        │  sonagram playlist  resolve content-hashes → .m3u8 (+ portable folder)
        ▼
  playable .m3u8 playlist
```

## Install

sonagram ships to PyPI as an **sdist** (a source distribution that compiles the
native Rust core on install), so a **Rust toolchain** ([rustup](https://rustup.rs))
must be present on the machine:

```bash
pip install sonagram          # builds the Rust core — needs a Rust toolchain
```

`pip install sonagram` also installs kglite (the runtime graph engine) and gives
you the same `sonagram` command as the standalone binary — one shared code path,
so the CLI and the Python library cannot drift. See the
[Quickstart](quickstart.md) for the end-to-end flow.

## Contents

```{toctree}
:maxdepth: 1
:caption: Using sonagram

quickstart
cli
python-api
```

```{toctree}
:maxdepth: 1
:caption: The graph

graph-schema
agent-guide
determinism
```

## License

MIT © Kristian dF Kollsgård. sonagram is an independent project; it depends on
`sonara` (analysis) and `kglite` (engine) but is not otherwise affiliated with
them.
