# Changelog

All notable changes to sonagram are documented in this file. The graph schema
is a public API: a stored `.kgl` graph is a compatibility surface, and every
release that moves it says so under **Graph schema**.

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
