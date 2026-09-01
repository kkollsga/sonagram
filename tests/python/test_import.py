"""Smoke test: the compiled wheel imports and reports the bootstrap version.

Also gates the kglite install hint that is *compiled into* the extension.
``sonagram.build()`` hands the graph off to the separately installed ``kglite``
wheel; when that import fails, the extension tells the user
``pip install kglite>=<floor>``. That literal lives in
``sonagram-python/src/lib.rs`` and had drifted a whole minor version below
``pyproject.toml``'s own floor, so following it installed a version our metadata
forbids. ``sonagram/tests/version_consistency.rs`` gates the Rust *source*; this
gates the *artifact* a user actually receives — a stale .so with a fresh source
tree would pass there and fail here.

Script style (no pytest), matching sonara's tests/python/ convention.
"""

import json
import re
import sys
import tempfile
import types
from pathlib import Path

import sonagram
from sonagram import _sonagram

assert sonagram.__version__ == "0.2.14", (
    f"expected 0.2.14, got {sonagram.__version__!r}"
)

# Single source of truth for the floor, same as the Rust gate: our own metadata.
repo_root = Path(__file__).resolve().parents[2]
pyproject = repo_root / "pyproject.toml"
declared = re.findall(r'"kglite>=([0-9][0-9.]*)"', pyproject.read_text())
assert declared, f"no `kglite>=<version>` runtime dependency found in {pyproject}"
assert len(set(declared)) == 1, f"conflicting kglite floors in {pyproject}: {declared}"
floor = declared[0]

# Compare whole version tokens, never substrings: `>=0.15.10` must not be
# accepted where `>=0.15.1` is expected.
extension = Path(_sonagram.__file__)
blob = extension.read_bytes()
assert b"pip install kglite>=" in blob, (
    f"no `pip install kglite>=` hint found in {extension}; the kglite-import "
    "failure path must tell the user which version to install"
)
found = re.findall(rb"pip install kglite>=([0-9][0-9.]*)", blob)
hinted = sorted({v.decode().rstrip(".") for v in found})
assert hinted == [floor], (
    f"{extension} tells users `pip install kglite>={', '.join(hinted)}` but pyproject.toml "
    f"declares kglite>={floor}. Rebuild (maturin develop) if the source is already fixed."
)

# The *upgrade* hint is a second, differently-phrased literal (it carries `-U`,
# for the user who already has an older kglite installed), so the check above
# cannot see it. Gate it on the same floor.
upgrade = re.findall(rb"pip install -U 'kglite>=([0-9][0-9.]*)'", blob)
assert upgrade, (
    f"no `pip install -U 'kglite>='` upgrade hint found in {extension}; the "
    "kglite-load failure path must tell the user how to upgrade the wheel"
)
upgraded = sorted({v.decode().rstrip(".") for v in upgrade})
assert upgraded == [floor], (
    f"{extension} tells users to upgrade to kglite>={', '.join(upgraded)} but "
    f"pyproject.toml declares kglite>={floor}."
)

# ── version skew: a failing kglite.load() must name the wheel and the fix ──────
#
# ``build()`` hands the graph to the installed kglite wheel through an invisible
# temp ``.kgl`` the caller never wrote. A wheel older than the container format
# we write fails there with a bare FileFormatError. Simulate exactly that with a
# fake ``kglite`` module and assert the raised message is actionable: it must
# keep kglite's own words, name the installed wheel version, and give the pip
# command. Fixture-backed synthetic cache — no audio, no sonara.
fixture = repo_root / "sonagram/tests/fixtures/analyses/01-intro-ft-king-rell.json"

with tempfile.TemporaryDirectory() as tmp:
    library = Path(tmp)
    audio = library / "a.mp3"
    audio.write_bytes(b"ID3\x04\x00\x00\x00\x00\x00\x00not-decodable-audio")

    record = json.loads(fixture.read_text())
    record["source"].update(
        {
            "content_hash": "skew-hash",
            "path": "a.mp3",
            "file_size": audio.stat().st_size,
        }
    )
    cache = library / ".sonagram"
    analysis = cache / "analysis"
    analysis.mkdir(parents=True)
    (analysis / "skew-hash.json").write_text(json.dumps(record, indent=2) + "\n")
    (cache / "index.json").write_text(
        json.dumps(
            {
                "a.mp3": {
                    "size": audio.stat().st_size,
                    "mtime_unix": int(audio.stat().st_mtime),
                    "content_hash": "skew-hash",
                }
            },
            indent=2,
        )
        + "\n"
    )

    stale = types.ModuleType("kglite")
    stale.__version__ = "0.15.3"

    def _refuse(path):  # what a pre-0.16 wheel does with a v6 container
        raise ValueError("File uses .kgl container version 6, this build supports 5")

    stale.load = _refuse

    previous = sys.modules.get("kglite")
    sys.modules["kglite"] = stale
    try:
        sonagram.build(str(library))
        raise AssertionError("build() must fail when the kglite wheel cannot load our .kgl")
    except AssertionError:
        raise
    except Exception as exc:  # noqa: BLE001 — the message under test
        message = str(exc)
    finally:
        if previous is None:
            del sys.modules["kglite"]
        else:
            sys.modules["kglite"] = previous

assert "container version 6" in message, (
    f"kglite's own error text was dropped from the re-raise: {message!r}"
)
assert "0.15.3" in message, (
    f"the installed wheel version is missing, so the user cannot see the skew: {message!r}"
)
assert f"pip install -U 'kglite>={floor}'" in message, (
    f"the upgrade command is missing from the re-raise: {message!r}"
)

print("ok")
