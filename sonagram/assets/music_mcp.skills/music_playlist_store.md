---
name: music_playlist_store
description: "TRIGGER after a curated playlist passes audit, or when listing, retrieving, updating, or deleting saved playlists. Use Sonagram's store commands so M3U order and provenance remain paired. SKIP direct sidecar edits."
references_tools: [music_curate_playlist, music_playlists_list, music_playlist_show, music_playlist_update, music_playlist_delete]
applies_when:
  tool_registered: music_playlists_list
  graph_has_property: {node_type: Track, prop_name: path}
---

<!-- sonagram-curation-contract:v1 -->

# Preserve provenance with the playlist

Pass `store.name` to `music_curate_playlist` to store the ordered `.m3u8` and
its curation metadata. Retrieve with `music_playlists_list` and
`music_playlist_show`; change only the request text with
`music_playlist_update`; remove both files with `music_playlist_delete`, which
requires an exact `confirm_slug` match.

Never edit `.meta.json` or reorder `.m3u8` by hand. Stored provenance is the
evidence tying the user brief, resolved policy, audit, repair count, graph,
immutable `build_input_fingerprint`, and ordered IDs together. Use that
fingerprint—not the graph pathname—to decide whether two stored results came
from identical analysis/model inputs.
