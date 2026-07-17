# GRAPH-GATE.md — sonagram's graph regression net

sonagram's output is a graph, so its gate is **digest fidelity + determinism**,
not audio fidelity. It builds the graph from the 15 frozen `TrackAnalysis`
fixtures (`sonagram/tests/fixtures/analyses/*.json` — captured sonara output, no
audio), renders it to a deterministic canonical string, SHA-256s that, and
asserts the digest matches the committed golden. Determinism needs no audio and
never re-runs analysis.

## The command

```
cargo test -p sonagram --test golden_graph
```

Green here is the standing contract every future phase must keep. The test lives
in `sonagram/tests/golden_graph.rs`; the goldens in `sonagram/tests/goldens/`.

## Two goldens: plain and enriched (P12)

There are **two** committed goldens, both built from the same 15 frozen analysis
fixtures:

- **`library.sha256`** — the graph built **without** Last.fm enrichment
  (`build_graph`). This is the standing digest every phase must keep. P12 does
  **not** change it: `build_graph` delegates to
  `build_graph_with_enrichment(records, None, library)`, a byte-identical path.
- **`library-enriched.sha256`** — the graph built **with** the frozen
  enrichment fixtures at `tests/fixtures/lastfm/{artists,tracks,albums}.json`
  (`build_graph_with_enrichment(records, Some(&enrichment), library)`). This
  digest covers the popularity/MBID/original-album `Track`/`Artist`/`Album`
  props, the folksonomy `IN_GENRE` edges (Genre 10→16, IN_GENRE 14→23), and the
  `CROWD_SIMILAR` edges (8: 4 weighted Track→Track + 4 `source="lastfm"`
  Artist→Artist). Its `golden_graph_enriched` test + `determinism_enriched`
  guard the enrichment mapping.

## What each part proves

- **`golden_graph`** — builds from the 15 fixtures (no enrichment), digests the
  canonical render, and asserts it equals `tests/goldens/library.sha256`. On
  mismatch it prints both digests and the first line where the live render
  diverges from the committed snapshot `tests/goldens/library.canonical.txt`, so
  a digest diff is always explainable. The canonical string covers, all sorted:
  node-type counts, edge-type counts, the (node_type, id) identity set, the full
  per-node property sweep, the full per-edge property sweep, and every embedding
  store (dimension, model_id, metric, and per-node vectors as exact f32 bits via
  `to_bits()` hex).
- **`golden_graph_enriched`** (P12) — same render + digest, built **with** the
  frozen enrichment, asserted against `tests/goldens/library-enriched.sha256`
  (snapshot `library-enriched.canonical.txt`). `determinism_enriched` proves the
  enriched build is order-independent. Integration asserts on the exact
  enrichment shape live in `tests/graph_enriched.rs`.
- **`determinism`** — the same records built twice, in reversed order, and in a
  deterministically shuffled order (fixed seed-free swap, no `rand`) all yield an
  identical digest. `build_graph` sorts internally, so input order must never
  leak into node identity or ordering — catches `HashMap`/iteration-order and
  identity leaks a golden alone would miss.
- **`contract_sonara` / `contract_kglite`** — compile-time + runtime assertions
  against the **real** upstream APIs, so a version bump that renames or removes
  anything sonagram maps fails HERE, loudly, instead of drifting into a silent
  mapping bug. Covers: `TrackAnalysis` field presence (a no-`..` destructure —
  compiler breaks on any rename/removal/addition), `AnalysisConfig`/`AnalysisMode`
  construction, `ANALYSIS_SCHEMA_VERSION == 1`, `similarity::{EMBEDDING_DIM == 48,
  SIMILARITY_VERSION == 1, WEIGHTS.len() == 48}`, and kglite `EdgeSpec` /
  `DataFrame::new` / `EmbeddingStore::new(48).dimension == 48` /
  `StorageMode::Memory`. Each assert says what breaks downstream if it changes.

## Regenerating the goldens — THE RULE

```
cargo test -p sonagram --test golden_graph -- --ignored capture_goldens
```

This rewrites **all four** golden files — `library.sha256` +
`library.canonical.txt` **and** `library-enriched.sha256` +
`library-enriched.canonical.txt` — from the current code. **A red `golden_graph`
(or `golden_graph_enriched`) after a mapping/schema change is a conscious
decision.** Regenerate **only** when the graph change is intended, **in the same
commit** as the change, and **say why in the commit body**. Never regenerate to
silence a diff you cannot explain — that is the one move that makes the whole
gate worthless.

## Golden history

| Date       | Digest (first 12) | Reason                   |
|------------|-------------------|--------------------------|
| 2026-07-16 | `d7a4a24f5366`    | initial goldens (P5)     |
| 2026-07-17 | `7ca12b8ac3bd`    | P6: +SIMILAR_TO/+CAMELOT_ADJACENT/+Style (intended) |
| 2026-07-17 | `9fa521068300`    | P10b: mutual-kNN style communities + unique names (intended) |
| 2026-07-17 | `fa5531c899d6`    | P10c: adaptive style threshold (intended) |
| 2026-07-17 | `acf45a46d51a`    | P11: sonara 0.2.3 sync — chroma fix + schema v2 (intended) |
| 2026-07-17 | `5c15e38bbdf5`    | P12: **enriched** golden added (`library-enriched.sha256`); Last.fm popularity/folksonomy/CROWD_SIMILAR. Plain `library.sha256` UNCHANGED (`acf45a46d51a`). |
| 2026-07-17 | `2c6d98954873`    | P14: sonara 0.2.4 sync — vocalness v2, recalibrated scales, bpm_confidence, original_year (intended). Enriched golden → `5d3519bdb6a9`. 15 fixtures re-captured at schema v3; new `Track.bpm_confidence`/`original_year`/`era_source` props; Decade/FROM_DECADE now prefer `original_year`; acousticness style cutoffs recalibrated (acoustic≥0.60/electric≤0.30). danceability recalibration shifted embedding dim 37 on all 15, re-forming styles 3→2. |
| 2026-07-17 | `c75a0112b8e9`    | P17: **Source dimension + `source_root`** (intended). Every build now stamps one `Source` node per source (id = source root; `path` + `n_tracks`), a `Track-[:FROM_SOURCE]->Source` edge per track, and a `Track.source_root` property — so the single-root fixture path and the new multi-source path share ONE code path and playlist export resolves absolute paths off `source_root`. Fixtures build single-source (root `"fixtures"`): +1 `Source` node, +15 `FROM_SOURCE` edges, +`source_root` on all 15 `Track`s. Enriched golden → `b07fa0edb7f4`. No analysis change. |
| 2026-07-17 | `c75a0112b8e9`    | P19: Source `scan_fingerprint` (intended) — **no digest change (fixture builds carry no fingerprint)**. Builds now stamp a `Source.scan_fingerprint` (blake3 over the sorted `rel_path\|size\|mtime` scan state, saved in `index.json`) so `sonagram status` can report `graph_stale`. The column is added **only when a source carries a fingerprint**; the frozen fixtures build with no scan index (`SourceInput.scan_fingerprint = None`), so the property is omitted and BOTH goldens are byte-unchanged (plain `c75a0112b8e9`, enriched `b07fa0edb7f4`). Verified: `golden_graph` + `golden_graph_enriched` pass without regen. |
| 2026-07-17 | `5d37c0b029c3`    | P21 Stage A + B: **graph schema v2** (intended). `Track` gains nine curve-derived Stage-A properties (`macro_dynamics`, `energy_arc_range`, `energy_builds_per_min`, `flow_smoothness`, `chord_vocab`, `chord_entropy`, `chord_churn`, `tempo_steadiness`, `seg_density`) and five percentile-calibrated Stage-B axes (`arousal_index`, `valence_index`, `tension_index`, `recording_quality`, `quality_tier`), all computed at build time from the already-cached record curves/scalars (pure mapper, no re-scan). Both goldens moved (the `schema_version` header/property alone changes v1→v2): plain `5d37c0b029c3`, enriched `213d682199075f`. Regenerated via `capture_goldens` in the same change. |
| 2026-07-17 | `9977d14b141a`    | P21 Stage C: **Song/version layer** (intended, still schema v2). Every `Track` gains a non-null `is_canonical` bool; a new `Song` node groups recordings sharing `(artist_id, normalized_title)` (≥2 members), each carrying `Track-[:VERSION_OF]->Song`. The 15 frozen fixtures are all distinct songs, so this build adds **no** `Song` node and **no** `VERSION_OF` edge — the only fixture change is `is_canonical=true` on all 15 `Track`s (the universal canonical filter). Both goldens moved on that one property: plain `9977d14b141a`, enriched `40709837f90c`. Regenerated via `capture_goldens` in the same change; the synthetic multi-version case is gated by `tests/song_versions.rs`. |
| 2026-07-17 | `72dedb4064f2`    | P21b gap 1: **non-music mood gate** (intended, still schema v2). Every `Track` gains non-null `is_music`; spectral flatness is removed from `tension_index`, and rows above the conservative `0.10` flatness threshold are excluded before mood-axis calibration and receive null arousal/valence/tension. All frozen fixtures are music, so each gains `is_music=true`; tension percentiles also change because the invalid flatness component is gone. Plain → `72dedb4064f2`, enriched → `6e383d4b99ac`. Regenerated via `capture_goldens` in the same change; the false/null path is gated by `tests/graph_build.rs`. |
