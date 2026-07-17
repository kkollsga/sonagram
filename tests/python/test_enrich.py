"""Last.fm enrichment binding smoke test (script style, no pytest).

Exercises `sonagram.enrich()` and the automatic enrichment pickup in
`sonagram.build()`. Skips gracefully when the prerequisites are absent, so CI
(which sets neither) never fails here:

- No ``$SONAGRAM_TEST_LIBRARY`` (a real MP3 library) → SKIP. The enrich path
  needs cached analysis records, which need real audio (never committed).
- No ``$LASTFM_API_KEY`` (env or a .env file) → we still assert that
  ``enrich()`` raises a clear ``no LASTFM_API_KEY`` error, then SKIP the live
  fetch.

Run locally with a key:

    SONAGRAM_TEST_LIBRARY=/path/to/Music LASTFM_API_KEY=... \
        .venv/bin/python tests/python/test_enrich.py
"""

import os
import sys
import shutil
import tempfile

LIB = os.environ.get("SONAGRAM_TEST_LIBRARY")
if not LIB or not os.path.isdir(LIB):
    print("SKIP: no library ($SONAGRAM_TEST_LIBRARY unset or not a dir)")
    sys.exit(0)

import sonagram  # noqa: E402
import kglite  # noqa: E402


def find_mp3s(root, n):
    found = []
    for dirpath, _dirs, files in os.walk(root):
        for name in sorted(files):
            if name.lower().endswith(".mp3"):
                found.append(os.path.join(dirpath, name))
    found.sort()
    return found[:n]


def main():
    srcs = find_mp3s(LIB, 5)
    assert len(srcs) == 5, f"need 5 MP3s under {LIB}, found {len(srcs)}"

    parent = os.path.dirname(os.path.abspath(LIB.rstrip("/")))
    tmp_lib = tempfile.mkdtemp(prefix="sonagram-enrich-lib-", dir=parent)
    try:
        for i, src in enumerate(srcs):
            os.link(src, os.path.join(tmp_lib, f"track{i}.mp3"))

        # Scan first — enrichment needs cached analysis records to derive the
        # distinct artist/track/album sets.
        report = sonagram.scan(tmp_lib)
        assert report["analyzed"] == 5, report

        have_key = bool(os.environ.get("LASTFM_API_KEY"))
        if not have_key:
            # No key: enrich must raise a clear error, then we stop.
            try:
                sonagram.enrich(tmp_lib)
            except RuntimeError as e:
                assert "no LASTFM_API_KEY" in str(e), f"unexpected message: {e}"
                print("  enrich: correctly errored without a key")
            else:
                raise AssertionError("enrich() should have raised without a key")
            print("SKIP: no LASTFM_API_KEY — live fetch skipped")
            return

        # With a key: fetch and assert the report shape + counts.
        er = sonagram.enrich(tmp_lib)
        assert isinstance(er, dict), type(er)
        for k in (
            "artists_fetched", "artists_skipped", "artists_failed",
            "tracks_fetched", "tracks_skipped", "tracks_failed",
            "albums_fetched", "albums_skipped", "albums_failed",
            "elapsed_sec",
        ):
            assert k in er, f"missing key {k} in {er}"
        print(f"  enrich: {er['artists_fetched']} artists, {er['tracks_fetched']} tracks, "
              f"{er['albums_fetched']} albums fetched")

        # Re-run is incremental: nothing new fetched, everything skipped.
        er2 = sonagram.enrich(tmp_lib)
        assert er2["artists_fetched"] == 0, er2
        assert er2["tracks_fetched"] == 0, er2
        print(f"  re-enrich: incremental (skipped "
              f"{er2['artists_skipped']} artists, {er2['tracks_skipped']} tracks)")

        # build() auto-loads the enrichment cache and the graph carries the
        # popularity properties.
        graph = sonagram.build(tmp_lib)
        assert isinstance(graph, kglite.KnowledgeGraph), type(graph)
        res = graph.cypher(
            "MATCH (t:Track) WHERE t.lastfm_playcount IS NOT NULL "
            "RETURN count(t) AS c"
        )
        n = res["rows"][0]["c"]
        print(f"  build: {n} tracks carry lastfm_playcount")

        print("ok")
    finally:
        shutil.rmtree(tmp_lib, ignore_errors=True)


if __name__ == "__main__":
    main()
