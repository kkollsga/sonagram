"""Console shim for Sonagram's typed KGLite MCP frontend."""

from __future__ import annotations

import json
import os
import sys


def main(argv: list[str] | None = None) -> int:
    """Serve the configured music graph over stdio until the client exits."""
    from sonagram._sonagram import _run_mcp_server

    args = list(sys.argv[1:] if argv is None else argv)
    # KGLite's --selftest must re-spawn this console frontend, not the Python
    # interpreter returned by current_exe(). Keep the command JSON-safe and
    # independent of PATH/aliases.
    os.environ["KGLITE_MCP_RESPAWN"] = json.dumps(
        [sys.executable, "-m", "sonagram.mcp_server"]
    )
    try:
        return _run_mcp_server(args)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
