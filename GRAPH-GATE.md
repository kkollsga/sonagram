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

## What each part proves

- **`golden_graph`** — builds from the 15 fixtures, digests the canonical
  render, and asserts it equals `tests/goldens/library.sha256`. On mismatch it
  prints both digests and the first line where the live render diverges from the
  committed snapshot `tests/goldens/library.canonical.txt`, so a digest diff is
  always explainable. The canonical string covers, all sorted: node-type counts,
  edge-type counts, the (node_type, id) identity set, the full per-node property
  sweep, the full per-edge property sweep, and every embedding store (dimension,
  model_id, metric, and per-node vectors as exact f32 bits via `to_bits()` hex).
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

This rewrites both `library.sha256` and `library.canonical.txt` from the current
code. **A red `golden_graph` after a mapping/schema change is a conscious
decision.** Regenerate **only** when the graph change is intended, **in the same
commit** as the change, and **say why in the commit body**. Never regenerate to
silence a diff you cannot explain — that is the one move that makes the whole
gate worthless. (Expected next intended regen: P6, when similarity + style nodes
land.)

## Golden history

| Date       | Digest (first 12) | Reason                   |
|------------|-------------------|--------------------------|
| 2026-07-16 | `d7a4a24f5366`    | initial goldens (P5)     |
| 2026-07-17 | `7ca12b8ac3bd`    | P6: +SIMILAR_TO/+CAMELOT_ADJACENT/+Style (intended) |
