---
name: sonagram-playlist
description: Create and evaluate playlists through Sonagram's deterministic library curation engine — "make me a work playlist", "a party mix", "songs like X but calmer". The agent translates intent into a typed preset/brief; Sonagram owns selection, ordering, audit, explanation, and storage.
---

# sonagram-playlist

<!-- sonagram-curation-contract:v1 -->

Turn a natural-language playlist request into a playable `.m3u8` (optionally a
portable folder of copies) using the sonagram music knowledge graph. sonagram is
**config-driven**: you register your music folders once, then every command fans
out over them into one central graph + one playlist store.

## Library detection (before First-run setup)
Find the music library before registering anything — **never register a guess
silently**:
1. **Memory / context first.** Check your own memory and this conversation for a
   library path the user already gave (or you used before). If you have one, skip
   to step 3 and confirm it.
2. **Probe the OS-standard locations** and count MP3s at each, so you can show the
   user real evidence (a fast stat count, not a full analysis):
   - macOS: `~/Music/Music/Media.localized/Music` (the Music.app library) first,
     then `~/Music`
   - Linux: `~/Music`
   - Windows: `~\Music`
   Count fast, e.g. `find "$HOME/Music" -iname '*.mp3' | head -20000 | wc -l`
   (adjust the path per OS; try the macOS Music.app path first on a Mac).
3. **Present findings and ASK.** Show each candidate with its MP3 count and ask
   the user to confirm the right one (or paste another path). Only after they
   confirm do you run `sonagram sources add <confirmed_path>`. Never register a
   guessed path.

## First-run setup (once)
- **Register the library**: `sonagram sources add <YOUR_LIBRARY_ROOT>` (repeat for
  each folder). Confirm with `sonagram sources list`.
- Everything else is defaults: `sonagram config` shows the resolved central graph
  (`~/.sonagram/music.kgl`) and playlist store (`~/.sonagram/playlists/`).
- Run `sonagram mcp install` after the first graph build to install kglite's
  native manifest and live-gated music skills beside the graph.
- Optional but recommended: Last.fm enrichment (next section).

## Fixed locations (this machine)
- **CLI**: `sonagram` if on PATH, else `<path to a built sonagram binary>`.
- **Graph**: the configured central graph — run `sonagram config` for the path
  (default `~/.sonagram/music.kgl`). `sonagram build` rebuilds it from every
  configured source (~1s from cache).
- **Playlist store**: `sonagram config` → `playlists_dir` (default
  `~/.sonagram/playlists/`), holding a `<slug>.m3u8` + `<slug>.meta.json` per
  saved playlist.
- **Exploration-only query runner** (full-JSON rows, no `$params`):
  `<PYTHON> -c 'import json,sys,kglite; print(json.dumps(kglite.load(sys.argv[1]).cypher(sys.argv[2]).to_dicts(), ensure_ascii=False, default=str))' <graph.kgl> '<cypher>'`
  (`<PYTHON>` is filled at install time with the absolute interpreter path —
  never use a bare `python`, shell aliases shadow it)
- **The manual**: `AGENT-GUIDE.md` (ships with the repo) documents the schema,
  query cookbook, pitfalls, and typed curation contract. Graph queries explain;
  the library's policy/audit decides whether a final playlist is deliverable.

## First-run Last.fm setup (optional, recommended)
Run this once when `sonagram config` shows `lastfm_key: not configured`. Last.fm
enrichment adds better genres, popularity, and crowd-similarity that noticeably
sharpen playlist curation.

1. Explain the benefit (one line): richer genres + popularity + "fans also like"
   similarity for better picks.
2. Walk the user through getting a **free** key:
   - Create or log into a free Last.fm account at `https://www.last.fm`.
   - Go to `https://www.last.fm/api/account/create`.
   - Fill **Application name** (e.g. `sonagram`). Contact email auto-fills;
     callback URL and homepage can stay **empty**.
   - Submit — the page shows an **API key** (32-char hex). They do **not** need
     the shared secret.
3. Ask the user to **paste the API key in chat**, then write it to
   `~/.sonagram/.env` as `LASTFM_API_KEY=<pasted>` and `chmod 600` that file.
4. **Relay this framing to the user**: pasting secrets in chat is normally unsafe
   practice; it's acceptable *here* because a Last.fm API key is free, instantly
   revocable, and grants no account access — and it's stored only in a local file
   that never leaves the machine except in requests to Last.fm. For any other kind
   of credential, do **not** use this flow.
5. Then run `sonagram enrich` (all sources; ~30–60 min first run, resumable, safe
   to background — tell the user the ETA). Note: a plain `sonagram scan` already
   runs enrichment in parallel when a key is configured, so a separate `enrich`
   is only needed to backfill an already-scanned library.

## Workflow
1. **Freshness**: `sonagram status --format json` (probes every configured
   source; exit = worst-of). Exit 0 → skip to step 3. Exit 1/2 → warn the user a
   cold scan is hours-scale on a big library / incremental is minutes, then
   `sonagram scan` (all sources; enriches from Last.fm in parallel when a key is
   configured — `--no-enrich` opts out). Scans stream results to disk: a killed
   scan resumes where it stopped, and `sonagram progress [--format json]` shows
   live per-source %, rate, and ETA from any shell while it runs.
2. **(Re)build**: `sonagram build` (multi-source → the configured graph). If the
   scan ran without a Last.fm key and one is configured later, run
   `sonagram enrich` before building (see above).
3. **Translate intent, do not curate privately**: choose one preset—`focus`,
   `party`, `workout`, `chill`, `discovery`, or `general`—plus requested track
   count/duration and explicit seeds. Plain seeds are pinned. For seed-relative
   requests use typed `seed_role` (`reference` or `pinned_and_reference`),
   `seed_similarity`, and `relative_*` targets (`relative_*_margin` requires a
   minimum change); use eligibility include/exclude
   lists for artists, genres, styles, decades, and year bounds. Resolve the full
   preset with `sonagram policy --preset <preset> --format json` or Python
   `sonagram.curation_policy()`. Use `sonagram profile --format json` only
   when an unusual brief genuinely needs calibration. Cypher is for exploring
   the library, never for selecting or ordering a final playlist.
4. **Curate + store through the library**:
   `sonagram curate --preset <preset> --tracks <N> --name "<request-derived name>" --description "<the user's ask>" --format json`.
   Sonagram enforces music/canonical eligibility, Track/Song/artist/album
   diversity, sequencing, bounded repair, independent audit, and explanation.
   Never replace or reorder the returned IDs. A result is ready only when
   `exportable` and `audit.passed` are both true; a named successful result
   writes the paired `.m3u8` + `.meta.json` to the central store.
5. **Evaluate honestly**: read audit metrics/issues and explanations. If the
   result is poor despite passing, describe the concrete defect and treat it as
   a Sonagram library issue. Do not hide it with an agent-only heuristic.
6. **Deliver**: give the user the stored `.m3u8` path with the tracklist
   (artist – title per slot) and one line on how it fulfils the brief. Mention
   `sonagram playlists` (and `sonagram playlists show <slug>`) to retrieve it
   later. Offer a tweak round.

## Rules
- Never modify, move, or retag source audio. Copies only, via `--copy-to`.
- Long operations (scan/enrich) run in the background; tell the user the ETA.
- If the graph/CLI are missing entirely, say what's missing rather than
  improvising another path.
- The agent owns intent translation only. Sonagram owns candidate selection,
  ordering, repair, audit, and store provenance. Never hand-author final IDs.
- For mood playlists, the resolved preset/policy owns the music-only
  `arousal_index`/`tension_index` targets and treats nullable `valence_index`
  conservatively. Use raw axes/curve features to explain or diagnose a result,
  not to hand-replace its IDs.
- Unknown typed fields are rejected. Put constraints the graph cannot enforce
  (such as lyrical subject matter without lyrics) in
  `brief.unsupported_intents`; a structured non-exportable result is required.
  Never approximate unsupported intent with private selection.
- For versions, start from `Track-[:VERSION_OF]->Song` and
  `Song.canonical_hash`. Primary grouping is artist + normalized title. Only an
  explicit junk artist tag can attach to one unambiguous same-title non-junk Song,
  and only with a `SIMILAR_TO` edge in either direction to an original member;
  known-artist covers never move and repaired tracks never seed a cascade.
- Canonical choice within a Song is `has_lastfm_match` first, then
  `recording_quality` (null lowest), then ascending `content_hash`.
  `lastfm_listeners`/`lastfm_playcount` are song-level counts and `popularity`
  is their listener percentile within the library (equal counts tie). The
  library policy uses these as familiarity proxies, never as proof that one
  recognized release is the master rather than an alternate take.
