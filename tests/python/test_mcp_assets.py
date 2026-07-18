"""Live kglite MCP gate for Sonagram's installed manifest and revealed skills."""

import json
import os
import queue
import shutil
import subprocess
import tempfile
import threading
from pathlib import Path

import kglite
import sonagram


def rpc(process, request_id, method, params):
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
            assert "error" not in response, response
            return response.get("result", {})


def notify(process, method):
    process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
    process.stdin.flush()


def inspect_server(graph_path, server):
    process = subprocess.Popen(
        [server, "--graph", str(graph_path)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
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


repo = Path(__file__).resolve().parents[2]
fixtures = repo / "sonagram" / "tests" / "fixtures" / "analyses"
binary = os.environ.get("SONAGRAM_TEST_BIN") or shutil.which("sonagram")
assert binary, "sonagram console script is required"

with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    library = root / "library"
    analysis = library / ".sonagram" / "analysis"
    analysis.mkdir(parents=True)
    for source in sorted(fixtures.glob("*.json")):
        data = json.loads(source.read_text())
        shutil.copyfile(source, analysis / f"{data['source']['content_hash']}.json")

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
    assert "changed:   6" in first.stdout
    assert "changed:   0" in second.stdout
    assert "unchanged: 6" in second.stdout
    assert (root / "music_mcp.yaml").exists()
    assert len(list((root / "music_mcp.skills").glob("*.md"))) == 5
    assert (root / ".sonagram-mcp-public").is_dir()
    assert not any((root / ".sonagram-mcp-public").iterdir())
    server_line = next(
        line for line in first.stdout.splitlines() if line.strip().startswith("server:")
    )
    server_path = Path(server_line.split("server:", 1)[1].strip())
    assert server_path.is_absolute() and server_path.is_file()
    secret = "LASTFM_API_KEY=must-not-be-readable"
    (root / ".env").write_text(secret)

    process, tools, prompts = inspect_server(graph_path, server_path)
    try:
        by_name = {tool["name"]: tool for tool in tools}
        assert "music_library_profile" in by_name
        # KGLite 0.14.0 registers source tools after manifest overrides, so its
        # documented `hidden: true` currently does not remove these routes.
        # Sonagram's security boundary is therefore the validated empty source
        # sandbox. Accept a future KGLite that hides them; on 0.14.0 prove a
        # traversal cannot reach the graph-adjacent .env sentinel.
        if "read_source" in by_name:
            blocked = rpc(
                process,
                20,
                "tools/call",
                {"name": "read_source", "arguments": {"file_path": "../.env"}},
            )
            blocked_text = "\n".join(
                part.get("text", "") for part in blocked.get("content", [])
            )
            assert secret not in blocked_text, blocked_text
        if "grep" in by_name:
            searched = rpc(
                process,
                21,
                "tools/call",
                {"name": "grep", "arguments": {"pattern": "LASTFM_API_KEY"}},
            )
            searched_text = "\n".join(
                part.get("text", "") for part in searched.get("content", [])
            )
            assert secret not in searched_text, searched_text
        if "list_source" in by_name:
            listed = rpc(
                process,
                22,
                "tools/call",
                {"name": "list_source", "arguments": {}},
            )
            listed_text = "\n".join(
                part.get("text", "") for part in listed.get("content", [])
            )
            assert ".env" not in listed_text, listed_text
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
        profile = rpc(
            process,
            4,
            "tools/call",
            {"name": "music_library_profile", "arguments": {}},
        )
        text = "\n".join(part.get("text", "") for part in profile.get("content", []))
        assert "tracks" in text and "music_tracks" in text, text
        assert "energy_present" in text and "arousal_present" in text, text
    finally:
        process.kill()
        process.wait()

    # The same manifest beside a non-music graph keeps every Sonagram skill
    # gated off; the declarative profile route remains registered but carries
    # no music methodology in its description.
    other_path = root / "other.kgl"
    kglite.from_records(
        {
            "nodes": [
                {
                    "type": "Person",
                    "id_field": "id",
                    "title_field": "name",
                    "records": [{"id": 1, "name": "Alice"}],
                }
            ]
        },
        save=str(other_path),
    )
    shutil.copyfile(root / "music_mcp.yaml", root / "other_mcp.yaml")
    shutil.copytree(root / "music_mcp.skills", root / "other_mcp.skills")
    process, tools, prompts = inspect_server(other_path, server_path)
    try:
        by_name = {tool["name"]: tool for tool in tools}
        assert "sonagram-curation-contract:v1" not in by_name[
            "music_library_profile"
        ].get("description", "")
        prompt_names = {prompt["name"] for prompt in prompts}
        assert not any(name.startswith("music_") for name in prompt_names)
    finally:
        process.kill()
        process.wait()

print("ok")
