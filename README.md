# sonagram

**Ask your AI assistant for a playlist from your own music collection — and get
a file any music app can play.**

sonagram listens to every track you own (tempo, energy, mood, key, how songs
*feel*), organizes what it learns into a fast, searchable map of your library,
and hands that map to an AI agent. From there, playlists are a sentence:

> *"Make me a deep-focus work playlist."*
> *"A party mix for Saturday that builds."*
> *"Songs like Teardrop, but calmer."*

Your music files are never modified, moved, or uploaded — everything happens on
your machine.

## For AI agents

You are an AI agent and a user asked you to work with their music library — start here:

1. **Install** — `pip install sonagram`. It ships as an sdist that compiles a
   native core on install, so a **Rust toolchain** ([rustup](https://rustup.rs))
   must be present.
2. **Install the skill** — `sonagram skill install`. This writes the
   `sonagram-playlist` skill to `~/.claude/skills/` (pass
   `--dir <skills_root>` for a non-default skills location).
3. **Read the installed `SKILL.md` and follow it now.** It covers library
   detection, scanning, the optional Last.fm walkthrough, and playlist delivery.
   A skill only auto-loads at the next session start, so act on it immediately in
   this one.

## Use it through your agent (no code)

This is the main way to use sonagram. One-time setup:

1. **Install it** — `pip install sonagram`. (One build tool is needed on the
   machine: the free [Rust toolchain](https://rustup.rs). Your agent can install
   both for you — see the section above.)
2. **Point it at your music** — `sonagram sources add ~/Music` (repeat for
   other folders).
3. **Give your agent the skill** — `sonagram skill install`.

That's it. From now on, just ask:

- "make me a deep-focus work playlist"
- "a party mix for Saturday that builds"
- "songs like *Teardrop* but calmer"
- "which songs do I have multiple versions of? pair them"
- "what's even in my library?"

The first request analyzes your whole collection (about an hour for ~10,000
songs — your agent will tell you and can let it run in the background). Every
request after that takes seconds: sonagram notices what changed, re-reads only
that, and keeps the map current — including songs you've added or deleted.

Each playlist is saved with its name, your original request, and the full track
list, so you can always ask for it again later (`sonagram playlists`). The
`.m3u8` files point straight at your own music files and open in any player.

> **Optional: better picks with Last.fm.** A free Last.fm API key adds richer
> genre info, song popularity, and "fans also like" connections. Your agent can
> walk you through getting one — just ask.

## CLI (scriptable)

`pip install sonagram` also gives you the standalone `sonagram` command (the same
shared code path the agent uses, so the two can't drift). Once a source is
registered, commands need no path arguments:

```bash
sonagram sources add ~/Music     # register a library folder (repeatable)
sonagram status                  # is everything up to date? (exit 0/1/2)
sonagram scan                    # analyze new/changed files → local cache
sonagram enrich                  # optional: fold in Last.fm metadata (needs a key)
sonagram build                   # merge all sources → the central graph
sonagram playlist --ids h1,h2,h3 \
    --name "Deep Focus" --description "a calm work playlist"
sonagram playlists               # list stored playlists (newest first)
```

`sonagram config` shows where everything lives (defaults under `~/.sonagram/`)
and whether a Last.fm key is set up. Explicit-path forms
(`sonagram scan ~/Music`, `sonagram status ~/Music --format json`, …) still work
for scripting a single library without touching the config, and
`sonagram playlist ... --copy-to <dir>` produces a **portable folder** — the
tracks copied next to the playlist file, ready for a USB stick or another device.

Everything is incremental: a rescan of an unchanged library analyzes nothing and
finishes in well under a second.

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

Agents get a full manual (`AGENT-GUIDE.md`: the schema, a query cookbook, and a
curation quality bar) — it ships with the repo and powers the bundled skill.

## How it works (the short version)

- [sonara](https://github.com/kkollsga/sonara) does the listening: tempo, key,
  energy, mood, loudness, structure, and an audio "fingerprint of feel" for
  every track.
- [kglite](https://github.com/kkollsga/kglite) stores the map: a graph database
  with search, similarity, and an agent-friendly query interface.
- **sonagram** is the part in between: it decides what the map contains — every
  song with all its signals, connected to artists, genres, decades, moods,
  detected styles, and its 10 most similar tracks — and keeps the map exactly
  reproducible, byte for byte, no matter how often you rescan.

Full documentation (CLI reference, Python API, the graph's schema, and the
engineering details) lives on Read the Docs — see `docs/`.

License: MIT
