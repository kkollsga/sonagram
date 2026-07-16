"""sonagram: map a music library's analysis into a kglite knowledge graph.

Public API (all implemented in the compiled `_sonagram` extension):

- ``scan(library_root, *, progress=None) -> dict`` — scan a library, returning
  a report of counts + failures.
- ``build(library_root, out_path=None) -> kglite.KnowledgeGraph`` — build the
  graph from cached analysis and return a live kglite graph (run ``scan`` first).
- ``scan_and_build(library_root, out_path=None, *, progress=None)`` — the two
  above composed.
- ``export_m3u(kgl_path, library_root, out_path, *, cypher=None, track_ids=None)
  -> str`` — write a ``.m3u8`` playlist from a saved ``.kgl`` graph.
"""

from sonagram._sonagram import (  # noqa: F401 — re-exported native surface
    __version__,
    scan,
    build,
    scan_and_build,
    export_m3u,
)

__all__ = ["__version__", "scan", "build", "scan_and_build", "export_m3u"]
