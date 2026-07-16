"""End-to-end scan + build over a tiny real library (script style, no pytest).

This exercises the full binding path: sonagram.scan() with a progress callback,
sonagram.build() returning a live kglite.KnowledgeGraph, a Cypher round-trip on
that graph, and an incremental no-op rescan.

REQUIRES LIBRARY AUDIO. sonara analysis needs real MP3s, which are never
committed. The test builds a throwaway library by HARDLINKING 5 MP3s from the
library at $SONAGRAM_TEST_LIBRARY into a temp dir on the SAME volume (hardlinks
can't cross filesystems, and we never copy audio). CI leaves the env var unset,
so this SKIPS GRACEFULLY there; run locally with e.g.

    SONAGRAM_TEST_LIBRARY=/path/to/Music .venv/bin/python tests/python/test_scan_build.py
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
import kglite  # noqa: E402  — build() returns a kglite.KnowledgeGraph


def find_mp3s(root, n):
    """First `n` MP3s under `root`, sorted for stability."""
    found = []
    for dirpath, _dirs, files in os.walk(root):
        for name in sorted(files):
            if name.lower().endswith(".mp3"):
                found.append(os.path.join(dirpath, name))
        if len(found) >= n * 4:  # enough candidates; sort the pool below
            pass
    found.sort()
    return found[:n]


def main():
    srcs = find_mp3s(LIB, 5)
    assert len(srcs) == 5, f"need 5 MP3s under {LIB}, found {len(srcs)}"

    # Temp library on the SAME volume as the source (hardlinks require it).
    parent = os.path.dirname(os.path.abspath(LIB.rstrip("/")))
    tmp_lib = tempfile.mkdtemp(prefix="sonagram-test-lib-", dir=parent)
    try:
        for i, src in enumerate(srcs):
            os.link(src, os.path.join(tmp_lib, f"track{i}.mp3"))

        # --- scan() with a progress collector ---
        events = []
        report = sonagram.scan(tmp_lib, progress=lambda s, d, t: events.append((s, d, t)))

        assert isinstance(report, dict), f"report not a dict: {type(report)}"
        assert report["total_files"] == 5, report
        assert report["analyzed"] == 5, report
        assert report["failed"] == [], report["failed"]
        assert report["reused_stat_match"] == 0, report
        assert "elapsed_sec" in report and report["elapsed_sec"] >= 0.0, report

        assert events, "progress callback was never called"
        stages = {s for (s, _d, _t) in events}
        assert "walk" in stages and "done" in stages, stages
        assert "analyze" in stages, stages  # 5 unseen files → analysis fired
        print(f"  scan: {report['analyzed']} analyzed, {len(events)} progress events, "
              f"stages={sorted(stages)}")

        # --- build() → live kglite.KnowledgeGraph, persisted to a .kgl ---
        kgl_path = os.path.join(tmp_lib, "music.kgl")
        graph = sonagram.build(tmp_lib, out_path=kgl_path)
        assert isinstance(graph, kglite.KnowledgeGraph), type(graph)
        assert os.path.isfile(kgl_path), "out_path .kgl was not persisted"

        # kglite.cypher() returns {'columns': [...], 'rows': [{col: val}, ...]}.
        res = graph.cypher("MATCH (t:Track) RETURN count(t) AS c")
        n = res["rows"][0]["c"]
        assert n == 5, f"expected 5 Track nodes, got {n}"
        print(f"  build: kglite graph, MATCH (t:Track) RETURN count(t) == {n}")

        # --- build() without out_path → temp-file handoff still works ---
        graph2 = sonagram.build(tmp_lib)
        assert isinstance(graph2, kglite.KnowledgeGraph), type(graph2)
        assert graph2.cypher("MATCH (t:Track) RETURN count(t) AS c")["rows"][0]["c"] == 5

        # --- incremental no-op rescan analyzes nothing ---
        report2 = sonagram.scan(tmp_lib)
        assert report2["analyzed"] == 0, report2
        assert report2["reused_stat_match"] == 5, report2
        print(f"  rescan: analyzed={report2['analyzed']} (no-op), "
              f"reused_stat_match={report2['reused_stat_match']}")

        print("ok")
    finally:
        shutil.rmtree(tmp_lib, ignore_errors=True)


if __name__ == "__main__":
    main()
