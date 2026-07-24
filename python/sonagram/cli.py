"""Console-script shim for the Rust ``sonagram`` CLI bundled in the wheel.

The CLI implementation lives in the ``sonagram`` core crate (``src/cli.rs``) and
is reached through the compiled ``_sonagram`` extension's ``_run_cli``. This
module only forwards ``sys.argv[1:]`` and turns the returned exit code into a
``SystemExit``; command parsing, output, and exit codes are shared with the
standalone ``sonagram`` binary (``cargo install`` / the release build), so the
two frontends cannot drift.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path


_MCP_SERVER_ENV = "SONAGRAM_MCP_SERVER"


def main(argv: list[str] | None = None) -> int:
    """Run the bundled Rust CLI with ``argv`` (else ``sys.argv[1:]``).

    Returns the CLI's process exit code (``0`` success, ``1`` error; ``status``
    additionally uses ``1`` = needs scan, ``2`` = no cache).
    """
    from sonagram._sonagram import _run_cli

    args = list(sys.argv[1:] if argv is None else argv)
    # Rust's current_exe() resolves a venv's Python symlink to the base
    # interpreter on some platforms. Pass the console-script sibling explicitly
    # so `sonagram mcp install` can report a launchable MCP server even when the
    # venv is not represented by current_exe() (notably GitHub-hosted runners).
    server = Path(sys.executable).with_name(
        f"sonagram-mcp-server{'.exe' if os.name == 'nt' else ''}"
    )
    prior_server = os.environ.get(_MCP_SERVER_ENV)
    if server.is_file():
        os.environ[_MCP_SERVER_ENV] = str(server)
    try:
        return _run_cli(args)
    except KeyboardInterrupt:
        return 130
    finally:
        if server.is_file():
            if prior_server is None:
                os.environ.pop(_MCP_SERVER_ENV, None)
            else:
                os.environ[_MCP_SERVER_ENV] = prior_server


if __name__ == "__main__":
    raise SystemExit(main())
