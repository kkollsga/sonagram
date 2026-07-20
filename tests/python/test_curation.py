"""No-audio Python parity gate for the Rust curation engine."""

import json
import shutil
import tempfile
from pathlib import Path

import sonagram


repo = Path(__file__).resolve().parents[2]
fixtures = repo / "sonagram" / "tests" / "fixtures" / "analyses"

with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    library = root / "library"
    analysis = library / ".sonagram" / "analysis"
    analysis.mkdir(parents=True)
    index = {}
    for source in sorted(fixtures.glob("*.json")):
        data = json.loads(source.read_text())
        content_hash = data["source"]["content_hash"]
        shutil.copyfile(source, analysis / f"{content_hash}.json")
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

    profile = sonagram.profile_library(str(graph_path))
    assert profile["tracks"] >= 12
    assert profile["stats"]["energy"]["present"] > 0

    focus_policy = sonagram.curation_policy("focus")
    assert focus_policy["preset"] == "focus"
    assert focus_policy["version"] == 1
    assert focus_policy["targets"]["seed_similarity"] == "neutral"
    assert focus_policy["eligibility"]["include_genres"] == []
    try:
        sonagram.curation_policy("not-a-preset")
    except ValueError:
        pass
    else:
        raise AssertionError("unknown preset must raise ValueError")

    brief = {
        "preset": "general",
        "target_tracks": 3,
        "target_duration_sec": None,
        "seed_ids": [],
    }
    initial = sonagram.curate_playlist(str(graph_path), brief)
    policy = initial["policy"]
    policy["eligibility"]["allow_low_quality"] = True
    policy["diversity"]["max_per_artist"] = 10
    policy["diversity"]["max_per_album"] = 10
    policy["diversity"]["min_artist_gap"] = 0
    policy["audit"]["min_unique_artist_ratio"] = 0.0
    policy["audit"]["max_artist_share"] = 1.0
    policy["audit"]["max_album_share"] = 1.0
    policy["audit"]["min_mean_transition_score"] = 0.0
    policy["audit"]["min_worst_transition_score"] = 0.0
    policy["audit"]["max_mean_arc_error"] = 1.0

    curated = sonagram.curate_playlist(str(graph_path), brief, policy)
    repeated = sonagram.curate_playlist(str(graph_path), brief, policy)
    assert curated == repeated
    assert curated["exportable"] is True, curated["audit"]["issues"]
    assert len(curated["track_ids"]) == 3

    audit = sonagram.audit_playlist(
        str(graph_path), curated["track_ids"], policy, brief=brief
    )
    assert audit == curated["audit"]
    short_audit = sonagram.audit_playlist(
        str(graph_path), curated["track_ids"][:-1], policy, brief=brief
    )
    assert any(
        issue["code"] == "target_track_count" for issue in short_audit["issues"]
    )
    explanation = sonagram.explain_playlist(
        str(graph_path), curated["track_ids"], policy
    )
    assert explanation["tracks"] == curated["explanation"]["tracks"]
    assert explanation["transitions"] == curated["explanation"]["transitions"]
    assert explanation["summary"] == curated["explanation"]["summary"][:1]
    assert len(explanation["transitions"]) == 2

    mismatched = dict(policy)
    mismatched["preset"] = "focus"
    try:
        sonagram.curate_playlist(str(graph_path), brief, mismatched)
    except ValueError:
        pass
    else:
        raise AssertionError("brief/policy preset mismatch must raise ValueError")

    for function in (sonagram.audit_playlist, sonagram.explain_playlist):
        try:
            function(str(graph_path), [])
        except ValueError:
            pass
        else:
            raise AssertionError("empty track id lists must raise ValueError")

    try:
        sonagram.curate_playlist(str(graph_path), {"preset": "invalid"})
    except ValueError:
        pass
    else:
        raise AssertionError("invalid brief must raise ValueError")

print("ok")
