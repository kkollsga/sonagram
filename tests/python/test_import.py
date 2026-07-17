"""Smoke test: the compiled wheel imports and reports the bootstrap version.

Script style (no pytest), matching sonara's tests/python/ convention.
"""

import sonagram

assert sonagram.__version__ == "0.2.0", (
    f"expected 0.2.0, got {sonagram.__version__!r}"
)
print("ok")
