# Changelog

All notable changes to sonagram are documented in this file. The graph schema
is a public API: a stored `.kgl` graph is a compatibility surface, and every
release that moves it says so under **Graph schema**.

## [0.2.1] - 2026-07-25

Library-owned playlist curation, richer graph statistics and version semantics,
and hardened analysis/cache integration with Sonara 0.3.4.

### Graph schema

Graph schema moves from **v1 to v3**. **Existing stored `.kgl` graphs must be
rebuilt after upgrading.** `Track` gains curve-derived dynamics, rhythm and
harmony features; library-relative arousal, valence, tension and recording
quality axes; music/canonical flags; Last.fm recognition and popularity; and
distinct Sonara aggression rank, evidence, component and model-provenance
properties. The new `Song` node and `VERSION_OF` edge group recording versions,
while `Source` and `Library` fingerprints now bind the analysis inputs. The
intentional golden transitions and final digests are recorded in
`GRAPH-GATE.md`.

### Added

- Deterministic, typed playlist curation with library profiles, presets and
  policies, seed-relative intent, eligibility and diversity constraints,
  sequencing/repair, independent audit and explanation, and provenance-aware
  playlist storage.
- Rust, CLI and Python curation APIs plus a typed MCP front end with modular,
  live-revealed music skills for profiling, policy resolution, curation,
  playlist auditing, storage and song-version inspection.
- Statistics-driven music features and calibrated composite mood/quality axes,
  with non-music excluded from calibration.
- Song/version clustering with deterministic canonical selection, Last.fm
  release recognition, and similarity-confirmed repair for explicit unknown-
  artist tags.

### Changed

- Sonara 0.3.4 supplies sample-rate-stable `aggression-rank-v3-sr22050`
  analysis. Aggression remains distinct from legacy mood, supports valid null
  abstention, and is used only by explicit fail-closed curation intent.
- Analysis freshness is schema/model aware. Compatible old records migrate
  without decoding; stale aggression-model records reanalyse from audio.
- KGLite integration now uses deterministic graph persistence, typed music MCP
  registration and packaged skill revelation through the bundled server.
- Agent and public guidance now routes final playlist selection, ordering and
  audit through Sonagram instead of private agent heuristics.

### Fixed

- Graph builds use the scan index as authority, coalesce duplicate content
  hashes before analysis, preserve usable enrichment records, and fingerprint
  analysis/model provenance so stale graphs are detected reliably.
- Canonical selection prefers recognized releases before recording quality;
  artist aliases use MusicBrainz identity and unknown-artist regrouping cannot
  cascade or absorb known covers.
- The packaged MCP server resolves correctly from virtual environments, with
  deterministic managed assets and fail-closed mutation behavior.

### Performance

- Sonara 0.3.4 reduces bounded 21-track cold aggression overhead to **5.38%**
  (gate: at most 10%); an unchanged rescan analyses zero tracks and reuses all
  cached records. The private full library was not part of the release gate.

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
