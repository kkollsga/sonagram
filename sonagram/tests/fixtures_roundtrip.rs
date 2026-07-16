//! Integration gate over the frozen fixtures under `tests/fixtures/analyses/`.
//!
//! Every captured fixture must parse into an [`AnalysisRecord`], round-trip
//! (serialize → parse) to an identical value, and carry the invariants the
//! graph builder relies on: a 48-dim similarity embedding and a schema version
//! of at least 1. This keeps the fixtures honest as the DTO evolves.

use std::path::PathBuf;

use sonagram::record::AnalysisRecord;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/analyses")
}

fn fixture_json_paths() -> Vec<PathBuf> {
    let dir = fixtures_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read fixtures dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn fixtures_exist() {
    let paths = fixture_json_paths();
    assert!(
        paths.len() >= 15,
        "expected at least 15 fixtures, found {} in {}",
        paths.len(),
        fixtures_dir().display()
    );
}

#[test]
fn every_fixture_round_trips_and_holds_invariants() {
    let paths = fixture_json_paths();
    assert!(!paths.is_empty(), "no fixtures found");

    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: read: {e}"));
        let rec = AnalysisRecord::from_json(&raw).unwrap_or_else(|e| panic!("{name}: parse: {e}"));

        // Serialize → parse must be identical.
        let json = rec
            .to_json_pretty()
            .unwrap_or_else(|e| panic!("{name}: serialize: {e}"));
        let reparsed =
            AnalysisRecord::from_json(&json).unwrap_or_else(|e| panic!("{name}: reparse: {e}"));
        assert_eq!(rec, reparsed, "{name}: round-trip mismatch");

        // Embedding present and 48-dimensional (kglite EmbeddingStore contract).
        let embedding = rec
            .analysis
            .embedding
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: embedding is None"));
        assert_eq!(
            embedding.len(),
            48,
            "{name}: embedding len {} != 48",
            embedding.len()
        );

        // Analysis schema version must be at least 1.
        assert!(
            rec.analysis.provenance.schema_version >= 1,
            "{name}: schema_version {} < 1",
            rec.analysis.provenance.schema_version
        );
    }
}
