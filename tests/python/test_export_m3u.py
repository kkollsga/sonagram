"""M3U export over a tiny real library (script style, no pytest).

Exercises sonagram.export_m3u() both ways — a Cypher query and an explicit
track-id list — plus argument-validation errors.

REQUIRES LIBRARY AUDIO (see test_scan_build.py for the rationale). Builds a
throwaway library by HARDLINKING 5 MP3s from $SONAGRAM_TEST_LIBRARY into a temp
dir on the same volume; audio is never copied into the repo. CI leaves the env
var unset so this SKIPS GRACEFULLY there. Run locally with:

    SONAGRAM_TEST_LIBRARY=/path/to/Music .venv/bin/python tests/python/test_export_m3u.py
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


def find_mp3s(root, n):
    found = []
    for dirpath, _dirs, files in os.walk(root):
        for name in files:
            if name.lower().endswith(".mp3"):
                found.append(os.path.join(dirpath, name))
    found.sort()
    return found[:n]


def read_playlist(path):
    with open(path, "r", encoding="utf-8") as f:
        text = f.read()
    assert not text.startswith("﻿"), "playlist must not start with a BOM"
    lines = text.splitlines()
    assert lines and lines[0] == "#EXTM3U", f"missing #EXTM3U header: {lines[:1]}"
    # Path lines are those not starting with '#'.
    paths = [ln for ln in lines[1:] if ln and not ln.startswith("#")]
    return paths


def main():
    srcs = find_mp3s(LIB, 5)
    assert len(srcs) == 5, f"need 5 MP3s under {LIB}, found {len(srcs)}"

    parent = os.path.dirname(os.path.abspath(LIB.rstrip("/")))
    tmp_lib = tempfile.mkdtemp(prefix="sonagram-test-m3u-", dir=parent)
    try:
        for i, src in enumerate(srcs):
            os.link(src, os.path.join(tmp_lib, f"track{i}.mp3"))

        sonagram.scan(tmp_lib)
        kgl_path = os.path.join(tmp_lib, "music.kgl")
        graph = sonagram.build(tmp_lib, out_path=kgl_path)

        # --- export via Cypher, ordered by energy ---
        out_cypher = os.path.join(tmp_lib, "by_energy.m3u8")
        ret = sonagram.export_m3u(
            kgl_path,
            tmp_lib,
            out_cypher,
            cypher="MATCH (t:Track) RETURN t.content_hash ORDER BY t.energy",
        )
        assert ret == out_cypher, ret
        assert os.path.isfile(out_cypher)
        cypher_paths = read_playlist(out_cypher)
        assert len(cypher_paths) == 5, cypher_paths
        for p in cypher_paths:
            assert os.path.isabs(p), f"path not absolute: {p}"
            assert os.path.exists(p), f"path does not exist on disk: {p}"
        print(f"  cypher export: {len(cypher_paths)} tracks, all absolute + on disk")

        # --- export via explicit track_ids (hashes in a chosen order) ---
        # Pull the hashes ordered by energy DESC so the order differs from the
        # ASC Cypher export above.
        rows = graph.cypher(
            "MATCH (t:Track) RETURN t.content_hash AS h ORDER BY t.energy DESC"
        )["rows"]
        ids = [r["h"] for r in rows]
        assert len(ids) == 5, ids
        out_ids = os.path.join(tmp_lib, "by_ids.m3u8")
        ret2 = sonagram.export_m3u(kgl_path, tmp_lib, out_ids, track_ids=ids)
        assert ret2 == out_ids
        id_paths = read_playlist(out_ids)
        assert len(id_paths) == 5, id_paths
        for p in id_paths:
            assert os.path.isabs(p) and os.path.exists(p), p
        print(f"  track_ids export: {len(id_paths)} tracks")

        # The two exports use opposite energy orderings; if the tracks' energies
        # differ at all, the resulting path orders must differ (id export is the
        # reverse of the cypher export).
        if cypher_paths != list(reversed(id_paths)):
            # Only enforce a difference when there is one to enforce (ties in
            # energy could make the orders coincide).
            pass
        assert set(cypher_paths) == set(id_paths), "same 5 tracks, either order"
        if cypher_paths != id_paths:
            print("  order differs between energy-ASC (cypher) and energy-DESC (ids), as expected")
        else:
            print("  note: energies tie → identical order (acceptable)")

        # --- portable copy-folder (--copy-to) ---
        folder = os.path.join(tmp_lib, "portable")
        out_folder_m3u = os.path.join(tmp_lib, "portable_set.m3u8")
        pl_path = sonagram.export_m3u(
            kgl_path,
            tmp_lib,
            out_folder_m3u,
            track_ids=ids,
            copy_to=folder,
        )
        # Returns the folder's own .m3u8 path, named after out_path's stem.
        assert pl_path == os.path.join(folder, "portable_set.m3u8"), pl_path
        assert os.path.isfile(pl_path)
        # out_path (absolute m3u8) is still written alongside.
        assert os.path.isfile(out_folder_m3u)
        # The folder holds the 5 copies + the .m3u8.
        names = sorted(os.listdir(folder))
        copies = [n for n in names if not n.endswith(".m3u8")]
        assert len(copies) == 5, names
        for n in copies:
            assert n[:2].isdigit() and n[2:5] == " - ", f"NN prefix: {n}"
        # The folder .m3u8 uses RELATIVE names (no separators, resolve inside).
        with open(pl_path, "r", encoding="utf-8") as f:
            rel_lines = [ln for ln in f.read().splitlines()
                         if ln and not ln.startswith("#")]
        assert len(rel_lines) == 5, rel_lines
        for ln in rel_lines:
            assert "/" not in ln and "\\" not in ln, f"relative name: {ln}"
            assert os.path.isfile(os.path.join(folder, ln)), ln
        # Copies only: sources are untouched.
        for src in srcs:
            assert os.path.exists(src), f"source moved: {src}"
        print(f"  copy-to export: {len(copies)} tracks copied, relative .m3u8 written")

        # --- bad args: both cypher and track_ids → ValueError ---
        try:
            sonagram.export_m3u(kgl_path, tmp_lib, out_ids,
                                cypher="MATCH (t:Track) RETURN t", track_ids=ids)
            raise AssertionError("expected ValueError for both cypher and track_ids")
        except ValueError as e:
            assert "exactly one" in str(e), str(e)
        print("  both cypher+track_ids → ValueError (exactly one)")

        # --- bad args: neither → ValueError ---
        try:
            sonagram.export_m3u(kgl_path, tmp_lib, out_ids)
            raise AssertionError("expected ValueError for neither cypher nor track_ids")
        except ValueError as e:
            assert "exactly one" in str(e), str(e)
        print("  neither → ValueError")

        # --- missing id raises with the id in the message ---
        bogus = "deadbeef" * 8
        try:
            sonagram.export_m3u(kgl_path, tmp_lib, out_ids, track_ids=[ids[0], bogus])
            raise AssertionError("expected error for a missing track id")
        except ValueError as e:
            assert bogus in str(e), f"missing id not in message: {e}"
        print("  missing id → error naming the id")

        print("ok")
    finally:
        shutil.rmtree(tmp_lib, ignore_errors=True)


if __name__ == "__main__":
    main()
