"""Live kglite MCP gate for Sonagram's installed manifest and revealed skills."""

import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

import kglite
import sonagram


def rpc(process, request_id, method, params, allow_error=False):
    payload = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
    process.stdin.write(json.dumps(payload) + "\n")
    process.stdin.flush()
    while True:
        try:
            response = process.responses.get(timeout=30)
        except queue.Empty as error:
            process.kill()
            process.wait()
            raise AssertionError(f"MCP server timed out during {method}") from error
        if response is None:
            stderr = process.stderr.read()
            raise AssertionError(f"MCP server exited during {method}: {stderr}")
        if response.get("id") == request_id:
            if allow_error:
                return response
            assert "error" not in response, response
            return response.get("result", {})


def notify(process, method):
    process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
    process.stdin.flush()


def inspect_server(graph_path, server, env=None):
    process = subprocess.Popen(
        [server, "--graph", str(graph_path)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
    )
    process.responses = queue.Queue()

    def read_responses():
        for line in process.stdout:
            try:
                process.responses.put(json.loads(line))
            except json.JSONDecodeError:
                continue
        process.responses.put(None)

    threading.Thread(target=read_responses, daemon=True).start()
    try:
        rpc(
            process,
            1,
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "sonagram-test", "version": "1"},
            },
        )
        notify(process, "notifications/initialized")
        tools = rpc(process, 2, "tools/list", {}).get("tools", [])
        prompts = rpc(process, 3, "prompts/list", {}).get("prompts", [])
        return process, tools, prompts
    except Exception:
        process.kill()
        process.wait()
        raise


def tool_payload(result):
    text = "\n".join(part.get("text", "") for part in result.get("content", []))
    return json.loads(text)


repo = Path(__file__).resolve().parents[2]
fixtures = repo / "sonagram" / "tests" / "fixtures" / "analyses"
venv_binary = Path(sys.executable).with_name("sonagram")
binary = (
    os.environ.get("SONAGRAM_TEST_BIN")
    or (str(venv_binary) if venv_binary.is_file() else None)
    or shutil.which("sonagram")
)
assert binary, "sonagram console script is required"

with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    library = root / "library"
    analysis = library / ".sonagram" / "analysis"
    analysis.mkdir(parents=True)
    index = {}
    for source in sorted(fixtures.glob("*.json")):
        data = json.loads(source.read_text())
        content_hash = data["source"]["content_hash"]
        provenance = data["analysis"]["provenance"]
        provenance["schema_version"] = 6
        provenance["requested_features"] = sorted(
            set(provenance["requested_features"] + ["aggression"])
        )
        provenance["vocalness_model_id"] = "sonara-vocalness-v2"
        provenance["aggression_model_id"] = "aggression-rank-v3-sr22050"
        data["analysis"].update(
            aggression_score=0.63,
            aggression_confidence=0.91,
            aggression_forcefulness=0.74,
            aggression_harshness=0.52,
            aggression_tension=0.67,
            aggression_rhythm=0.58,
        )
        (analysis / f"{content_hash}.json").write_text(json.dumps(data, indent=2) + "\n")
        index[data["source"]["path"]] = {
            "size": data["source"]["file_size"],
            "mtime_unix": 0,
            "content_hash": content_hash,
        }
    (library / ".sonagram" / "index.json").write_text(
        json.dumps(index, indent=2, sort_keys=True) + "\n"
    )

    graph_path = root / "music.kgl"
    sonagram.build(str(library), str(graph_path))
    home = root / "home"
    env = {**os.environ, "SONAGRAM_HOME": str(home)}
    subprocess.run(
        [binary, "config", "set", "graph", str(graph_path)],
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    first = subprocess.run(
        [binary, "mcp", "install"],
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    second = subprocess.run(
        [binary, "mcp", "install"],
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    # The six managed assets (manifest + five skills) carry the counters; the
    # env file the manifest pins is operator-owned and reported on its own line.
    assert "changed:   6" in first.stdout
    assert "changed:   0" in second.stdout
    assert "unchanged: 6" in second.stdout
    env_file = root / "sonagram_mcp.env"
    # Install reports the canonical path (macOS /var -> /private/var).
    reported_env = root.resolve() / "sonagram_mcp.env"
    assert f"env:       {reported_env} (created)" in first.stdout
    assert f"env:       {reported_env} (kept)" in second.stdout
    assert env_file.is_file()
    operator_env = env_file.read_text() + "LASTFM_API_KEY=operator-owned\n"
    env_file.write_text(operator_env)
    forced = subprocess.run(
        [binary, "mcp", "install", "--force"],
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    assert f"env:       {reported_env} (kept)" in forced.stdout
    assert env_file.read_text() == operator_env
    assert (root / "music_mcp.yaml").exists()
    assert len(list((root / "music_mcp.skills").glob("*.md"))) == 5
    assert (root / ".sonagram-mcp-public").is_dir()
    assert not any((root / ".sonagram-mcp-public").iterdir())
    server_line = next(
        line for line in first.stdout.splitlines() if line.strip().startswith("server:")
    )
    server_path = Path(server_line.split("server:", 1)[1].strip())
    assert server_path.is_absolute() and server_path.is_file()
    writable = subprocess.run(
        [server_path, "--graph", str(graph_path), "--writable"],
        env=env,
        capture_output=True,
        text=True,
    )
    assert writable.returncode != 0
    assert "read-only" in writable.stderr
    selftest = subprocess.run(
        [server_path, "--graph", str(graph_path), "--selftest"],
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    assert "Selftest PASSED" in selftest.stdout
    process, tools, prompts = inspect_server(graph_path, server_path, env=env)
    try:
        by_name = {tool["name"]: tool for tool in tools}
        domain_tools = {
            "music_library_profile",
            "music_curation_policy",
            "music_curate_playlist",
            "music_audit_playlist",
            "music_explain_playlist",
            "music_playlists_list",
            "music_playlist_show",
            "music_playlist_update",
            "music_playlist_delete",
        }
        assert domain_tools.issubset(by_name)
        policy_schema = json.dumps(
            by_name["music_curate_playlist"].get("inputSchema", {}),
            sort_keys=True,
        )
        for field in (
            "min_aggression",
            "max_aggression",
            "aggression",
            "relative_aggression",
            "relative_aggression_margin",
        ):
            assert field in policy_schema, field
        # KGLite 0.14.2+ applies manifest hidden overrides after every route is
        # registered. The empty source sandbox remains defense in depth, while
        # discovery and direct-call rejection are now strict contracts.
        for request_id, hidden_name in enumerate(
            ("read_source", "grep", "list_source"), start=20
        ):
            assert hidden_name not in by_name
            rejected = rpc(
                process,
                request_id,
                "tools/call",
                {"name": hidden_name, "arguments": {}},
                allow_error=True,
            )
            assert "error" in rejected, rejected
        description = by_name["music_library_profile"].get("description", "")
        assert "sonagram-curation-contract:v1" in description
        # Playlist methodology is routed through the dedicated profile tool;
        # do not repeat several KB on every generic Cypher/overview reveal.
        assert len(by_name["cypher_query"].get("description", "")) < 8000
        assert len(by_name["graph_overview"].get("description", "")) < 8000
        prompt_names = {prompt["name"] for prompt in prompts}
        assert {
            "music_library_profile",
            "music_curation_policy",
            "music_playlist_audit",
            "music_playlist_store",
        }.issubset(prompt_names)
        profile = tool_payload(rpc(
            process,
            4,
            "tools/call",
            {"name": "music_library_profile", "arguments": {}},
        ))
        assert profile["ok"] is True
        assert profile["result"]["tracks"] == 15
        assert profile["result"]["stats"]["energy"]["present"] > 0
        assert "aggression" in profile["result"]["stats"]
        assert "aggression_confidence" in profile["result"]["stats"]
        assert "aggression_models" in profile["result"]

        policy = tool_payload(rpc(
            process,
            5,
            "tools/call",
            {"name": "music_curation_policy", "arguments": {"preset": "general"}},
        ))
        assert policy["ok"] is True
        assert policy["result"]["version"] == 1
        assert policy["result"]["targets"]["aggression"] is None
        assert policy["result"]["targets"]["relative_aggression"] == "any"
        assert policy["result"]["eligibility"]["min_aggression"] is None

        brief = {"preset": "general", "target_tracks": 5}
        curate_args = {"brief": brief}
        curated = tool_payload(rpc(
            process,
            6,
            "tools/call",
            {"name": "music_curate_playlist", "arguments": curate_args},
        ))
        repeated = tool_payload(rpc(
            process,
            7,
            "tools/call",
            {"name": "music_curate_playlist", "arguments": curate_args},
        ))
        assert curated == repeated
        assert curated["ok"] is True
        result = curated["result"]["curated"]
        assert result["exportable"] and result["audit"]["passed"]
        track_ids = result["track_ids"]

        audited = tool_payload(rpc(
            process,
            8,
            "tools/call",
            {
                "name": "music_audit_playlist",
                "arguments": {"track_ids": track_ids, "brief": brief},
            },
        ))
        assert audited["ok"] is True
        assert audited["result"] == result["audit"]

        explained = tool_payload(rpc(
            process,
            9,
            "tools/call",
            {
                "name": "music_explain_playlist",
                "arguments": {"track_ids": track_ids, "brief": brief},
            },
        ))
        assert explained["ok"] is True
        assert len(explained["result"]["tracks"]) == len(track_ids)

        stored = tool_payload(rpc(
            process,
            10,
            "tools/call",
            {
                "name": "music_curate_playlist",
                "arguments": {
                    "brief": brief,
                    "store": {"name": "MCP Focus", "description": "typed MCP gate"},
                },
            },
        ))
        assert stored["ok"] is True
        stored_paths = stored["result"]["stored"]
        assert stored_paths and Path(stored_paths["m3u8_path"]).is_file()
        assert Path(stored_paths["meta_path"]).is_file()
        assert Path(stored_paths["m3u8_path"]).parent == home / "playlists"
        stored_meta = json.loads(Path(stored_paths["meta_path"]).read_text())
        assert Path(stored_meta["graph"]).resolve() == graph_path.resolve()
        slug = stored_paths["slug"]

        listed = tool_payload(rpc(
            process, 11, "tools/call", {"name": "music_playlists_list", "arguments": {}}
        ))
        assert listed["ok"] is True and listed["result"][0]["slug"] == slug
        shown = tool_payload(rpc(
            process,
            12,
            "tools/call",
            {"name": "music_playlist_show", "arguments": {"slug": slug}},
        ))
        assert shown["ok"] is True and shown["result"]["request"] == "typed MCP gate"
        updated = tool_payload(rpc(
            process,
            13,
            "tools/call",
            {
                "name": "music_playlist_update",
                "arguments": {"slug": slug, "description": "updated by MCP"},
            },
        ))
        assert updated["ok"] is True and updated["result"]["request"] == "updated by MCP"
        rejected_delete = tool_payload(rpc(
            process,
            14,
            "tools/call",
            {
                "name": "music_playlist_delete",
                "arguments": {"slug": slug, "confirm_slug": "wrong"},
            },
        ))
        assert rejected_delete["ok"] is False
        assert Path(stored_paths["m3u8_path"]).exists()
        deleted = tool_payload(rpc(
            process,
            15,
            "tools/call",
            {
                "name": "music_playlist_delete",
                "arguments": {"slug": slug, "confirm_slug": slug},
            },
        ))
        assert deleted["ok"] is True and deleted["result"]["deleted"] is True
        assert not Path(stored_paths["m3u8_path"]).exists()
    finally:
        process.kill()
        process.wait()

    # The same manifest beside a generic graph keeps every Sonagram capability
    # gated off, even if it uses the conventional Track.content_hash shape.
    other_path = root / "other.kgl"
    kglite.from_records(
        {
            "nodes": [
                {
                    "type": "Track",
                    "id_field": "content_hash",
                    "title_field": "title",
                    "records": [{"content_hash": "generic", "title": "Not Sonagram"}],
                }
            ]
        },
        save=str(other_path),
    )
    shutil.copyfile(root / "music_mcp.yaml", root / "other_mcp.yaml")
    shutil.copytree(root / "music_mcp.skills", root / "other_mcp.skills")
    process, tools, prompts = inspect_server(other_path, server_path, env=env)
    try:
        by_name = {tool["name"]: tool for tool in tools}
        assert not any(name.startswith("music_") for name in by_name)
        prompt_names = {prompt["name"] for prompt in prompts}
        assert not any(name.startswith("music_") for name in prompt_names)
    finally:
        process.kill()
        process.wait()

print("ok")
