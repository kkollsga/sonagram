---
name: sonagram-playlist
description: Create playlists from the local music library via the sonagram knowledge graph — "make me a work playlist", "a party mix", "songs like X but calmer". Handles freshness check, (re)scan, graph build, Cypher curation, and .m3u8 export.
---

# sonagram-playlist

Turn a natural-language playlist request into a playable `.m3u8` (optionally a
portable folder of copies) using the sonagram music knowledge graph.

## Fixed locations (this machine)
- **Library**: `<YOUR_LIBRARY_ROOT>`
- **Graph**: `<library>/.sonagram/music.kgl` (build target; rebuild is ~1s from cache)
- **CLI**: `sonagram` if on PATH, else
  `<path to a built sonagram binary>`
- **Query runner** (full-JSON rows, no `$params`):
  `python -c 'import json,sys,kglite; print(json.dumps(kglite.load(sys.argv[1]).cypher(sys.argv[2]).to_dicts(), ensure_ascii=False, default=str))' <graph.kgl> '<cypher>'`
- **The manual**: read
  `AGENT-GUIDE.md (ships with this repo)` before querying —
  schema, cookbook, pitfalls, and the **Quality bar** (duration checks, era
  validation, style-world cohesion, final human sanity-read) are all binding.

## Workflow
1. **Freshness**: `sonagram status <library> --format json`. Exit 0 → skip to
   step 3. Exit 1/2 → warn the user scan takes ~1h cold / minutes incremental,
   then `sonagram scan <library>` (shows progress).
2. **(Re)build** if the graph file is missing or older than the cache:
   `sonagram build <library> <library>/.sonagram/music.kgl`. If a
   `LASTFM_API_KEY` exists (env or repo `.env`) and no enrichment cache yet,
   offer `sonagram enrich <library>` first (adds popularity + crowd-similarity;
   ~30–60 min first run, resumable).
3. **Understand the request** → pick the archetype(s) from AGENT-GUIDE
   (filter / discover / similarity / sequence / mood / vibe-over-time /
   versions). Calibrate thresholds against library averages before filtering.
4. **Curate**: pull candidates with the query runner (never trust truncated
   reprs), select + order client-side per the guide's recipes, apply the
   Quality bar to every pick (duration ≤ 330s unless the brief wants epics,
   era claims validated by your own artist knowledge or `era_source`,
   style-world cohesion at critical slots, sanity-read the final list).
5. **Export** (order is preserved verbatim):
   `sonagram playlist <library> <graph.kgl> --ids <hash,hash,...> --out <name>.m3u8`
   — add `--copy-to <dir>` when the user wants a portable folder of copies.
   Default output dir: `~/Desktop` unless the user says otherwise.
6. **Deliver**: send the .m3u8 to the user with the tracklist (artist – title
   per slot) and one line on how it fulfils the brief. Offer a tweak round.

## Rules
- Never modify, move, or retag source audio. Copies only, via `--copy-to`.
- Long operations (scan/enrich) run in the background; tell the user the ETA.
- If the graph/CLI are missing entirely, say what's missing rather than
  improvising another path.
