"""Live kglite MCP gate for Sonagram's installed manifest and revealed skills."""

import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
from collections import deque
from pathlib import Path

import kglite
import sonagram


def stop_process(process):
    if process.poll() is None:
        process.kill()
    process.wait()
    for thread in process.reader_threads:
        thread.join(timeout=1)


def rpc(process, request_id, method, params, allow_error=False):
    payload = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
    process.request_trace.append(payload)
    process.stdin.write(json.dumps(payload) + "\n")
    process.stdin.flush()
    while True:
        try:
            response = process.responses.get(timeout=30)
        except queue.Empty as error:
            stop_process(process)
            raise AssertionError(
                f"MCP server timed out during {method}; "
                f"stdout: {process.raw_stdout_lines!r}; "
                f"stderr: {''.join(process.stderr_lines)}"
            ) from error
        if response is None:
            raise AssertionError(
                f"MCP server exited during {method}: {''.join(process.stderr_lines)}"
            )
        if response.get("id") == request_id:
            if allow_error:
                return response
            assert "error" not in response, response
            return response.get("result", {})


def notify(process, method):
    process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
    process.stdin.flush()


def inspect_server(graph_path, server, env=None, cwd=None):
    process = subprocess.Popen(
        [server, "--graph", str(graph_path)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
        cwd=cwd,
    )
    process.responses = queue.Queue()
    process.request_trace = []
    process.raw_stdout_lines = []
    process.stderr_lines = deque(maxlen=200)

    def read_responses():
        for line in process.stdout:
            process.raw_stdout_lines.append(line)
            try:
                process.responses.put(json.loads(line))
            except json.JSONDecodeError:
                continue
        process.responses.put(None)

    stdout_thread = threading.Thread(target=read_responses, daemon=True)
    stdout_thread.start()

    def read_stderr():
        # The boot summary can exceed the OS pipe capacity as tool descriptions
        # grow; drain it while the child boots or it can block before stdio MCP starts.
        process.stderr_lines.extend(process.stderr)

    stderr_thread = threading.Thread(target=read_stderr, daemon=True)
    stderr_thread.start()
    process.reader_threads = (stdout_thread, stderr_thread)
    try:
        process.instructions = rpc(
            process,
            1,
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "sonagram-test", "version": "1"},
            },
        ).get("instructions", "")
        notify(process, "notifications/initialized")
        tools = rpc(process, 2, "tools/list", {}).get("tools", [])
        prompts = rpc(process, 3, "prompts/list", {}).get("prompts", [])
        return process, tools, prompts
    except Exception:
        stop_process(process)
        raise


def tool_text(result):
    return "\n".join(part.get("text", "") for part in result.get("content", []))


def tool_payload(result):
    return json.loads(tool_text(result))


def track_count(process, request_id):
    text = tool_text(rpc(
        process,
        request_id,
        "tools/call",
        {
            "name": "cypher_query",
            "arguments": {"query": "MATCH (t:Track) RETURN count(t) AS tracks"},
        },
    ))
    header, rows = text.split("tracks\n", 1)
    assert header.startswith("1 row(s):"), text
    return int(rows.splitlines()[0])


def scalar_count(process, request_id, predicate):
    """`MATCH (t:Track) WHERE <predicate> RETURN count(t)`, as an int."""
    text = tool_text(rpc(
        process,
        request_id,
        "tools/call",
        {
            "name": "cypher_query",
            "arguments": {
                "query": f"MATCH (t:Track) WHERE {predicate} "
                         "RETURN count(t) AS matched"
            },
        },
    ))
    header, rows = text.split("matched\n", 1)
    assert header.startswith("1 row(s):"), text
    return int(rows.splitlines()[0])


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
    expected_hashes = sorted(entry["content_hash"] for entry in index.values())

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
    # KGLite 0.16.20 made `extensions.writable: true` the same statement the
    # flag makes, and the installed manifest is operator-editable: `sonagram mcp
    # install` never overwrites a differing one. `cypher_query` is on the
    # allow-list, so an honoured key would accept mutations into a graph the
    # next `sonagram build` regenerates and discards. Since KGLite 0.16.21 the
    # binary pins the engine read-only (`ServerExtensions::read_only`) instead
    # of grepping the manifest for the key, so the key is inert rather than
    # fatal — the server boots and refuses the mutation. That refusal is the
    # guarantee, and this is the only place it can be observed: the pin is not
    # readable from our own process.
    manifest_path = root / "music_mcp.yaml"
    clean_manifest = manifest_path.read_text()
    manifest_path.write_text(
        clean_manifest.replace("extensions:\n", "extensions:\n  writable: true\n", 1)
    )
    optin_process, optin_tools, _ = inspect_server(graph_path, server_path, env=env)
    try:
        assert "cypher_query" in {tool["name"] for tool in optin_tools}
        before = track_count(optin_process, 4)
        assert before > 0, "control: the opt-in boot must serve the fixture graph"
        mutation = rpc(
            optin_process,
            5,
            "tools/call",
            {
                "name": "cypher_query",
                "arguments": {
                    "query": "MATCH (t:Track) SET t.title = 'mutated' RETURN count(t)"
                },
            },
            allow_error=True,
        )
        rendered = json.dumps(mutation)
        assert (
            "read-only" in rendered
            or "read only" in rendered
            or "not enabled" in rendered
            or "writable" in rendered
        ), rendered
        # ...and the graph is untouched. The refusal message alone is not the
        # guarantee: a mutation refused with a confusing message but applied
        # anyway passes the check above and fails these two.
        assert track_count(optin_process, 6) == before
        assert scalar_count(optin_process, 7, "t.title = 'mutated'") == 0
    finally:
        stop_process(optin_process)
    manifest_path.write_text(clean_manifest)
    selftest = subprocess.run(
        [server_path, "--graph", str(graph_path), "--selftest"],
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    assert "Selftest PASSED" in selftest.stdout
    # KGLite 0.17.1 reports how many skills the session actually serves.
    # `skills: true` is the bundled marker, so the auto-detected project layer
    # is `music_mcp.skills/` beside the YAML — the directory `sonagram mcp
    # install` writes in the same operation as the manifest. Keyed to the wrong
    # basename it resolves nothing and every earlier release still printed
    # PASSED; the count is what makes that visible, so assert it rather than
    # the word PASSED alone.
    skills_line = next(
        (line for line in selftest.stdout.splitlines() if "skills" in line), None
    )
    assert skills_line is not None, selftest.stdout
    assert "0 served" not in skills_line, skills_line
    for name in (
        "music_library_profile",
        "music_curation_policy",
        "music_playlist_audit",
        "music_playlist_store",
    ):
        assert name in skills_line, (name, skills_line)
    # The fifth, `music_song_versions`, is gated on `graph_has_node_type:
    # [Song]` and is correctly withheld here: Song nodes exist only for grouped
    # versions and the fixture library has no duplicate recordings. Asserting
    # its absence keeps the four above from being read as "all five" — and if a
    # fixture ever gains a duplicate, this line names why it changed.
    assert "music_song_versions" not in skills_line, skills_line
    # Boot the live server with credentials the music deployment must never
    # forward: mcp_server::run scrubs them, so kglite's github builtin stays off
    # and no github_* route can reach the surface asserted below.
    hostile_env = {**env, "GITHUB_TOKEN": "dummy", "GH_TOKEN": "dummy"}
    process, tools, prompts = inspect_server(graph_path, server_path, env=hostile_env)
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
        # The manifest's tools_allow is a closed surface, and an unmatched entry
        # is a silent no-op upstream: exact equality is the only thing that
        # catches a typo'd or dropped name, so subset checks are not enough.
        expected_tools = {
            "ping",
            "cypher_query",
            "expand_response",
            "graph_overview",
            "reload_graph",
            *domain_tools,
        }
        assert set(by_name) == expected_tools, (
            f"served tool surface drifted: unexpected="
            f"{sorted(set(by_name) - expected_tools)} "
            f"missing={sorted(expected_tools - set(by_name))}"
        )
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
        # do not repeat several thousand characters on every generic reveal.
        # The 8,192-character ceiling includes KGLite's standard response-control
        # help; it still fails if a second multi-thousand-character music
        # methodology is appended.
        cypher_description = by_name["cypher_query"].get("description", "")
        overview_description = by_name["graph_overview"].get("description", "")
        assert len(cypher_description) < 8192, len(cypher_description)
        assert len(overview_description) < 8192, len(overview_description)
        # The manifest's description overrides speak in the music voice and
        # kglite appends its own skill body after them, so the override is a
        # prefix. Losing it means the agent meets a generic graph tool instead.
        cypher_prefix = "Read-only Cypher over the Sonagram music graph:"
        overview_prefix = "Inventory of the Sonagram music graph:"
        assert cypher_description.startswith(cypher_prefix), cypher_description[:200]
        assert overview_description.startswith(overview_prefix), (
            overview_description[:200]
        )
        # AGENT-GUIDE.md enumerates `cypher_query`'s arguments for the agent
        # reading it, and kglite owns that schema. Nothing checked the two
        # against each other, and both drifted: the guide said the tool takes
        # "a single `query` string and nothing else" while the server had
        # accepted `params` since at least 0.16.22, and 0.17.1 then added
        # `timeout_ms`. An agent that believes the guide inlines every literal
        # by hand and never sets a deadline. Pin the served property set and
        # require the guide to name each member, so the next argument kglite
        # adds turns this red instead of quietly falsifying the manual.
        cypher_args = set(
            by_name["cypher_query"].get("inputSchema", {}).get("properties", {})
        )
        assert cypher_args == {"query", "params", "timeout_ms", "_response"}, (
            f"cypher_query's argument set moved: {sorted(cypher_args)}. "
            "Update AGENT-GUIDE.md and docs/agent-guide.md in the same change."
        )
        guide = (repo / "AGENT-GUIDE.md").read_text()
        for argument in sorted(cypher_args):
            assert f"`{argument}`" in guide, (
                f"AGENT-GUIDE.md never names cypher_query's `{argument}` argument"
            )
        response_schema = by_name["cypher_query"]["inputSchema"]["properties"]["_response"]
        minimum = response_schema["properties"]["max_bytes"]["minimum"]
        assert isinstance(minimum, int) and minimum > 0
        trace_start = len(process.request_trace)
        probe = rpc(
            process,
            29,
            "tools/call",
            {
                "name": "cypher_query",
                "arguments": {
                    "query": "MATCH (t:Track) RETURN t "
                             "ORDER BY t.content_hash LIMIT 15",
                    "_response": {"max_bytes": minimum},
                },
            },
        )
        # The framework budgets the MCP result object, excluding JSON-RPC
        # framing. Compact UTF-8 reserialization preserves that measurement.
        serialized = json.dumps(
            probe, ensure_ascii=False, separators=(",", ":")
        ).encode("utf-8")
        assert len(serialized) <= minimum, (len(serialized), minimum)
        preview = tool_payload(probe)["response_budget"]
        assert preview["complete"] is False
        assert preview["original_bytes"] > minimum
        assert expected_hashes[-1] not in serialized.decode("utf-8")
        assert preview["coverage"] != ""
        coverage = next(
            field["value"]["query_and_executor"]
            for field in preview["domain_guidance"]["fields"]
            if field.get("location") == "/coverage"
        )
        assert coverage["executed_rows"] == 15
        assert coverage["engine_row_limit"] is None
        assert coverage["query_literal_limits"] == [15]
        action = preview["next"]["selected_value"]
        expanded = rpc(process, 31, "tools/call", action)
        assert action["name"] in by_name
        assert action["arguments"]["result_id"] == preview["result_id"]
        assert action["arguments"]["response"] == {"mode": "full"}
        expanded_hashes = [
            row[0]["properties"]["content_hash"]
            for row in expanded["structuredContent"]["rows"]
        ]
        assert expanded_hashes == expected_hashes
        trace = process.request_trace[trace_start:]
        assert [entry["method"] for entry in trace] == ["tools/call", "tools/call"]
        assert [entry["params"]["name"] for entry in trace] == [
            "cypher_query",
            action["name"],
        ]
        assert trace[1]["params"]["arguments"] == action["arguments"]

        missing = rpc(
            process,
            32,
            "tools/call",
            {
                "name": "cypher_query",
                "arguments": {
                    "query": "MATCH (t:Track) WHERE t.title = $missing "
                             "RETURN count(t) AS matched LIMIT 1"
                },
            },
            allow_error=True,
        )
        missing_text = json.dumps(missing).lower()
        assert missing["result"]["isError"] is True, missing
        assert "missing parameter: $missing" in missing_text, missing
        # The schema advertising `params` is not the same claim as binding
        # working through *our* manifest's tools_allow surface, and the guide
        # now tells agents to use it. Bind one and prove the value reached the
        # query: a server that ignored `params` would answer the unfiltered
        # count here, and one that refused the key would error.
        bound = tool_text(rpc(
            process,
            30,
            "tools/call",
            {
                "name": "cypher_query",
                "arguments": {
                    "query": "MATCH (t:Track) WHERE t.energy > $floor "
                             "RETURN count(t) AS matched",
                    "params": {"floor": 2.0},
                },
            },
        ))
        header, rows = bound.split("matched\n", 1)
        assert header.startswith("1 row(s):"), bound
        # `energy` is a 0..1 axis, so a floor of 2.0 matches nothing. The value
        # had to arrive for that to be the answer.
        assert int(rows.splitlines()[0]) == 0, bound

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
        stop_process(process)

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
        stop_process(process)

    # A rescan rewrites the served .kgl underneath a running server. Two
    # routes must reach the live query path: the explicit reload_graph tool
    # (Desktop-callable), and — since KGLite 0.16.19 — the unconditional
    # per-call re-read of a --graph file whose identity changed, which is what
    # replaced our former `graph_watch: true` manifest opt-in.
    served_path = root / "served.kgl"
    shutil.copyfile(graph_path, served_path)
    shutil.copyfile(root / "music_mcp.yaml", root / "served_mcp.yaml")
    shutil.copytree(root / "music_mcp.skills", root / "served_mcp.skills")
    process, tools, _ = inspect_server(served_path, server_path, env=env)
    try:
        assert "reload_graph" in {tool["name"] for tool in tools}
        booted_tracks = track_count(process, 30)
        assert booted_tracks == 15, booted_tracks
        reloaded = tool_text(rpc(
            process, 31, "tools/call", {"name": "reload_graph", "arguments": {}}
        ))
        assert reloaded.startswith("Reloaded "), reloaded
        unchanged_tracks = track_count(process, 32)
        assert unchanged_tracks == 15, unchanged_tracks
        # Swap the served file for the one-Track generic graph: the reload has
        # to be what changes the answer.
        shutil.copyfile(other_path, served_path)
        swapped = tool_text(rpc(
            process, 33, "tools/call", {"name": "reload_graph", "arguments": {}}
        ))
        assert "1 nodes" in swapped, swapped
        swapped_tracks = track_count(process, 34)
        assert swapped_tracks == 1, (
            f"reload did not reach the query path: {swapped_tracks}"
        )
        # Swap back to the 15-Track graph and ask WITHOUT reload_graph: the
        # server must notice the file's identity moved and re-read it before
        # answering. This is the engine guarantee the manifest now relies on
        # instead of a graph_watch key.
        shutil.copyfile(graph_path, served_path)
        refreshed_tracks = track_count(process, 35)
        assert refreshed_tracks == 15, (
            f"automatic re-read did not reach the query path: {refreshed_tracks}"
        )
    finally:
        stop_process(process)

    # The installed manifest declares its source sandbox RELATIVELY
    # (`source_root: ./.sonagram-mcp-public`), and a desktop client starts this
    # server from whatever working directory it happens to hold. KGLite 0.17.5
    # resolves that path from the manifest's own directory; before it, the same
    # manifest resolved against the launching process's cwd, so the sandbox
    # `sonagram mcp install` created was reachable only when the client's cwd
    # happened to be the graph directory.
    #
    # The observable is `initialize`'s instructions: an unresolved declared
    # source root appends a NOTE naming the path it tried. Both directions are
    # asserted, because either one alone passes under cwd-relative resolution
    # for the wrong reason.
    unresolved_note = "no declared source root resolved"
    elsewhere = root / "elsewhere"
    elsewhere.mkdir()
    process, _, _ = inspect_server(graph_path, server_path, env=env, cwd=str(elsewhere))
    try:
        assert unresolved_note not in process.instructions, (
            "manifest-relative sandbox did not resolve when the server started "
            f"from another directory: {process.instructions[-400:]}"
        )
    finally:
        stop_process(process)

    # The inverse, which is what makes the assertion above non-vacuous: put the
    # sandbox at the server's cwd and NOT beside the manifest. Manifest-relative
    # resolution must fail here; cwd-relative resolution would succeed.
    decoy_cwd = root / "decoy-cwd"
    (decoy_cwd / ".sonagram-mcp-public").mkdir(parents=True)
    sandboxless = root / "sandboxless"
    sandboxless.mkdir()
    shutil.copyfile(graph_path, sandboxless / "music.kgl")
    shutil.copyfile(root / "music_mcp.yaml", sandboxless / "music_mcp.yaml")
    shutil.copyfile(root / "sonagram_mcp.env", sandboxless / "sonagram_mcp.env")
    shutil.copytree(root / "music_mcp.skills", sandboxless / "music_mcp.skills")
    process, _, _ = inspect_server(
        sandboxless / "music.kgl", server_path, env=env, cwd=str(decoy_cwd)
    )
    try:
        assert unresolved_note in process.instructions, (
            "a sandbox present only at the server's cwd resolved the manifest's "
            "relative source_root; resolution must be manifest-relative: "
            f"{process.instructions[-400:]}"
        )
    finally:
        stop_process(process)


print("ok")
