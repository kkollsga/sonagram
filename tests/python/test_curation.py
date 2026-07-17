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
    for source in sorted(fixtures.glob("*.json")):
        data = json.loads(source.read_text())
        content_hash = data["source"]["content_hash"]
        shutil.copyfile(source, analysis / f"{content_hash}.json")

    graph_path = root / "music.kgl"
    sonagram.build(str(library), str(graph_path))

    profile = sonagram.profile_library(str(graph_path))
    assert profile["tracks"] >= 12
    assert profile["stats"]["energy"]["present"] > 0

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

    audit = sonagram.audit_playlist(str(graph_path), curated["track_ids"], policy)
    assert audit == curated["audit"]
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
