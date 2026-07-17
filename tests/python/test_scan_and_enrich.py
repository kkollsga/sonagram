"""scan_and_enrich() binding + on-disk progress snapshot (P20, script style).

Mirrors test_scan_build.py: REQUIRES LIBRARY AUDIO via $SONAGRAM_TEST_LIBRARY
(hardlinked into a temp dir, never copied); SKIPS GRACEFULLY when unset.

Runs the combined pipeline **without a resolvable Last.fm key** (env cleared,
$SONAGRAM_HOME pointed at an empty dir) so the run stays offline and must
degrade to a plain scan with `enrich=None` — the graceful-degradation contract.
Also asserts the scan left a finalized `scan_progress.json`, the P20 guarantee
that progress is observable no matter which entry point ran the scan.
"""

import json
import os
import sys
import shutil
import tempfile

LIB = os.environ.get("SONAGRAM_TEST_LIBRARY")
if not LIB or not os.path.isdir(LIB):
    print("SKIP: no library ($SONAGRAM_TEST_LIBRARY unset or not a dir)")
    sys.exit(0)

# Make the Last.fm key unresolvable BEFORE importing sonagram: no env var, no
# cwd .env, and SONAGRAM_HOME at an empty temp dir (the last .env fallback).
os.environ.pop("LASTFM_API_KEY", None)
_home = tempfile.mkdtemp(prefix="sonagram-test-home-")
os.environ["SONAGRAM_HOME"] = _home
os.chdir(tempfile.mkdtemp(prefix="sonagram-test-cwd-"))

import sonagram  # noqa: E402


def find_mp3s(root, n):
    found = []
    for dirpath, _dirs, files in os.walk(root):
        for name in sorted(files):
            if name.lower().endswith(".mp3"):
                found.append(os.path.join(dirpath, name))
    found.sort()
    return found[:n]


def main():
    srcs = find_mp3s(LIB, 3)
    assert len(srcs) == 3, f"need 3 MP3s under {LIB}, found {len(srcs)}"

    parent = os.path.dirname(os.path.abspath(LIB.rstrip("/")))
    tmp_lib = tempfile.mkdtemp(prefix="sonagram-test-se-", dir=parent)
    try:
        for i, src in enumerate(srcs):
            os.link(src, os.path.join(tmp_lib, f"track{i}.mp3"))

        events = []
        out = sonagram.scan_and_enrich(
            tmp_lib, progress=lambda s, d, t: events.append((s, d, t))
        )

        # Shape: {"scan": dict, "enrich": None} (no key resolvable → no network).
        assert isinstance(out, dict), out
        assert set(out) == {"scan", "enrich"}, out
        assert out["enrich"] is None, f"expected no-key degradation, got {out['enrich']}"
        assert out["scan"]["total_files"] == 3, out
        assert out["scan"]["analyzed"] == 3, out
        assert out["scan"]["failed"] == [], out
        assert any(s == "analyze" for s, _d, _t in events), events
        assert events[-1][0] == "done", events

        # P20: the scan left a finalized on-disk progress snapshot.
        with open(os.path.join(tmp_lib, ".sonagram", "scan_progress.json")) as f:
            prog = json.load(f)
        assert prog["stage"] == "done", prog
        assert prog["total"] == 3 and prog["analyzed"] == 3, prog
        assert prog["analyze_done"] == prog["analyze_total"] == 3, prog

        print("OK: scan_and_enrich degraded gracefully + progress snapshot finalized")
    finally:
        shutil.rmtree(tmp_lib, ignore_errors=True)
        shutil.rmtree(_home, ignore_errors=True)


if __name__ == "__main__":
    main()
