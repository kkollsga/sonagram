# Agent guide

sonagram is built for AI agents: the graph is served by
`sonagram-mcp-server`, a thin KGLite 0.16.19 frontend, and an agent translates intent into a typed Sonagram
curation brief. The library—not the agent—selects, orders, repairs, audits, and
stores playlists. The repo
ships the full agent-facing manual as **`AGENT-GUIDE.md`** at its root, plus an
invocable Claude Code skill at **`skills/sonagram-playlist/`**. This page is the
orientation; the manual is the reference.

## Generic and typed tools

An agent works the graph through three MCP tools:

- **`graph_overview`** — the node/edge inventory with live counts and sample ids.
  Call it first on an unfamiliar graph to see the library's shape.
- **`cypher_query`** — run one openCypher query, get up to ~15 rows inline. It
  takes only a `query` string: there is **no parameter binding over MCP**, so
  inline every literal (`{title:'Marry You'}`, `[0.1, ...]`). A `$param`
  reference errors.
- **`music_library_profile`** — report eligible counts, axis coverage, and
  distributions before translating an unusual request. Read coverage and
  p25/median/p75 before choosing any numeric threshold.

Typed `music_curation_policy`, `music_curate_playlist`,
`music_audit_playlist`, `music_explain_playlist`, and `music_playlist*` store
tools call Sonagram's library methods against the same live graph. MCP-only
hosts therefore use the full contract without inventing final IDs.

The [graph schema](graph-schema.md) is the node/edge/property reference those
queries run against.

## The four query archetypes

`AGENT-GUIDE.md` gives a copy-paste-runnable cookbook for each:

1. **Filter + order** — the graph's bread and butter: every filterable signal is
   a flat scalar on `Track`, so `WHERE t.bpm >= 110 AND t.vocalness < 0.5 …
   ORDER BY t.energy` needs no joins.
2. **Discover / group** — aggregate over the dimension nodes (`Genre`, `Decade`,
   `TempoBand`) or read the detected `Style` communities' profiles to learn
   what's in an unfamiliar library.
3. **Similarity** — the `SIMILAR_TO` hop ("like this, but calmer") composes with
   any `WHERE`; chain `-[:SIMILAR_TO*1..2]->` to reach beyond the top-10 horizon.
4. **Sequence evidence** — inspect `CAMELOT_ADJACENT`, tempo, and energy to
   explain transitions; the Sonagram sequencer still owns the final order.

## Creating and accepting the answer

Use graph tools for exploration, then call `music_curate_playlist`, run
[`sonagram curate`](cli.md#sonagram-profile--curate--audit--explain), or use
[`curate_playlist`](python-api.md#curation-profile_library-curate_playlist-audit_playlist-explain_playlist).
Choose a preset plus size/duration/seeds; do not hand-select or reorder returned
IDs. Accept only `exportable: true` with `audit.passed: true`.

For advanced intent, resolve the preset with `sonagram policy` or Python
`sonagram.curation_policy`, then use typed reference-seed similarity/relative
targets and eligibility filters for artist, genre, style, decade, and year.
Unknown fields are rejected; explicitly unsupported intent produces a
structured non-exportable result.

Aggression is opt-in policy, never a preset assumption. Profile its coverage,
percentiles, and exact model counts first, then use typed
`eligibility.min_aggression` / `max_aggression`, `targets.aggression`, or
`targets.relative_aggression` plus its margin. The score is a perceptual rank,
not a probability; confidence is evidence support. A null score is a valid
abstention but fails an active directive as `aggression_unknown`. Do not replace
it with the separate `mood_aggressive` heuristic, `tension_index`, or private
agent ranking.

The independent audit enforces eligibility, Track/Song deduplication,
artist/album concentration and spacing, duration, transitions, and arc error.
If a passing result is still poor, record that measurable defect as a Sonagram
library issue rather than hiding it with an agent-only heuristic.

The manual also carries extensive **pitfalls and field notes** from live agent
validation (e.g. gate `bpm` on `bpm_confidence`; use exact-model fused aggression
rather than `mood_aggressive`; `SIMILAR_TO` is directed; compilation folder
names often beat scalars for vibe grouping). Read
[`AGENT-GUIDE.md`](https://github.com/kkollsga/sonagram/blob/main/AGENT-GUIDE.md)
in the repo before curating against a real library.
