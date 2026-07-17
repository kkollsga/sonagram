"""sonagram: map a music library's analysis into a kglite knowledge graph.

Public API (all implemented in the compiled `_sonagram` extension):

- ``scan(library_root, *, progress=None) -> dict`` — scan a library, returning
  a report of counts + failures.
- ``enrich(library_root, *, api_key=None) -> dict`` — fetch Last.fm metadata
  (popularity, folksonomy tags, MBIDs, similar artists/tracks, original-album
  mapping) and cache it under ``<library_root>/.sonagram/lastfm/``. Needs a
  ``LASTFM_API_KEY`` (``api_key=`` overrides env / ``.env``).
- ``build(library_root, out_path=None) -> kglite.KnowledgeGraph`` — build the
  graph from cached analysis and return a live kglite graph (run ``scan`` first).
  Automatically folds in the Last.fm enrichment cache when present.
- ``scan_and_build(library_root, out_path=None, *, progress=None)`` — the two
  above composed.
- ``export_m3u(kgl_path, library_root, out_path, *, cypher=None, track_ids=None)
  -> str`` — write a ``.m3u8`` playlist from a saved ``.kgl`` graph.
"""

from sonagram._sonagram import (  # noqa: F401 — re-exported native surface
    __version__,
    scan,
    enrich,
    build,
    scan_and_build,
    export_m3u,
)

__all__ = ["__version__", "scan", "enrich", "build", "scan_and_build", "export_m3u"]
