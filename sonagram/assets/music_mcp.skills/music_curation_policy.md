---
name: music_curation_policy
description: "TRIGGER for every request to create, improve, or vary a playlist. Translate intent into a Sonagram preset and compact brief, then invoke the library curation method. SKIP hand-authored candidate selection or ordering: Cypher is exploratory, never the final playlist engine."
references_tools: [music_library_profile, music_curation_policy, music_curate_playlist]
applies_when:
  tool_registered: music_curate_playlist
  graph_has_property: {node_type: Track, prop_name: is_music}
---

<!-- sonagram-curation-contract:v1 -->

# The library owns the playlist

Choose one preset: `focus`, `party`, `workout`, `chill`, `discovery`, or
`general`. Add requested size/duration and explicit seed IDs. Plain seeds are
`pinned` (the backward-compatible default). For "songs like X but calmer", use
`seed_role: reference`, set `targets.seed_similarity: prefer`, and set the
relevant `relative_energy` / `relative_arousal` targets to `lower`; the seed is
then an anchor, not an exported track. Optional `relative_*_margin` values in
`[0,1]` require a measurable change; lower/higher are strict even at margin 0.
`pinned_and_reference` does both.

Hard categorical intent belongs in policy eligibility: normalized
`include_*` / `exclude_*` lists cover artists, genres, detected styles, and
decades; `min_year` / `max_year` cover exact ranges. Exclusion dominates.
Resolve a complete preset policy with `music_curation_policy`, amend only typed
fields, then invoke `music_curate_playlist` with the brief and optional policy.
Pass `store.name` only when the result should be persisted; the tool refuses to
store a failed audit. CLI/Python parity remains available as:

```sh
sonagram curate --preset focus --tracks 25 --name "Focused Thinking" \
  --description "focused thinking at work" --format json
```

Python parity is `sonagram.curate_playlist(kgl_path, brief, policy=None)`.
Selection, Song deduplication, sequencing, repair, audit, and explanations are
library behavior. Do not fetch candidates and choose/reorder them yourself. A
non-exportable result is a structured library failure: inspect its issues and
change policy only when the user's intent justifies the change.

Unknown JSON fields are rejected. If a request includes an unenforceable
constraint (for example a lyrical theme when lyrics are unavailable), put a
short description in `brief.unsupported_intents`; Sonagram returns the
structured `unsupported_intent` failure. Never silently approximate it with
agent-selected IDs.
