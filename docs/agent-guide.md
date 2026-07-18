# Agent guide

sonagram is built for AI agents: the graph is served over MCP by
`kglite-mcp-server`, and an agent translates intent into a typed Sonagram
curation brief. The library—not the agent—selects, orders, repairs, audits, and
stores playlists. The repo
ships the full agent-facing manual as **`AGENT-GUIDE.md`** at its root, plus an
invocable Claude Code skill at **`skills/sonagram-playlist/`**. This page is the
orientation; the manual is the reference.

## The three tools

An agent works the graph through three MCP tools:

- **`graph_overview`** — the node/edge inventory with live counts and sample ids.
  Call it first on an unfamiliar graph to see the library's shape.
- **`cypher_query`** — run one openCypher query, get up to ~15 rows inline. It
  takes only a `query` string: there is **no parameter binding over MCP**, so
  inline every literal (`{title:'Marry You'}`, `[0.1, ...]`). A `$param`
  reference errors.
- **`music_library_profile`** — report eligible counts, axis coverage, and
  means before translating an unusual request.

On KGLite 0.14.0, typed curate/audit/store calls still require the agent host's
shell or Python runtime; an MCP-only host can explore but must not invent final
IDs. The upstream domain-tool seam is tracked for a future direct MCP route.

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

Use graph tools for exploration, then run [`sonagram curate`](cli.md#sonagram-profile--curate--audit--explain)
or [`curate_playlist`](python-api.md#curation-profile_library-curate_playlist-audit_playlist-explain_playlist).
Choose a preset plus size/duration/seeds; do not hand-select or reorder returned
IDs. Accept only `exportable: true` with `audit.passed: true`.

For advanced intent, resolve the preset with `sonagram policy` or Python
`sonagram.curation_policy`, then use typed reference-seed similarity/relative
targets and eligibility filters for artist, genre, style, decade, and year.
Unknown fields are rejected; explicitly unsupported intent produces a
structured non-exportable result.

The independent audit enforces eligibility, Track/Song deduplication,
artist/album concentration and spacing, duration, transitions, and arc error.
If a passing result is still poor, record that measurable defect as a Sonagram
library issue rather than hiding it with an agent-only heuristic.

The manual also carries extensive **pitfalls and field notes** from live agent
validation (e.g. gate `bpm` on `bpm_confidence`; `mood_aggressive` inverts on
extreme material; `SIMILAR_TO` is directed; compilation folder names often beat
scalars for vibe grouping). Read
[`AGENT-GUIDE.md`](https://github.com/kkollsga/sonagram/blob/main/AGENT-GUIDE.md)
in the repo before curating against a real library.
