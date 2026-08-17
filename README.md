# sonagram

**Ask your AI assistant for a playlist from your own music collection — and get
a file any music app can play.**

sonagram listens to every track you own (tempo, energy, mood, key, how songs
*feel*), organizes what it learns into a fast, searchable map of your library,
and lets an AI agent translate your sentence into a typed request. Sonagram's
deterministic library engine—not the agent—then selects, orders, audits, and
explains the playlist:

> *"Make me a deep-focus work playlist."*
> *"A party mix for Saturday that builds."*
> *"Songs like Teardrop, but calmer."*

Your music files are never modified, moved, or uploaded — everything happens on
your machine.

## For AI agents

You are an AI agent and a user asked you to work with their music library — start here:

1. **Install** — `pip install sonagram`. Prebuilt wheels cover macOS
   (Apple Silicon + Intel), Linux x86_64, and Windows x86_64 on Python ≥ 3.9 —
   no toolchain needed. Only on other platforms does pip fall back to the
   sdist, which compiles the native core and needs a Rust toolchain
   ([rustup](https://rustup.rs)).
2. **Install the skill** — `sonagram skill install`. This writes the
   `sonagram-playlist` skill to `~/.claude/skills/` (pass
   `--dir <skills_root>` for a non-default skills location).
3. **Read the installed `SKILL.md` and follow it now.** It covers library
   detection, scanning, the optional Last.fm walkthrough, and playlist delivery.
   A skill only auto-loads at the next session start, so act on it immediately in
   this one.

## Use it through your agent (no code)

This is the main way to use sonagram. One-time setup:

1. **Install it** — `pip install sonagram`. That's it on Mac, Windows, and
   regular Linux — prebuilt packages, nothing to compile. (On unusual
   platforms pip builds from source, which needs the free
   [Rust toolchain](https://rustup.rs) — your agent can sort that out.)
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
sonagram profile --format json   # curation-relevant coverage/distributions
sonagram curate --preset focus --tracks 25 \
    --name "Deep Focus" --description "a calm work playlist" --format json
sonagram playlists               # list stored playlists (newest first)
sonagram mcp install             # native kglite manifest + revealed music skills
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

Install Sonagram's safe kglite manifest/revealed skills, then use the absolute
launch command it prints—or drive the same library contract from Python:

```bash
sonagram mcp install
# RUN: '/absolute/path/sonagram-mcp-server' --graph '/.../music.kgl'
```

The thin Sonagram frontend embeds KGLite 0.16.3's server and registers typed
profile/policy/curate/audit/explain/store tools against its live graph. KGLite
still owns MCP, Cypher, graph lifecycle, and generic tools; Sonagram owns only
the music-domain handlers.

```python
import sonagram
sonagram.scan("~/Music")
g = sonagram.build("~/Music", out_path="music.kgl")   # a live kglite graph
brief = {"preset": "focus", "target_tracks": 25,
         "target_duration_sec": None, "seed_ids": [],
         "seed_role": "pinned", "unsupported_intents": []}
result = sonagram.curate_playlist("music.kgl", brief)
assert result["exportable"] and result["audit"]["passed"]
```

Agents get a full manual (`AGENT-GUIDE.md`: the schema, a query cookbook, and a
typed curation contract) plus live-gated kglite skills. Both route final
selection/order/audit through the library rather than agent-authored heuristics.

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
