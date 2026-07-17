# Changelog

All notable changes to sonagram are documented in this file. The graph schema
is a public API: a stored `.kgl` graph is a compatibility surface, and every
release that moves it says so under **Graph schema**.

## [Unreleased]

Statistics-driven mood + quality layer plus song/version clustering (P21 Stages
A, B, C). Pure mapper change — no scan, analysis, or CLI/status behaviour is
touched; the graph is built from the curves already cached on disk. Graph schema
moves to **v2**, so both golden digests were regenerated in this change (plain
`9977d14b141a`, enriched `40709837f90c`).

### Graph schema

Graph schema **v2**. `Track` gains fifteen new properties, all null when their
source curve/scalar is absent (the existing null-property policy), except
`is_canonical` which is non-null on every `Track`. A new `Song` node type groups
version families:

- **Stage A — curve-derived flat features** (computed per track from the cached
  `loudness_curve` / `energy_curve` / `tempo_curve` / `chord_events` /
  `segments`): `macro_dynamics` (loudness-curve population stdev),
  `energy_arc_range` (`(p95−p5)/mean` of the energy curve),
  `energy_builds_per_min` (maximal ≥8-sample rising runs per minute),
  `flow_smoothness` (`1 − mean|Δ|/mean`, clamped), `chord_vocab` (distinct chord
  labels), `chord_entropy` (Shannon bits of the chord-label distribution),
  `chord_churn` (chord events per minute), `tempo_steadiness` (`1 − cv`,
  clamped), `seg_density` (segments per minute).
- **Stage B — percentile-calibrated composite axes** (library-relative, computed
  after all raw features exist; each is a percentile rank in `[0,1]` of a
  signed-z-score composite, tie-broken by `content_hash`): `arousal_index`,
  `valence_index` (documented weak prior — literature R² 0.12–0.28),
  `tension_index`, `recording_quality`, plus `quality_tier` (`high`/`mid`/`low`
  by percentile thirds of `recording_quality`).
- **Stage C — song/version layer.** Recordings that share a version key
  `(artist_id, normalized_title)` are grouped: every group of two or more gets a
  `Song` node (id `"<artist_id>|<normalized_title>"`, properties `title`,
  `artist`, `n_versions`, `canonical_hash`) with a `Track -[:VERSION_OF]-> Song`
  edge from each member. Singletons get no `Song`. Every `Track` gains a non-null
  `is_canonical` bool — `true` unless the track is a non-best member of a version
  group; within a group the best member is the highest `recording_quality` (nulls
  lowest), tie-broken by `content_hash` — so `WHERE t.is_canonical` skips
  duplicate/inferior takes. Grouping is title+artist only in this iteration
  (embeddings/duration deferred; the grouping seam is structured for a later
  splitter).

### Added

- `graph/features.rs` — pure statistics module for the P21 Stage-A curve
  features and the Stage-B two-pass z-score + percentile composite axes, with
  unit tests for every feature (including empty/constant/too-short curves) and a
  determinism test for the percentile pass.
- `graph/song.rs` — the P21 Stage-C version-grouping + canonical-selection
  module, plus `normalize::normalized_title` (lowercase; strip
  bracketed/parenthesized and trailing edition markers; fold Unicode
  apostrophes). Unit-tested for title normalization, canonical selection (null
  `recording_quality`, tie-break, singleton exclusion) and order-independence;
  `tests/song_versions.rs` is the integration gate over a synthetic version set.

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
