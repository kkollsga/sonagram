---
name: music_playlist_store
description: "TRIGGER after a curated playlist passes audit, or when listing, retrieving, updating, or deleting saved playlists. Use Sonagram's store commands so M3U order and provenance remain paired. SKIP direct sidecar edits."
references_tools: [music_library_profile]
applies_when:
  graph_has_property: {node_type: Track, prop_name: path}
---

<!-- sonagram-curation-contract:v1 -->

# Preserve provenance with the playlist

Pass `--name` to `sonagram curate` to store the ordered `.m3u8` and
its curation metadata. Retrieve with `sonagram playlists` and `sonagram
playlists show <slug>`; change only the request text with `playlists update`;
remove both files with `playlists delete`.

Never edit `.meta.json` or reorder `.m3u8` by hand. Stored provenance is the
evidence tying the user brief, resolved policy, audit, repair count, graph, and
ordered IDs together.
