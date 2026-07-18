---
name: music_song_versions
description: "TRIGGER when inspecting duplicate recordings, canonical choices, covers, or alternate versions. Traverse Track→VERSION_OF→Song and use canonical_hash/is_canonical. SKIP filename/title-only deduplication."
references_tools: [cypher_query, graph_overview]
applies_when:
  graph_has_node_type: [Song]
  graph_has_property: {node_type: Track, prop_name: is_canonical}
---

<!-- sonagram-curation-contract:v1 -->

# Song and recording semantics

`Track` is a file/recording; `Song` exists only for grouped versions.
`Song.canonical_hash` selects a recognized release first, then recording
quality, then content hash. It is not proof of a historical master.

Final playlists normally require `t.is_canonical = true` and at most one Track
per Song. Unknown-artist repair is conservative and audio-confirmed; known
artist covers remain separate. Use `VERSION_OF` and `canonical_hash` rather
than title strings to explain why a version was kept or excluded.
