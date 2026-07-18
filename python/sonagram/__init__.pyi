"""Type stubs for sonagram."""

from typing import Any, Callable, Optional

__version__: str

def scan(
    library_root: str,
    *,
    progress: Optional[Callable[[str, int, int], None]] = ...,
) -> dict[str, Any]: ...
def scan_and_enrich(
    library_root: str,
    *,
    api_key: Optional[str] = ...,
    progress: Optional[Callable[[str, int, int], None]] = ...,
) -> dict[str, Any]: ...
def enrich(
    library_root: str,
    *,
    api_key: Optional[str] = ...,
) -> dict[str, Any]: ...
def build(library_root: str, out_path: Optional[str] = ...) -> Any: ...
def scan_and_build(
    library_root: str,
    out_path: Optional[str] = ...,
    *,
    progress: Optional[Callable[[str, int, int], None]] = ...,
) -> Any: ...
def export_m3u(
    kgl_path: str,
    library_root: str,
    out_path: str,
    *,
    cypher: Optional[str] = ...,
    track_ids: Optional[list[str]] = ...,
    copy_to: Optional[str] = ...,
) -> str: ...
def profile_library(kgl_path: str) -> dict[str, Any]: ...
def curation_policy(preset: str) -> dict[str, Any]: ...
def curate_playlist(
    kgl_path: str,
    brief: dict[str, Any],
    policy: Optional[dict[str, Any]] = ...,
) -> dict[str, Any]: ...
def audit_playlist(
    kgl_path: str,
    track_ids: list[str],
    policy: Optional[dict[str, Any]] = ...,
    *,
    brief: Optional[dict[str, Any]] = ...,
) -> dict[str, Any]: ...
def explain_playlist(
    kgl_path: str,
    track_ids: list[str],
    policy: Optional[dict[str, Any]] = ...,
    *,
    brief: Optional[dict[str, Any]] = ...,
) -> dict[str, Any]: ...
def _run_cli(argv: list[str]) -> int: ...
