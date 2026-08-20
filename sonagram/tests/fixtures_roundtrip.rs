//! Integration gate over the frozen fixtures under `tests/fixtures/analyses/`.
//!
//! Every captured fixture must parse into an [`AnalysisRecord`], round-trip
//! (serialize → parse) to an identical value, and carry the invariants the
//! graph builder relies on: a 48-dim similarity embedding, the current Sonara
//! schema and the complete fused-aggression contract. This keeps the fixtures
//! honest as the DTO evolves.

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

        assert_eq!(
            rec.analysis.provenance.schema_version,
            sonara::analyze::ANALYSIS_SCHEMA_VERSION,
            "{name}: fixture must use the current Sonara analysis schema"
        );
        assert!(
            rec.analysis
                .provenance
                .requested_features
                .as_ref()
                .is_some_and(|features| features.iter().any(|f| f == "aggression")),
            "{name}: aggression feature was not requested"
        );
        assert_eq!(
            rec.analysis.provenance.aggression_model_id.as_deref(),
            Some(sonara::aggression::AGGRESSION_MODEL_ID),
            "{name}: wrong aggression model provenance"
        );

        let bounded = |field: &str, value: Option<f32>| {
            let value = value.unwrap_or_else(|| panic!("{name}: {field} is missing"));
            assert!(
                value.is_finite() && (0.0..=1.0).contains(&value),
                "{name}: {field} out of range: {value}"
            );
        };
        if let Some(score) = rec.analysis.aggression_score {
            assert!(
                score.is_finite() && (0.0..=1.0).contains(&score),
                "{name}: aggression_score out of range: {score}"
            );
        }
        bounded("aggression_confidence", rec.analysis.aggression_confidence);
        bounded(
            "aggression_forcefulness",
            rec.analysis.aggression_forcefulness,
        );
        bounded("aggression_harshness", rec.analysis.aggression_harshness);
        bounded("aggression_tension", rec.analysis.aggression_tension);
        bounded("aggression_rhythm", rec.analysis.aggression_rhythm);
    }
}
