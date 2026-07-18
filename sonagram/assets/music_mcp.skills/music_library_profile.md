---
name: music_library_profile
description: "TRIGGER before translating an unusual playlist request into policy, or when asked what the library contains. Use the typed profile tool first; use Cypher only for a narrower follow-up. SKIP repeated profiling when a fresh profile is already in context."
references_tools: [music_library_profile]
applies_when:
  tool_registered: music_library_profile
  graph_has_property: {node_type: Track, prop_name: energy}
---

<!-- sonagram-curation-contract:v1 -->

# Profile, then express intent

Call `music_library_profile` once for coverage and means. For tails or genres,
use one aggregate Cypher query rather than fetching candidate rows. Percentile
axes (`arousal_index`, `tension_index`, `popularity`, `recording_quality`) are
library-relative: `0.5` is the median. Null coverage must be reported, never
treated as zero.

Profiling informs a typed brief/policy; it does not select final tracks. For a
standard request, prefer the preset without custom thresholds.
