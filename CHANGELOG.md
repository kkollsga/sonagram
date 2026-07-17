# Changelog

All notable changes to sonagram are documented in this file. The graph schema
is a public API: a stored `.kgl` graph is a compatibility surface, and every
release that moves it says so under **Graph schema**.

## [0.2.0] - 2026-07-17

Streaming scan pipeline (P20). Graph schema unchanged (v1); the golden-graph
gate is untouched.

### Added

- **Streaming, resumable scans** — analysis results now persist one record at
  a time the moment each file completes (previously the whole batch was held
  in memory and flushed only at the end: a killed 33k-file cold scan lost
  hours of work). The index is checkpointed every 30 s, so an interrupted scan
  resumes from roughly where it stopped via the existing stat/hash fast paths.
- **Hash/analyze overlap** — the decision loop (stat, hash, queue) feeds a
  worker pool that starts analyzing the first unseen file immediately, instead
  of hashing the entire library first while every core idles. On a large cold
  scan the hash pass now costs no separate wall-clock.
- **On-disk progress snapshots** — every scan writes
  `<lib>/.sonagram/scan_progress.json` and every enrichment
  `<lib>/.sonagram/enrich_progress.json` (atomic, throttled to 1/s), so
  progress is observable from ANY entry point — CLI, Python, or an outside
  probe — regardless of how stdout is wired. New `sonagram progress
  [<root>] [--format json]` renders them with derived %, rate, ETA, and a
  staleness marker; `sonagram status` inlines live snapshots.
- **Parallel scan + enrichment** — `sonagram scan` now runs Last.fm
  enrichment concurrently with analysis by default (scan is CPU-bound,
  enrichment network-bound; `--no-enrich` opts out, a missing API key
  degrades to a plain scan). New `sonagram.scan_and_enrich()` in the Python
  API. The enrich loop re-passes the growing analysis cache and a final pass
  catches the tail; per-entity fetches were already incremental and cached.
- CLI scan/enrich stderr progress now prints roughly every 1 % (the old
  boundary-only rule left multi-hour analyze phases stuck on `Analyze 0/N`).

### Changed

- `scan::Analyzer` (Rust API) is now a per-file seam: `analyze_one(&self,
  &AnalyzeRequest) -> Result<AnalysisRecord>` replaces the batch `analyze`
  method — the scanner owns fan-out and persistence. External implementations
  must migrate (trivially); hence the minor version bump.

## [0.1.0] - 2026-07-17

Initial release.

### Graph schema

Graph schema v1, built on sonara 0.2.4 analysis (analysis schema v3,
similarity v2). The graph is deterministic — the same library builds
byte-identically, guarded by a golden-digest gate (`GRAPH-GATE.md`). Nodes:
`Track` (all audio + tag signals flat, including `bpm_confidence`,
`original_year`/`era_source`), `Artist`, `Album`, `Genre`, `Key`, `TempoBand`,
`EnergyLevel`, `Decade`, `Style` (detected similarity communities, adaptive
threshold), `Source`, `Library`. Edges: dimension membership, top-10
`SIMILAR_TO` audio-similarity, `CAMELOT_ADJACENT` harmonic wheel, and — with
Last.fm enrichment — folksonomy `IN_GENRE` plus weighted `CROWD_SIMILAR`.

### Added

- **Library scan** — incremental, content-addressed (ID3-stripped audio
  hashing: retagged or moved files keep their identity; duplicate files share
  one analysis), powered by [sonara](https://github.com/kkollsga/sonara)
  ≥ 0.2.4. A no-op rescan of 9k+ files completes in ~0.5 s with zero analyses;
  stale records (older sonara schema) re-analyze automatically.
- **Knowledge graph build** — cached analysis →
  [kglite](https://github.com/kkollsga/kglite) `.kgl` graph (~1 s for a
  10k-file library), servable to AI agents via `kglite-mcp-server`.
- **Last.fm enrichment** (optional) — popularity, folksonomy genres, MBIDs,
  original-album mapping, and human co-listening similarity, cached as JSON
  and folded into the graph. `LASTFM_API_KEY` via env or `.env` (multi-tier
  resolution incl. `~/.sonagram/.env`).
- **Playlist export** — Cypher result or track-id list → `.m3u8` with absolute
  paths (order preserved verbatim); optional `--copy-to` portable folder
  (copies only — sources are never modified); central playlist store with
  metadata (`--name`/`--description`, `sonagram playlists` to retrieve).
- **Config-driven CLI** — `sources add`/`config`/path-less
  `scan|enrich|build|status`; multi-source builds with cross-source dedup;
  `status` freshness probe (stat-level, record-schema-level, and
  graph-fingerprint-level; exit 0/1/2) for automation.
- **Agent-first UX** — bundled `sonagram-playlist` Claude skill
  (`sonagram skill install`, personalized at install), `AGENT-GUIDE.md`
  (schema reference + validated query cookbook + curation quality bar), and a
  README bootstrap section for cold-starting agents. Validated end-to-end by
  blank-agent trials.
- **Python API** — `scan`, `build` (returns a live `kglite.KnowledgeGraph`),
  `scan_and_build`, `enrich`, `export_m3u`; the pip console script shares the
  Rust CLI code path.
