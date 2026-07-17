# sonagram

Scan a music library, analyze every track with [sonara](https://github.com/kkollsga/sonara),
and structure the results into a queryable [kglite](https://github.com/kkollsga/kglite)
knowledge graph — so AI agents can reason over your music collection: filter it,
group it, find what's *similar but calmer*, detect styles nobody tagged, and
materialize the answer as a playable `.m3u8` playlist.

**Status: under construction** — bootstrap in progress.

## Use it through an agent (no code)

The main way to use sonagram is to just **ask an AI agent** (e.g. Claude Code)
for a playlist. A bundled skill does the rest — freshness check, scan, graph
build, curation, and export — so you never touch the CLI.

**One-time setup (3 steps):**

1. **Install** — `pip install sonagram`. sonagram ships as an sdist (it compiles
   a native core on install), so you need a **Rust toolchain**
   ([rustup](https://rustup.rs)) on the machine.
2. **Register your music** — `sonagram sources add ~/Music` (repeat for other
   folders).
3. **Install the skill** — copy `skills/sonagram-playlist/` into
   `~/.claude/skills/` and fill in your library path.

Then just type what you want at your agent:

- "make me a deep-focus work playlist"
- "a party mix for Saturday that builds"
- "songs like *Teardrop* but calmer"
- "which songs do I have multiple versions of? pair them"
- "what's even in my library?"

Under the hood the skill runs a read-only freshness probe, does an
**incremental** scan only if something changed, (re)builds the graph, curates
against a Quality bar (duration, era, style-world cohesion), and writes a named
`.m3u8` plus a metadata sidecar into a central store — openable in any music app
and retrievable later with `sonagram playlists`.

> **Optional: Last.fm enrichment.** Adding a free Last.fm API key folds in richer
> genres, popularity, and crowd-similarity for better picks. The Claude skill can
> walk you through getting one and storing it — just ask.

## CLI (scriptable)

`pip install sonagram` also gives you the standalone `sonagram` command (the same
shared code path as the library, so the two frontends cannot drift). Once a
source is registered, the flow is config-driven — no path arguments:

```bash
sonagram sources add ~/Music     # register a library folder (repeatable)
sonagram status                  # freshness of every source (exit 0/1/2)
sonagram scan                    # walk, hash, analyze → per-source .sonagram/ cache
sonagram enrich                  # optional: fold in Last.fm metadata (needs a key)
sonagram build                   # merge all sources → the central .kgl graph
sonagram playlist --ids h1,h2,h3 \
    --name "Deep Focus" --description "a calm work playlist"
sonagram playlists               # list stored playlists (newest first)
```

`sonagram config` shows the resolved graph + playlist-store paths (defaults under
`~/.sonagram/`) and whether a Last.fm key is configured; `sonagram config set
graph|playlists_dir <path>` overrides them.

**Explicit-path forms** still work exactly as before, for scripting a single
library without touching the config:

```bash
sonagram scan  ~/Music
sonagram build ~/Music music.kgl
sonagram status ~/Music --format json          # one stable JSON object
sonagram playlist ~/Music music.kgl \
    --cypher 'MATCH (t:Track) WHERE t.bpm > 120 RETURN t.content_hash ORDER BY t.energy' \
    --copy-to ~/Desktop/roadtrip               # portable folder: copied tracks + .m3u8
```

Everything is **incremental and read-only where it can be**: `scan` re-analyzes
only changed files (a no-op rescan analyzes nothing), and `enrich` skips
already-fetched entities.

## For building your own agents / integrations

Serve the graph to any MCP-speaking agent, or drive sonagram from Python:

```bash
kglite-mcp-server --graph ~/.sonagram/music.kgl   # expose the graph over MCP
```

```python
import sonagram
sonagram.scan("~/Music")
g = sonagram.build("~/Music", out_path="music.kgl")   # a live kglite graph
sonagram.export_m3u("music.kgl", "~/Music", "set.m3u8",
                    cypher="MATCH (t:Track) RETURN t.content_hash ORDER BY t.energy")
```

The multi-source graph carries a `Source` node per registered folder and a
`source_root` on every `Track`, so playlist export resolves absolute paths
without a library-root argument.

## The idea

- **sonara** supplies per-track analysis: tempo, key/Camelot, energy, valence,
  danceability, acousticness, mood, loudness, structure, and a versioned 48-dim
  similarity embedding.
- **kglite** supplies the graph engine: storage, Cypher, vector search, MCP
  exposure.
- **sonagram** owns the mapping and the schema between them: fat `Track` nodes
  with every filterable signal flat, dimension nodes (`Artist`, `Genre`, `Key`,
  `TempoBand`, `Style`, `Source`, …) for grouping and discovery, materialized
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

## Claude Code skill

`skills/sonagram-playlist/` ships the invocable skill described at the top: copy
it to `~/.claude/skills/` (filling in your library path), and "make me a work
playlist" becomes a one-liner. The skill chains `status` → `scan`/`build` (only
when stale) → graph curation per `AGENT-GUIDE.md` → named `.m3u8` export into the
central store.
