# Determinism

**The same library scanned twice must produce a byte-identical graph.**
Determinism is a feature, not an accident: an agent that reasons over the graph,
a golden test that guards it, and a `.kgl` diffed across rescans all rely on
stable node ids, stable ordering, and a stable canonical digest. An unstable node
id is a bug even when every test is green.

## Why it matters

sonagram's output *is* a graph, so its regression net is **digest fidelity +
determinism**, not audio fidelity. The mapping sorts internally and derives node
identity from content (a track's blake3 `content_hash`, never its scan order), so
input order can never leak into the graph. This is what lets a no-op rescan of an
unchanged library rebuild an identical `.kgl`, and what makes a digest diff a
reliable signal that *the mapping* changed.

## The golden gate

The contract is enforced by a committed gate (`GRAPH-GATE.md` in the repo is the
record). It builds the graph from **frozen `TrackAnalysis` fixtures** — captured
sonara output, no audio — renders it to a deterministic canonical string, hashes
that, and asserts the digest matches a committed golden. It has three parts:

- **`golden_graph`** — build from the fixtures and assert the canonical digest
  matches the committed golden. The canonical string covers, all sorted: node-
  and edge-type counts, the `(node_type, id)` identity set, the full per-node and
  per-edge property sweep, and every embedding vector as exact f32 bits. A plain
  and an *enriched* golden (with frozen Last.fm fixtures) are both guarded.
- **`determinism`** — the same records built twice, reversed, and shuffled all
  yield an identical digest. This catches `HashMap`/iteration-order and identity
  leaks that a golden alone would miss.
- **`contract`** — compile-time + runtime assertions against the **real** sonara
  and kglite APIs, so an upstream version bump that breaks the mapping fails here,
  loudly, instead of drifting into a silent mapping bug.

The gate runs with no audio and never re-runs analysis, so it is fast and
reproducible in CI.

## Regenerating goldens — the rule

A red `golden_graph` after a mapping or schema change is a **conscious
decision**. When the graph change is intended, the goldens are regenerated **in
the same commit** as the change, with the reason stated in the commit body. They
are never regenerated to silence a diff that can't be explained — that is the one
move that would make the whole gate worthless.
