//! End-to-end CLI contracts for library-owned curation. No audio is used.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use sonagram::curation::{PlaylistPolicy, PlaylistPreset};
use sonagram::graph::{self, LibraryInfo};
use sonagram::playlist;
use sonagram::record::AnalysisRecord;

fn temp_home() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "sonagram-cli-curation-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn records() -> Vec<AnalysisRecord> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/analyses");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            AnalysisRecord::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
        })
        .collect()
}

fn prepare_graph(home: &Path) {
    let records = records();
    let mut graph = graph::build_graph(
        &records,
        &LibraryInfo {
            root: "cli-curation-fixtures".into(),
            n_tracks: records.len(),
        },
    )
    .unwrap();
    graph::save(&mut graph, &home.join("music.kgl")).unwrap();
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sonagram"))
        .env("SONAGRAM_HOME", home)
        .args(args)
        .output()
        .unwrap()
}

fn json_output(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn permissive_policy_json() -> String {
    let mut policy = PlaylistPolicy::for_preset(PlaylistPreset::General);
    policy.eligibility.allow_low_quality = true;
    policy.diversity.max_per_artist = 10;
    policy.diversity.max_per_album = 10;
    policy.diversity.min_artist_gap = 0;
    policy.audit.min_unique_artist_ratio = 0.0;
    policy.audit.max_artist_share = 1.0;
    policy.audit.max_album_share = 1.0;
    policy.audit.min_mean_transition_score = 0.0;
    policy.audit.min_worst_transition_score = 0.0;
    policy.audit.max_mean_arc_error = 1.0;
    serde_json::to_string(&policy).unwrap()
}

#[test]
fn profile_curate_audit_explain_and_failed_save_contracts() {
    let home = temp_home();
    prepare_graph(&home);
    let policy = permissive_policy_json();

    let profile = run(&home, &["profile", "--format", "json"]);
    assert!(profile.status.success(), "{}", String::from_utf8_lossy(&profile.stderr));
    let profile_json = json_output(&profile);
    assert!(profile_json["tracks"].as_u64().unwrap() > 3);

    let resolved = run(&home, &["policy", "--preset", "focus", "--format", "json"]);
    assert!(resolved.status.success());
    let resolved = json_output(&resolved);
    assert_eq!(resolved["preset"], "focus");
    assert_eq!(resolved["targets"]["seed_similarity"], "neutral");

    let curated = run(
        &home,
        &[
            "curate",
            "--tracks",
            "3",
            "--policy-json",
            &policy,
            "--name",
            "CLI Focus",
            "--description",
            "typed CLI regression",
            "--format",
            "json",
        ],
    );
    assert!(curated.status.success(), "{}", String::from_utf8_lossy(&curated.stderr));
    let curated_json = json_output(&curated);
    assert_eq!(curated_json["result"]["exportable"], true);
    assert_eq!(curated_json["result"]["track_ids"].as_array().unwrap().len(), 3);
    assert_eq!(curated_json["stored"]["slug"], "cli-focus");
    let ids = curated_json["result"]["track_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(",");

    let audit = run(
        &home,
        &["audit", "--ids", &ids, "--policy-json", &policy, "--format", "json"],
    );
    assert!(audit.status.success());
    assert_eq!(json_output(&audit)["passed"], true);

    let brief = serde_json::to_string(&curated_json["result"]["brief"]).unwrap();
    let brief_audit = run(
        &home,
        &[
            "audit",
            "--ids",
            &ids,
            "--brief-json",
            &brief,
            "--policy-json",
            &policy,
            "--format",
            "json",
        ],
    );
    assert!(brief_audit.status.success());
    assert_eq!(json_output(&brief_audit)["passed"], true);

    let explain = run(
        &home,
        &[
            "explain",
            "--ids",
            &ids,
            "--policy-json",
            &policy,
            "--format",
            "json",
        ],
    );
    assert!(explain.status.success());
    assert_eq!(json_output(&explain)["tracks"].as_array().unwrap().len(), 3);

    let meta = playlist::load_playlist_meta(&home.join("playlists"), "cli-focus").unwrap();
    assert!(meta.curation.is_some());
    assert_eq!(meta.request.as_deref(), Some("typed CLI regression"));

    let updated = run(
        &home,
        &[
            "playlists",
            "update",
            "cli-focus",
            "--description",
            "updated through CLI",
        ],
    );
    assert!(updated.status.success());
    assert_eq!(
        playlist::load_playlist_meta(&home.join("playlists"), "cli-focus")
            .unwrap()
            .request
            .as_deref(),
        Some("updated through CLI")
    );

    let focus_policy = serde_json::to_string(&PlaylistPolicy::for_preset(PlaylistPreset::Focus))
        .unwrap();
    let mismatch = run(
        &home,
        &[
            "audit",
            "--ids",
            &ids,
            "--preset",
            "general",
            "--policy-json",
            &focus_policy,
            "--format",
            "json",
        ],
    );
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("does not match"));

    let before = std::fs::read_dir(home.join("playlists")).unwrap().count();
    let failed = run(
        &home,
        &[
            "curate",
            "--tracks",
            "9999",
            "--policy-json",
            &policy,
            "--name",
            "Must Not Exist",
            "--format",
            "json",
        ],
    );
    assert!(!failed.status.success());
    assert_eq!(json_output(&failed)["result"]["exportable"], false);
    let after = std::fs::read_dir(home.join("playlists")).unwrap().count();
    assert_eq!(before, after, "failed audit writes no playlist files");
    assert!(!home.join("playlists/must-not-exist.meta.json").exists());

    let deleted = run(&home, &["playlists", "delete", "cli-focus"]);
    assert!(deleted.status.success());
    assert!(!home.join("playlists/cli-focus.meta.json").exists());
    assert!(!home.join("playlists/cli-focus.m3u8").exists());

    let _ = std::fs::remove_dir_all(home);
}
