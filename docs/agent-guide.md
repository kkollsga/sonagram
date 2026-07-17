# Agent guide

sonagram is built for AI agents: the graph is served over MCP by
`kglite-mcp-server`, and an agent curates playlists by querying it. The repo
ships the full agent-facing manual as **`AGENT-GUIDE.md`** at its root, plus an
invocable Claude Code skill at **`skills/sonagram-playlist/`**. This page is the
orientation; the manual is the reference.

## The two tools

An agent works the graph through two MCP tools:

- **`graph_overview`** — the node/edge inventory with live counts and sample ids.
  Call it first on an unfamiliar graph to see the library's shape.
- **`cypher_query`** — run one openCypher query, get up to ~15 rows inline. It
  takes only a `query` string: there is **no parameter binding over MCP**, so
  inline every literal (`{title:'Marry You'}`, `[0.1, ...]`). A `$param`
  reference errors.

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
4. **Sequence** — walk `CAMELOT_ADJACENT` for a harmonic step plus an energy ramp
   to build a DJ set.

## Materializing the answer

A query answers *which tracks, in what order*; you turn that into a playable
`.m3u8` outside the graph via [`sonagram playlist`](cli.md#sonagram-playlist) or
[`export_m3u`](python-api.md#export_m3u), passing the `content_hash` values a
query returns. Order is preserved verbatim, so for any hand-built arc (a ramp, a
breather, a harmonic chain) the `--ids` / `track_ids=` path — export candidates,
order them client-side, feed the hash list — is the primary route, not one big
`ORDER BY`.

## The quality bar

`AGENT-GUIDE.md` grades every playlist against four requirements before export —
treat them as requirements, not suggestions:

1. **Duration-check every pick.** Casual/mood playlists default to radio-length
   cuts (`duration_sec <= 330`) unless the brief wants epics — always `RETURN
   t.duration_sec` in the candidate query.
2. **Audit `era_source` first, then fall back to your own knowledge.** When
   `era_source = 'original_year'` the decade is era-true; when `'file_year'` the
   `year` is the file tag (often a reissue/compilation date), so validate era
   claims yourself.
3. **Critical slots need style-world cohesion, not just scalar fit.** Finales,
   the seed side of a "like this" set, and genre-set peaks must belong to the
   brief's musical world — scalars will happily pass a rap track into a disco set.
4. **Sanity-read the final tracklist as a human would** — artist/title, in order.
   If a pick needs a defensive explanation, swap it.

The manual also carries extensive **pitfalls and field notes** from live agent
validation (e.g. gate `bpm` on `bpm_confidence`; `mood_aggressive` inverts on
extreme material; `SIMILAR_TO` is directed; compilation folder names often beat
scalars for vibe grouping). Read
[`AGENT-GUIDE.md`](https://github.com/kkollsga/sonagram/blob/main/AGENT-GUIDE.md)
in the repo before curating against a real library.
