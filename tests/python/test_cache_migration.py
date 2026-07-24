"""Python API gate: aggression model migrations reanalyze, current caches reuse."""

import json
import tempfile
from pathlib import Path

import sonagram


repo = Path(__file__).resolve().parents[2]
fixture = repo / "sonagram/tests/fixtures/analyses/01-intro-ft-king-rell.json"

with tempfile.TemporaryDirectory() as tmp:
    library = Path(tmp)
    audio = library / "a.mp3"
    audio.write_bytes(b"ID3\x04\x00\x00\x00\x00\x00\x00not-decodable-audio")

    record = json.loads(fixture.read_text())
    record["source"].update(
        {
            "content_hash": "cached-hash",
            "path": "a.mp3",
            "file_size": audio.stat().st_size,
        }
    )
    record["analysis"]["provenance"]["schema_version"] = 5
    record["analysis"]["provenance"]["aggression_model_id"] = "aggression-rank-v2"

    cache = library / ".sonagram"
    analysis = cache / "analysis"
    analysis.mkdir(parents=True)
    (analysis / "cached-hash.json").write_text(json.dumps(record, indent=2) + "\n")
    index = {
        "a.mp3": {
            "size": audio.stat().st_size,
            "mtime_unix": int(audio.stat().st_mtime),
            "content_hash": "cached-hash",
        }
    }
    (cache / "index.json").write_text(json.dumps(index, indent=2) + "\n")

    report = sonagram.scan(str(library))
    assert report["migrated_analysis"] == 0, report
    assert report["analyzed"] == 0, report
    assert report["reused_stat_match"] == 0, report
    assert len(report["failed"]) == 1, report

    unchanged = json.loads((analysis / "cached-hash.json").read_text())
    assert unchanged["analysis"]["provenance"]["schema_version"] == 5
    assert unchanged["analysis"]["provenance"]["aggression_model_id"] == "aggression-rank-v2"

with tempfile.TemporaryDirectory() as tmp:
    library = Path(tmp)
    audio = library / "a.mp3"
    audio.write_bytes(b"ID3\x04\x00\x00\x00\x00\x00\x00not-decodable-audio")

    record = json.loads(fixture.read_text())
    record["source"].update(
        {
            "content_hash": "cached-hash",
            "path": "a.mp3",
            "file_size": audio.stat().st_size,
        }
    )
    record["analysis"]["provenance"]["schema_version"] = 6
    record["analysis"]["provenance"]["aggression_model_id"] = (
        "aggression-rank-v3-sr22050"
    )

    cache = library / ".sonagram"
    analysis = cache / "analysis"
    analysis.mkdir(parents=True)
    (analysis / "cached-hash.json").write_text(json.dumps(record, indent=2) + "\n")
    index = {
        "a.mp3": {
            "size": audio.stat().st_size,
            "mtime_unix": int(audio.stat().st_mtime),
            "content_hash": "cached-hash",
        }
    }
    (cache / "index.json").write_text(json.dumps(index, indent=2) + "\n")

    report = sonagram.scan(str(library))
    assert report["migrated_analysis"] == 0, report
    assert report["analyzed"] == 0, report
    assert report["reused_stat_match"] == 1, report
    assert report["failed"] == [], report

print("cache reanalysis API tests passed")
