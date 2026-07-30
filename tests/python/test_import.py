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

import re
from pathlib import Path

import sonagram
from sonagram import _sonagram

assert sonagram.__version__ == "0.2.2", (
    f"expected 0.2.2, got {sonagram.__version__!r}"
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

print("ok")
