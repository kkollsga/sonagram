# sonagram

**Ask your AI assistant for a playlist from your own music collection — and get
a file any music app can play.**

sonagram listens to every track you own (tempo, energy, mood, key — how songs
*feel*), organizes what it learns into a fast, searchable map of your library,
and hands that map to an AI agent. From there, playlists are a sentence: *"make
me a deep-focus work playlist"*, *"a party mix that builds"*, *"songs like this
one, but calmer"*. Your files are never modified or uploaded — everything runs
on your machine.

```{tip}
**Just want playlists?** You don't need most of this documentation. Install
(`pip install sonagram`), register your music
(`sonagram sources add ~/Music`), give your agent the bundled skill
(`sonagram skill install`) — and ask. The [Quickstart](quickstart.md) walks
through it; everything below the fold is for people building on top.
```

## What sonagram is (for the technically curious)

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

sonagram ships to PyPI with **prebuilt abi3 wheels** for macOS (Apple Silicon +
Intel), Linux x86_64 (manylinux2014), and Windows x86_64 — one wheel per
platform covers every CPython ≥ 3.9, so on these platforms nothing compiles:

```bash
pip install sonagram          # prebuilt wheel on common platforms
```

On other platforms pip falls back to the **sdist**, which compiles the native
core on install and needs a **Rust toolchain** ([rustup](https://rustup.rs)).

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
:caption: Under the hood (for developers)

graph-schema
agent-guide
determinism
```

## License

MIT © Kristian dF Kollsgård. sonagram is an independent project; it depends on
`sonara` (analysis) and `kglite` (engine) but is not otherwise affiliated with
them.
