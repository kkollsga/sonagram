---
name: sonagram-playlist
description: Create playlists from the local music library via the sonagram knowledge graph — "make me a work playlist", "a party mix", "songs like X but calmer". Handles freshness check, (re)scan, graph build, Cypher curation, and .m3u8 export.
---

# sonagram-playlist

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
- Optional but recommended: Last.fm enrichment (next section).

## Fixed locations (this machine)
- **CLI**: `sonagram` if on PATH, else `<path to a built sonagram binary>`.
- **Graph**: the configured central graph — run `sonagram config` for the path
  (default `~/.sonagram/music.kgl`). `sonagram build` rebuilds it from every
  configured source (~1s from cache).
- **Playlist store**: `sonagram config` → `playlists_dir` (default
  `~/.sonagram/playlists/`), holding a `<slug>.m3u8` + `<slug>.meta.json` per
  saved playlist.
- **Query runner** (full-JSON rows, no `$params`):
  `<PYTHON> -c 'import json,sys,kglite; print(json.dumps(kglite.load(sys.argv[1]).cypher(sys.argv[2]).to_dicts(), ensure_ascii=False, default=str))' <graph.kgl> '<cypher>'`
  (`<PYTHON>` is filled at install time with the absolute interpreter path —
  never use a bare `python`, shell aliases shadow it)
- **The manual**: read `AGENT-GUIDE.md (ships with this repo)` before querying —
  schema, cookbook, pitfalls, and the **Quality bar** (duration checks, era
  validation, style-world cohesion, final human sanity-read) are all binding.

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
3. **Understand the request** → pick the archetype(s) from AGENT-GUIDE
   (filter / discover / similarity / sequence / mood / vibe-over-time /
   versions). Calibrate thresholds against library averages before filtering.
4. **Curate**: pull candidates with the query runner against the configured graph
   (never trust truncated reprs), select + order client-side per the guide's
   recipes. Candidate queries default to `t.is_music AND t.is_canonical`; mood
   queries must also require every used mood axis `IS NOT NULL`. Apply the
   Quality bar to every pick (prefer `quality_tier <> 'low'`, duration ≤ 330s
   unless the brief wants epics, era claims validated by your own artist
   knowledge or `era_source`, style-world cohesion at critical slots, sanity-read
   the final list). For a familiar/mainstream brief, optionally order by
   `has_lastfm_match DESC, popularity DESC, recording_quality DESC`.
5. **Export + store** (order is preserved verbatim):
   `sonagram playlist --ids <hash,hash,...> --name "<request-derived name>" --description "<the user's ask>"`
   — writes `<slug>.m3u8` (absolute paths, openable in any music app) + a
   `<slug>.meta.json` into the central store. Add `--copy-to <dir>` when the user
   wants a portable folder of copies.
6. **Deliver**: give the user the stored `.m3u8` path with the tracklist
   (artist – title per slot) and one line on how it fulfils the brief. Mention
   `sonagram playlists` (and `sonagram playlists show <slug>`) to retrieve it
   later. Offer a tweak round.

## Rules
- Never modify, move, or retag source audio. Copies only, via `--copy-to`.
- Long operations (scan/enrich) run in the background; tell the user the ETA.
- If the graph/CLI are missing entirely, say what's missing rather than
  improvising another path.
- For mood playlists, use the music-only `arousal_index`/`tension_index`
  percentiles plus curve features; treat nullable `valence_index` as a ranking
  hint. Non-music has null mood axes and was excluded from their calibration.
- For versions, start from `Track-[:VERSION_OF]->Song` and
  `Song.canonical_hash`. Primary grouping is artist + normalized title. Only an
  explicit junk artist tag can attach to one unambiguous same-title non-junk Song,
  and only with a `SIMILAR_TO` edge in either direction to an original member;
  known-artist covers never move and repaired tracks never seed a cascade.
- Canonical choice within a Song is `has_lastfm_match` first, then
  `recording_quality` (null lowest), then ascending `content_hash`.
  `lastfm_listeners`/`lastfm_playcount` are song-level counts and `popularity`
  is their listener percentile within the library (equal counts tie). Use these
  to prefer familiar songs, never as proof that one recognized release is the
  master rather than an alternate take.
