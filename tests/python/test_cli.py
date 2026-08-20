"""Console-script / CLI-shim test for the bundled Rust ``sonagram`` command.

Script style (no pytest), matching sonara's tests/python/ convention.

Two layers:
  1. The ``sonagram.cli:main`` shim exists, is importable, and returns the
     Rust CLI's exit codes in-process (``--version`` -> 0, ``status`` on a
     dir with no cache -> 2).
  2. If ``maturin develop`` installed the ``sonagram`` console script, drive
     it as a real subprocess and check ``--version`` output + the status exit
     code. If the script is absent, we say so and fall back to a subprocess
     that invokes ``sonagram.cli.main`` directly (same code path, minus the
     entry-point wiring).
"""

import os
import shutil
import subprocess
import sys
import tempfile

import sonagram
from sonagram.cli import main

VERSION = "0.2.5"

assert sonagram.__version__ == VERSION, (
    f"expected {VERSION}, got {sonagram.__version__!r}"
)
assert callable(main), "sonagram.cli.main must be callable"

# ── Layer 1: in-process exit codes (Rust prints to its own stdout) ──
rc = main(["--version"])
assert rc == 0, f"--version should exit 0, got {rc}"

rc = main(["--help"])
assert rc == 0, f"--help should exit 0, got {rc}"

with tempfile.TemporaryDirectory() as d:
    # No .sonagram/ cache under a fresh dir ⇒ "no cache" ⇒ exit 2.
    rc = main(["status", d])
    assert rc == 2, f"status on an uncached dir should exit 2, got {rc}"
    rc = main(["status", d, "--format", "json"])
    assert rc == 2, f"status --format json on an uncached dir should exit 2, got {rc}"

# `skill show` prints the embedded skill and exits 0 (P19 cold-start bootstrap).
rc = main(["skill", "show"])
assert rc == 0, f"skill show should exit 0, got {rc}"

# ── Layer 2: the real console script, if maturin installed it ──
def find_console_script():
    # Same-interpreter bin dir first (venv), then PATH.
    bindir = os.path.dirname(sys.executable)
    for name in ("sonagram", "sonagram.exe"):
        cand = os.path.join(bindir, name)
        if os.path.isfile(cand) and os.access(cand, os.X_OK):
            return [cand]
    found = shutil.which("sonagram")
    if found:
        return [found]
    return None


def run(cmd, *args):
    return subprocess.run(
        cmd + list(args), capture_output=True, text=True
    )


script = find_console_script()
if script is not None:
    how = f"console script {script[0]}"
else:
    # Fall back to invoking the shim as a module-level main() in a subprocess,
    # so we still exercise the full argv -> _run_cli path out-of-process.
    shim = (
        "import sys; from sonagram.cli import main; "
        "raise SystemExit(main(sys.argv[1:]))"
    )
    script = [sys.executable, "-c", shim]
    how = "cli.main subprocess (console script not installed by maturin develop)"

# --version prints "sonagram 0.2.5" to stdout and exits 0.
res = run(script, "--version")
assert res.returncode == 0, f"--version rc={res.returncode}; stderr={res.stderr!r}"
assert VERSION in res.stdout, f"--version stdout missing {VERSION}: {res.stdout!r}"

# `skill show` prints the embedded skill (non-empty, names itself).
res = run(script, "skill", "show")
assert res.returncode == 0, f"skill show rc={res.returncode}; stderr={res.stderr!r}"
assert "sonagram-playlist" in res.stdout, "skill show output names the skill"

# status on an uncached dir exits 2 with parseable JSON under --format json.
with tempfile.TemporaryDirectory() as d:
    res = run(script, "status", d, "--format", "json")
    assert res.returncode == 2, (
        f"status rc={res.returncode}; stdout={res.stdout!r}; stderr={res.stderr!r}"
    )
    import json

    obj = json.loads(res.stdout)
    assert obj["status"] == "no_cache", obj
    assert obj["exit_code"] == 2, obj
    assert obj["has_cache"] is False, obj

print(f"ok (subprocess path: {how})")
