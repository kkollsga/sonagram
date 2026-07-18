# Quickstart

The short version: **install, register your music once, hand your agent the
skill, and ask for playlists.** The first request analyzes your collection
(roughly an hour per 10,000 songs, once); everything after that takes seconds,
because sonagram only re-reads what changed.

sonagram is **config-driven**: register your music folders once
(`sonagram sources add`) and every command knows where to look — the registry,
the central graph, and your saved playlists all live under `~/.sonagram/`
(`sonagram config` shows the resolved paths). Per-library analysis caches live
next to the music itself, under `<library_root>/.sonagram/`. Explicit-path
command forms also exist for scripting a single folder without any registration.

## Install

Prebuilt wheels cover macOS (Apple Silicon + Intel), Linux x86_64, and Windows
x86_64 on Python ≥ 3.9 — no toolchain needed. Other platforms build from the
sdist, which needs a **Rust toolchain** ([rustup](https://rustup.rs)):

```bash
pip install sonagram           # prebuilt wheel on common platforms
export LASTFM_API_KEY=...      # optional: enables `enrich`
```

The wheel's console script and the standalone binary run the same pure-Rust
code, so `pip install sonagram` gives the same `sonagram` command either way.

## For AI agents (the fastest path)

If you are an AI agent a user pointed at their music library, you don't touch the
flow below by hand — install the bundled skill and follow it:

```bash
pip install sonagram
sonagram skill install     # writes ~/.claude/skills/sonagram-playlist/SKILL.md
```

Then **read and follow the installed `SKILL.md` now, in this session** (a skill
only auto-loads next session). It walks library detection, scanning, the optional
Last.fm setup, graph build, and playlist delivery.

## Config-driven flow (register once, no path args)

Register your music folders once and every command fans out over them into one
central graph + playlist store — no `<library_root>` argument:

```bash
sonagram sources add ~/Music   # register a folder (repeatable); ~/.sonagram/config.json
sonagram scan                  # scan every source
sonagram build                 # merge all sources → the central ~/.sonagram/music.kgl
sonagram status                # freshness of every source + graph (graph_stale)
sonagram curate --preset focus --tracks 25 --name "Deep Focus" \
    --description "a calm work set" --format json
sonagram playlists             # list stored playlists
sonagram mcp install           # manifest + revealed music skills
```

For seed-relative or categorical requests, start from `sonagram policy
--preset <name> --format json` (or Python `sonagram.curation_policy`) and amend
the typed policy; Sonagram still owns the resulting selection and audit.

`sonagram config` shows the resolved graph + playlist-store paths. The explicit
`<library_root>`-per-command forms below still work for scripting a single
library.

## The end-to-end flow

```bash
sonagram scan   ~/Music                 # walk, hash, analyze → .sonagram/ cache
sonagram enrich ~/Music                 # optional: fold in Last.fm metadata
sonagram build  ~/Music music.kgl       # cached analysis → queryable .kgl graph
sonagram config set graph music.kgl
sonagram mcp install                    # prints the safe absolute server command
```

Each step is **incremental and read-only where it can be**: `scan` re-analyzes
only changed files, and `enrich` skips already-fetched entities. See the
[CLI reference](cli.md) for every flag.

1. **`scan`** walks `~/Music` for MP3s, content-hashes each file, and runs sonara
   analysis on anything unseen — writing per-track records under
   `~/Music/.sonagram/`. Re-running only analyzes changed files.
2. **`enrich`** (optional) fetches Last.fm metadata (popularity, folksonomy tags,
   MBIDs, similar artists/tracks, original-album mapping) into
   `~/Music/.sonagram/lastfm/`. Needs `LASTFM_API_KEY` (env or a `.env` file).
3. **`build`** maps the cached records into a kglite `.kgl` graph, auto-folding in
   the Last.fm cache when present.
4. **install MCP assets** with `sonagram mcp install`, then use its printed
   absolute `sonagram-mcp-server` command. The agent explores with graph tools
   and invokes Sonagram's typed MCP engine for every final playlist.

## Freshness probe for automation

`sonagram status <library_root>` is a **read-only** probe (it mutates nothing)
that a skill or CI step can chain before deciding whether to rescan:

```bash
sonagram status ~/Music --format json   # one stable JSON object
# exit code: 0 = fresh, 1 = needs scan, 2 = no cache
```

It compares the files on disk against the `.sonagram/` cache — counting fresh /
stale / newly-added / deleted tracks and checking each record against the
current sonara analysis schema — without hashing a file or running analysis.
Chain it as `status` → `scan`/`build` if needed → query via MCP.

## Create a playlist

Let Sonagram select, sequence, audit, explain, and store the result:

```bash
sonagram curate --preset focus --tracks 25 \
  --name "Focused Thinking" --description "focused work" --format json
```

The older materializer below is for explicit human-authored/manual exports; it
preserves caller order but does not curate it.

## Manual materialization

Once an agent (or you) has a query that answers *which tracks, in what order*,
turn it into a playable `.m3u8`. Track order is preserved verbatim, never
re-sorted:

```bash
# From a Cypher query — write an absolute-path .m3u8:
sonagram playlist ~/Music music.kgl \
    --cypher 'MATCH (t:Track) WHERE t.bpm > 120 RETURN t.content_hash ORDER BY t.energy' \
    --out set.m3u8

# --copy-to writes a SELF-CONTAINED PORTABLE folder: tracks copied as
# 'NN - Artist - Title.<ext>' next to a relative-path .m3u8. Copies only —
# source files are never moved, retagged, or modified.
sonagram playlist ~/Music music.kgl \
    --ids h1,h2,h3 --copy-to ~/Desktop/roadtrip
```

Pass `--out`, `--copy-to`, or both — at least one is required. The copy-folder's
`.m3u8` is named after `--out`'s file stem when given, else the destination
folder's own name. See the [CLI reference](cli.md#sonagram-playlist) for the
full contract and the [Python API](python-api.md#export_m3u) for the
programmatic equivalent.

## Claude Code skill

`skills/sonagram-playlist/` in the repo ships an invocable skill for Claude
Code: copy it to `~/.claude/skills/` (filling in your library path), and "make
me a work playlist" becomes a one-liner — the skill chains `status` →
`scan`/`build` (only when stale) → typed library curation/audit → stored `.m3u8`.
