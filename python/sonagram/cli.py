"""Console-script shim for the Rust ``sonagram`` CLI bundled in the wheel.

The CLI implementation lives in the ``sonagram`` core crate (``src/cli.rs``) and
is reached through the compiled ``_sonagram`` extension's ``_run_cli``. This
module only forwards ``sys.argv[1:]`` and turns the returned exit code into a
``SystemExit``; command parsing, output, and exit codes are shared with the
standalone ``sonagram`` binary (``cargo install`` / the release build), so the
two frontends cannot drift.
"""

from __future__ import annotations

import sys


def main(argv: list[str] | None = None) -> int:
    """Run the bundled Rust CLI with ``argv`` (else ``sys.argv[1:]``).

    Returns the CLI's process exit code (``0`` success, ``1`` error; ``status``
    additionally uses ``1`` = needs scan, ``2`` = no cache).
    """
    from sonagram._sonagram import _run_cli

    args = list(sys.argv[1:] if argv is None else argv)
    try:
        return _run_cli(args)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
