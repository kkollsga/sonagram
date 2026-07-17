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
