---
name: music_library_profile
description: "TRIGGER before translating an unusual playlist request or any numeric aggression threshold into policy, or when asked what the library contains. Use the typed coverage/distribution profile first; use Cypher only for a narrower follow-up. SKIP repeated profiling when a fresh profile is already in context."
references_tools: [music_library_profile]
applies_when:
  tool_registered: music_library_profile
  graph_has_property: {node_type: Track, prop_name: energy}
---

<!-- sonagram-curation-contract:v1 -->

# Profile, then express intent

Call `music_library_profile` once for coverage and distributions. Read
`present`/`total` plus p25, median, and p75 before choosing a numeric threshold;
do not transfer a cutoff from another library. For tails or genres, use one
aggregate Cypher query rather than fetching candidate rows. Percentile axes
(`arousal_index`, `tension_index`, `popularity`, `recording_quality`) are
library-relative: `0.5` is the median. Null coverage must be reported, never
treated as zero.

Aggression profile keys are `aggression`, `aggression_confidence`,
`aggression_forcefulness`, `aggression_harshness`, `aggression_tension`, and
`aggression_rhythm`; `aggression_models` reports exact model-id counts.
`aggression` is a rank, while confidence is evidence support. Profile
these before adding an explicit aggression threshold or relative target.

Profiling informs a typed brief/policy; it does not select final tracks. For a
standard request, prefer the preset without custom thresholds.
