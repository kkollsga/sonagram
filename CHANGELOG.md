# Changelog

All notable changes to sonagram are documented in this file. The graph schema
is a public API: a stored `.kgl` graph is a compatibility surface, and every
release that moves it says so under **Graph schema**.

## [0.2.13] - 2026-08-31

A maintenance release: the embedded graph engine moves to KGLite 0.16.18 (one
engine release). How your library is analyzed and stored does not change, and
nothing sonagram builds moves — this release makes the agent server
(`sonagram mcp`) sturdier at startup.

### Graph schema

No change. Graph schema stays at **v3**, the `.kgl` file format is unchanged,
and both canonical digests are byte-identical to 0.2.12 — **stored `.kgl`
graphs do not need rebuilding.**

### Fixed

- Embedded KGLite: 0.16.17 → 0.16.18. The agent server now starts even when
  one of its optional extras can't: a data-export listener that fails to
  claim its network port, or a configured source folder that has moved, is
  reported as a warning in the startup summary instead of stopping the whole
  server. The data-export listener also picks a free port automatically again
  when none is configured, so several agent apps can share one machine
  without colliding.

## [0.2.12] - 2026-08-31

A maintenance release: the embedded graph engine moves to KGLite 0.16.17 (two
engine releases). How your library is analyzed and stored does not change, and
nothing sonagram builds moves — this release keeps the embedded engine current
and slims what gets compiled alongside it.

### Graph schema

No change. Graph schema stays at **v3**, the `.kgl` file format is unchanged,
and both canonical digests are byte-identical to 0.2.11 — **stored `.kgl`
graphs do not need rebuilding.**

### Changed

- Embedded KGLite: 0.16.15 → 0.16.17. The engine dropped a diagnostics field
  that never carried real information, taught its graph-file exports to label
  nodes the way external graph viewers expect, and trimmed unused pieces of a
  mapping dependency from the build — none of which changes how sonagram
  builds or queries your graph.
- Query cancellation inside the engine got more responsive (a long-running
  pattern search now notices an interrupt in milliseconds rather than
  seconds). Sonagram itself sets no query deadlines, so this reaches only
  agents querying a live graph with timeouts of their own.
- For developers building sonagram from source: dependency debug info is now
  built as line-tables-only, cutting the debug build tree roughly a fifth
  with no effect on the shipped package.

## [0.2.11] - 2026-08-30

A maintenance release: the embedded graph engine moves to KGLite 0.16.15 (two
engine releases). How your library is analyzed and stored does not change, and
neither does anything sonagram builds — this release exists to pick up two
engine fixes that quietly repair saved graph files.

### Graph schema

No change. Graph schema stays at **v3**, the `.kgl` file format is unchanged,
and both canonical digests are byte-identical to 0.2.10 — **stored `.kgl`
graphs do not need rebuilding.**

### Changed

- Embedded KGLite: 0.16.13 → 0.16.15. The new engine features (load-time
  options, memory estimation for large files, a result-row cap) are all
  opt-in surfaces sonagram does not switch on; loading a saved graph is also
  5–10% faster.

### Fixed

Two engine repairs that reach saved `.kgl` files automatically — no action
needed:

- A saved graph could report **zero connections per relationship type** after
  being reopened (affecting overview summaries and query planning, not query
  answers). Reopening such a file now recounts and repairs it.
- Saving the same graph twice now produces **byte-identical files** again —
  the same same-library-twice determinism sonagram promises for its own
  builds, extended to the engine's file writer.

## [0.2.10] - 2026-08-27

A maintenance release: the embedded graph engine moves to KGLite 0.16.13. How
your library is analyzed and stored does not change, and neither does anything
sonagram builds — this release exists so agents working on a live graph get
another round of engine fixes.

### Graph schema

No change. Graph schema stays at **v3**, the `.kgl` file format is unchanged,
and both canonical digests are byte-identical to 0.2.9 — **stored `.kgl` graphs
do not need rebuilding.**

### Changed

- Embedded KGLite: 0.16.12 → 0.16.13. Most of this release refines the engine's
  optional "ontology" layer for declaring how node types relate — machinery
  sonagram does not switch on. Sonagram builds your graph the same way it did
  before, and the similarity search behind playlists is unchanged.

### Fixed

Three fixes for agents querying or editing a live graph, all of which only
applied once a search index had been created on it. Sonagram creates none, so
nothing it built was ever affected and no rebuild is needed:

- Creating an index on a track or artist **name** could silently drop rows from
  later searches for that name — the index recorded only names stored as a
  property, while the search also matched names carried as the node's title. A
  "create if missing" write could likewise duplicate a node it had failed to
  find. Such indexes are no longer trusted for these lookups.
- A search across *all* node types filtered by a property (rather than one
  named type) could drop matches, because results were checked against the
  first match's type only.
- The same flaw in the non-query ("fluent") search interface could return only
  the first matching type's results when several types matched.

## [0.2.9] - 2026-08-26

A maintenance release: the embedded graph engine moves to KGLite 0.16.12
(absorbing 0.16.10 and 0.16.11). How your library is analyzed and stored does
not change, and neither does anything sonagram builds — this release is here so
agents working on a live graph get three releases of engine fixes.

### Graph schema

No change. Graph schema stays at **v3**, the `.kgl` file format is unchanged,
and both canonical digests are byte-identical to 0.2.8 — **stored `.kgl` graphs
do not need rebuilding.**

### Changed

- Embedded KGLite: 0.16.9 → 0.16.12. The bulk of what these three releases add
  is optional machinery sonagram does not switch on — a keyword (full-text)
  search index, a way to rank results by keyword and meaning at once, and an
  "ontology" layer for declaring how node types relate to each other. Sonagram
  builds your graph the same way it did before, and the similarity search
  behind playlists is unchanged.

### Fixed

Three fixes that matter to agents querying or editing a live graph. None of
them affect how sonagram builds the graph, and none require a rebuild:

- The engine's automatic housekeeping pass (which tidies up storage on its own,
  with no explicit request) could corrupt extra labels an agent had attached to
  nodes — afterwards, searching by such a label could return empty phantom
  rows, over-count, or miss nodes that still had the label. Fixed.
- Asking for an index on a label that only ever existed as an extra label used
  to appear to succeed while building something no search would ever consult.
  It now says so plainly.
- On graphs using extra labels, two common query shapes (aggregating over
  connections, and joining by location) had been falling back to a much slower
  path for *every* query as soon as any extra label existed anywhere. The
  engine now makes that decision per query — upstream measured the old
  behaviour at 71x and 33x slower on the affected shapes.

## [0.2.8] - 2026-08-24

A maintenance release: the embedded graph engine moves to KGLite 0.16.9
(absorbing 0.16.8). How your library is analyzed and stored does not change;
saved graphs load noticeably faster again (details below).

### Graph schema

No change. Graph schema stays at **v3**, the `.kgl` file format is unchanged,
and both canonical digests are byte-identical to 0.2.7 — **stored `.kgl`
graphs do not need rebuilding.**

### Changed

- Embedded KGLite: 0.16.7 → 0.16.9. The headline is a loading-speed fix — and
  a correction to this changelog: the 0.2.6 entry said the corruption
  checksums added then left loading unaffected. That upstream claim turned
  out to be wrong — verifying the checksums roughly doubled load time for
  graphs saved by 0.2.6 or later. The engine now uses the CPU's built-in
  checksum instructions, bringing the cost of full verification down to about
  5% while keeping the same protection. Saved files are unchanged either way;
  nothing needs re-saving.
- Several query-correctness fixes for agents working on a live graph:
  counting distinct relationships no longer undercounts when two tracks are
  linked more than once, a filtered-and-limited aggregation no longer returns
  fewer rows than asked, and an edge-property update made while other results
  are held open is no longer invisible to later filters. None of these
  affected how sonagram builds the graph itself.

## [0.2.7] - 2026-08-23

A maintenance release: the embedded graph engine moves to KGLite 0.16.7. How
your library is analyzed and stored does not change; the engine is more honest
when something is wrong (details below).

### Graph schema

No change. Graph schema stays at **v3**, the `.kgl` file format is unchanged,
and both canonical digests are byte-identical to 0.2.6 — **stored `.kgl`
graphs do not need rebuilding.**

### Changed

- Embedded KGLite: 0.16.6 → 0.16.7. The theme of the release is that a
  similarity search that could not possibly match anything — asking for a
  vector column that doesn't exist, for example — now raises a clear
  did-you-mean error instead of silently answering "no similar tracks", which
  was indistinguishable from a genuine empty result. Sonagram's own graph
  build is unaffected (it always creates the similarity store before
  searching it); the change protects agents querying a served graph.
- Deleting a track from a live graph now removes its similarity vector with
  it. Previously a track added *after* a deletion could inherit the deleted
  track's vector and show up as a perfect-score match for queries meant for
  the old track. Sonagram rebuilds the graph from scratch on each build, so
  this could only surface on a graph an agent was editing live — but there it
  was a real phantom-match bug, now fixed.
- Whole-graph semantic search (no type filter) now uses the fast vector index
  on graphs with several node types — upstream measures 6–13× — where it
  previously fell back to a slow full scan.

## [0.2.6] - 2026-08-22

A maintenance release: the embedded graph engine moves to KGLite 0.16.6. How
your library is analyzed and stored does not change; one class of similarity
query now answers more completely (details below).

### Graph schema

No change. Graph schema stays at **v3** and both canonical digests are
byte-identical to 0.2.5 — **stored `.kgl` graphs do not need rebuilding.**

The bytes of a *newly saved* `.kgl` do change: the engine now writes a
checksum for each section of the file, so corruption (a truncated copy, a bad
disk) is caught at load instead of surfacing as wrong answers later. The
format change is additive in both directions — existing graphs load unchanged,
and older sonagram versions can read files written by this one. Saving is
measured ~11–15% slower upstream; loading is unaffected.

### Changed

- Embedded KGLite: 0.16.5 → 0.16.6. The headline is a correctness fix for
  multi-step similarity queries: chaining several `SIMILAR_TO` hops (as the
  agent guide's "reach beyond the top-10" recipe does) previously dropped some
  reachable tracks when similarity links form a loop — including the seed
  track itself, which by Cypher's rules *is* reachable through a mutual pair.
  Such queries now answer completely; seeing the seed in its own extended
  neighbourhood is correct, not a bug. A new always-on test pins these answers
  over a known loop so a future engine change cannot move them silently.
- The engine is stricter about malformed queries: a typo like a `$parameter`
  that was never given a value, or `count()` with no argument, now raises a
  clear error where it previously answered empty. Agent-facing docs already
  told agents to write every value inline, so recipes are unaffected.
- Python `.cypher()` calls are faster on repeated queries (the engine wheel
  now caches parsed queries — upstream measures 25–36% per call).

## [0.2.5] - 2026-08-20

A maintenance release: the embedded graph engine moves to KGLite 0.16.5, which
brings a measurable speed-up to the queries that return many tracks at once.
Nothing about how your library is analyzed, stored, or queried changes.

### Graph schema

No change. Graph schema stays at **v3** and both canonical digests are
byte-identical to 0.2.4 — **stored `.kgl` graphs do not need rebuilding.**

One byte inside the file does move: a `.kgl` records which engine version wrote
it, so a graph rebuilt under 0.2.5 differs from the same graph built under 0.2.4
by that stamp and nothing else — same size, same contents, and either version
reads either file. It only matters if you content-hash a `.kgl` to detect
changes: mask the `library_version` field, or every engine upgrade will look
like a data change.

### Changed

- Embedded KGLite: 0.16.3 → 0.16.5. Queries that return or sort many track
  nodes at once are faster — measured **11% faster wall-clock** (and 26% less
  CPU) on an `ORDER BY` over 5,000 tracks from a 32,890-track library, because
  the engine now shares track properties between result rows instead of copying
  them. Building a graph, rescanning an unchanged library, and ordinary
  filtered lookups are unchanged in speed, and every measured result was
  byte-identical before and after.
- CI now checks formatting, and the whole workspace was reformatted to a
  current rustfmt in this release. No behaviour change — the graph gate's
  golden digests are untouched across the sweep.

### Fixed

- The release checks that keep our stated KGLite version consistent now also
  cover the CI workflow pins, for both embedded engines. Those four pins had
  drifted out of step with the manifest during this upgrade while every other
  version site moved correctly; the check that exists to catch exactly that
  had never looked at them.

## [0.2.4] - 2026-08-17

A security release for the MCP server: the served tool surface is now closed
by default, GitHub credentials can no longer reach a music server, and a
running server picks up a rebuilt graph without a client restart. The embedded
engines move to KGLite 0.16.3 and Sonara 0.3.6.

**After upgrading, re-run `sonagram mcp install --force` to refresh the
installed server assets** — the new manifest is what closes the surface.

### Graph schema

No change. Graph schema stays at **v3** and both canonical digests are
byte-identical to 0.2.3 — stored `.kgl` graphs do not need rebuilding, and no
engine move in this release touches the graph or the container format.

### Fixed

- The MCP server's tool surface is now an explicit allowlist of exactly the 13
  music-relevant tools. Previously the surface was open by default: a
  `GITHUB_TOKEN` reachable from the environment (including via a `.env` file
  found by an upward directory walk) silently added authenticated GitHub tools
  to a personal music server. Three defenses now stack: the manifest allowlist
  (anything not named is rejected), the server scrubbing `GITHUB_TOKEN` and
  `GH_TOKEN` at startup, and environment loading pinned to a dedicated
  `sonagram_mcp.env` beside the manifest — the walk-up is gone. (Reported by
  our first production operator; the underlying opt-in default also landed
  upstream in KGLite 0.16.3 / mcp-methods 0.4.5 at our request.)
- Two code-graph tools that could never return anything on a music graph
  (`explore`, `read_code_source`) no longer appear.

### Added

- Live graph refresh: the server watches the served `.kgl` and lazily reloads
  it after `sonagram build` rewrites it, and a `reload_graph` tool forces the
  same refresh on demand — usable from shell-less clients like Claude Desktop.
  A read-only server no longer holds the file's writer lease, so rebuilding a
  served graph in place is now possible at all.
- `sonagram mcp install` creates an operator-owned `sonagram_mcp.env` beside
  the manifest when absent (never overwritten, exempt from `--force`) for
  server environment such as a Last.fm key.
- The generic exploration tools now describe themselves in music-library
  terms instead of code-graph terms.

### Changed

- Embedded KGLite: 0.16.2 → 0.16.3 (MCP-server layer only). Embedded Sonara:
  0.3.5 → 0.3.6, which adds the per-feature analysis lane this release banks
  for a future scan improvement — re-acquiring a single feature (such as
  aggression) will no longer require re-analyzing a whole library. Analysis
  output is bit-identical; cached records carry over untouched.
- CI now builds both sibling engines from their release tags (`v0.16.3`,
  `v0.3.6`) instead of their default branches, so published artifacts are
  provably built against the pinned releases.

## [0.2.3] - 2026-08-16

An engine-upgrade release: the embedded KGLite moves from 0.15.8 to 0.16.2,
bringing its columnar-everywhere storage rewrite and large graph-build
speedups, and sonagram's node reads move to the engine's NodeView API. CI now
builds against exactly the KGLite release we ship.

### Graph schema

No change. Graph schema stays at **v3** and both canonical digests are
byte-identical to 0.2.2 — **stored `.kgl` graphs do not need rebuilding**, and
0.16.2 reads existing (v5) graph files as-is. One compatibility note in the
other direction: a graph **saved** by this version uses KGLite's `.kgl`
container v6, which KGLite tools older than 0.16 cannot read. The `kglite`
Python package requirement therefore moves to `>=0.16.2`, and `pip install -U
sonagram` upgrades it alongside.

### Changed

- Embedded KGLite engine: 0.15.8 → 0.16.2. Node reads go through the engine's
  `NodeView` API (the raw node record no longer carries identity on in-memory
  graphs), and the graph gate now pins that contract with explicit tripwires —
  including one that fails the build if node identity ever silently reads as
  null again.
- The four CI/release workflow checkouts of KGLite are pinned to the `v0.16.2`
  tag, so CI tests exactly the engine version the published wheels embed.

### Fixed

- `sonagram status` no longer reports an existing-but-unreadable graph file as
  if no graph had been built: it now says the file exists but cannot be read,
  includes the load error, and the JSON output carries it in a new
  `graph_error` field.
- When the separately installed `kglite` Python package is too old to read a
  graph sonagram just wrote, the error now names the installed version and the
  fix (`pip install -U 'kglite>=0.16.2'`) instead of surfacing a bare
  file-format error from a load call the user never wrote.

### Added

- Docs: playlist curation queries should name their id column
  (`RETURN t.content_hash AS content_hash`); the agent guide and CLI docs now
  say so explicitly rather than relying on positional auto-detection.

## [0.2.2] - 2026-07-30

A packaging and upstream-hygiene release: the source distribution no longer
carries a stale copy of KGLite, the embedded KGLite moves to 0.15.3, and the
version a user reads in the docs is now mechanically tied to the one sonagram
actually builds against.

### Graph schema

No change. Graph schema stays at **v3** and both canonical digests are
byte-identical to 0.2.1 — **stored `.kgl` graphs do not need rebuilding.** The
KGLite 0.15.3 move touches only error-message wording and Cypher planner
warnings, neither of which feeds the graph.

### Fixed

- The source distribution shipped a vendored snapshot of KGLite that could be
  newer than the version the manifest declared (0.14.5 vendored against a 0.14.4
  declaration in 0.2.1). Installing from source now resolves KGLite from
  crates.io, which also cuts the sdist from ~2.1 MB to ~872 KB.
- The error shown when the `kglite` package is missing told users to
  `pip install kglite>=0.14`, a version this project's own metadata rejects. It
  now names the real floor.

### Changed

- Embedded KGLite moves from 0.15.1 to **0.15.3**, raising the `kglite` runtime
  requirement to `>=0.15.3`. Upstream changes are internal (a `did_you_mean`
  edit-distance fix, unknown-label warnings inside `CALL`/`EXISTS`/`UNION`
  blocks, and corrected dependency floors); no API sonagram uses moved.

### Added

- A version-consistency test binds the embedded KGLite version to a single
  source — the workspace manifest — and fails with the full list of any
  documentation, metadata or error-string site that disagrees. It caught two
  statements that had silently gone stale.
- Release CI now builds the source distribution, verifies it carries no vendored
  KGLite, then installs it into a clean environment and exercises the package
  and its console scripts before anything is published.

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
