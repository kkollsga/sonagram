//! Sonagram's typed music-domain extension of KGLite's MCP server.
//!
//! This module is the single registration authority used by both the native
//! `sonagram-mcp-server` binary and the Python-wheel console entry point.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use kglite::api::DirGraph;
use kglite_mcp_server::{DomainGraphState, ServerExtensions};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::Config;
use crate::curation::{
    self, PlaylistAudit, PlaylistBrief, PlaylistExplanation, PlaylistPolicy, PlaylistPreset,
    CURATION_POLICY_VERSION,
};
use crate::playlist::{self, PlaylistMeta};
use crate::{Result, SonagramError};

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PolicyArgs {
    #[serde(default)]
    preset: PlaylistPreset,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StoreRequest {
    /// Human-readable playlist name. A filesystem-safe unique slug is derived.
    name: String,
    /// Original natural-language request retained with the audit provenance.
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CurateArgs {
    brief: PlaylistBrief,
    /// Complete policy override. Omit to resolve the brief's preset policy.
    #[serde(default)]
    policy: Option<PlaylistPolicy>,
    /// Omit for a read-only result. When present, only an exportable result is
    /// written to Sonagram's configured playlist store.
    #[serde(default)]
    store: Option<StoreRequest>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OrderedPlaylistArgs {
    track_ids: Vec<String>,
    /// Shorthand preset when neither a complete brief nor policy supplies it.
    #[serde(default)]
    preset: Option<PlaylistPreset>,
    /// Supplying the original brief activates its stricter intent checks.
    #[serde(default)]
    brief: Option<PlaylistBrief>,
    /// Complete policy override. Omit to resolve the selected preset policy.
    #[serde(default)]
    policy: Option<PlaylistPolicy>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlaylistSlugArgs {
    slug: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlaylistUpdateArgs {
    slug: String,
    #[serde(default)]
    description: Option<String>,
    /// Set true to clear the stored request. Exactly one of this and
    /// `description` must be supplied.
    #[serde(default)]
    clear_description: bool,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlaylistDeleteArgs {
    slug: String,
    /// Must exactly equal `slug`; this prevents an inferred or stale delete.
    confirm_slug: String,
}

#[derive(Debug, Serialize)]
struct StoredPaths {
    slug: String,
    m3u8_path: PathBuf,
    meta_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct CurateResult {
    curated: crate::curation::CuratedPlaylist,
    stored: Option<StoredPaths>,
}

#[derive(Debug, Serialize)]
struct PlaylistSummary {
    name: String,
    slug: String,
    created_at: String,
    request: Option<String>,
    n_tracks: usize,
    total_duration_sec: i64,
    preset: Option<PlaylistPreset>,
    audit_passed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct DeleteResult {
    slug: String,
    deleted: bool,
}

/// Run KGLite's stdio server with Sonagram's domain tools registered.
///
/// A static `--graph` is mandatory because Sonagram exposes a deliberately
/// read-only music capability set, registered against the boot graph schema.
pub fn run<I, T>(args: I) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let argv: Vec<OsString> = args.into_iter().map(Into::into).collect();
    // Let KGLite/Clap own its standard informational exits without forcing an
    // otherwise-unused graph argument first.
    if !argv.iter().skip(1).any(|value| {
        matches!(value.to_str(), Some("--help" | "-h" | "--version" | "-V"))
    }) {
        startup_graph_path(&argv)?;
    }
    let extensions = server_extensions();
    kglite_mcp_server::run_with_extensions(argv, extensions)?;
    Ok(())
}

fn server_extensions() -> ServerExtensions {
    ServerExtensions::new().with_domain_tools(|registry| {
        // A generic KGLite graph must not acquire music methods merely because
        // it happens to sit beside Sonagram's manifest assets.
        let state = registry.graph_state();
        let sonagram_track_contract = [
            "content_hash",
            "path",
            "source_root",
            "is_music",
            "is_canonical",
        ]
        .into_iter()
        .all(|property| state.has_property("Track", property));
        if !state.has_node_type("Track")
            || !state.has_node_type("Library")
            || !state.has_property("Library", "path")
            || !sonagram_track_contract
        {
            return Ok(());
        }

        let graph = registry.graph_state().clone();
        registry.register_typed_tool::<EmptyArgs, _>(
            "music_library_profile",
            "Return Sonagram's typed curation profile: eligible counts, diversity, quality tiers, and per-axis coverage/distributions.",
            move |_| handle_graph(&graph, curation::profile_library),
        )?;

        registry.register_typed_tool::<PolicyArgs, _>(
            "music_curation_policy",
            "Resolve a complete, versioned Sonagram playlist policy for one preset.",
            |args| success(PlaylistPolicy::for_preset(args.preset)),
        )?;

        let graph = registry.graph_state().clone();
        registry.register_typed_tool::<CurateArgs, _>(
            "music_curate_playlist",
            "Deterministically select, sequence, repair, audit, and explain a playlist. Optional storage writes only an exportable, audit-passing result to Sonagram's configured store.",
            move |args| handle_graph_context(&graph, |active, source_path| {
                curate_on_graph(active, source_path, args)
            }),
        )?;

        let graph = registry.graph_state().clone();
        registry.register_typed_tool::<OrderedPlaylistArgs, _>(
            "music_audit_playlist",
            "Independently audit ordered Track content hashes against a typed Sonagram policy and optional original brief.",
            move |args| handle_graph(&graph, |active| audit_on_graph(active, args)),
        )?;

        let graph = registry.graph_state().clone();
        registry.register_typed_tool::<OrderedPlaylistArgs, _>(
            "music_explain_playlist",
            "Explain ordered Track content hashes with per-track contributions, transitions, and optional seed-relative evidence.",
            move |args| handle_graph(&graph, |active| explain_on_graph(active, args)),
        )?;

        registry.register_typed_tool::<EmptyArgs, _>(
            "music_playlists_list",
            "List compact summaries of playlists in Sonagram's configured store, newest first.",
            |_| respond(list_playlists()),
        )?;

        registry.register_typed_tool::<PlaylistSlugArgs, _>(
            "music_playlist_show",
            "Retrieve one stored playlist's ordered tracks and complete curation provenance by validated slug.",
            |args| respond(show_playlist(args)),
        )?;

        registry.register_typed_tool::<PlaylistUpdateArgs, _>(
            "music_playlist_update",
            "Update or explicitly clear only the request description of a stored playlist; order and provenance remain unchanged.",
            |args| respond(update_playlist(args)),
        )?;

        registry.register_typed_tool::<PlaylistDeleteArgs, _>(
            "music_playlist_delete",
            "Delete a stored playlist's M3U and metadata pair. confirm_slug must exactly match slug.",
            |args| respond(delete_playlist(args)),
        )
    })
}

fn handle_graph<T, F>(state: &DomainGraphState, operation: F) -> String
where
    T: Serialize,
    F: FnOnce(&DirGraph) -> Result<T>,
{
    match state.with_graph(|graph| operation(graph.dir().as_ref())) {
        Some(result) => respond(result),
        None => failure("no_active_graph", "no active graph is loaded"),
    }
}

fn handle_graph_context<T, F>(state: &DomainGraphState, operation: F) -> String
where
    T: Serialize,
    F: FnOnce(&DirGraph, Option<&Path>) -> Result<T>,
{
    match state.with_context(|context| {
        operation(context.graph().dir().as_ref(), context.source_path())
    }) {
        Some(result) => respond(result),
        None => failure("no_active_graph", "no active graph is loaded"),
    }
}

fn curate_on_graph(
    graph: &DirGraph,
    graph_path: Option<&Path>,
    args: CurateArgs,
) -> Result<CurateResult> {
    let policy = resolve_policy(Some(&args.brief), None, args.policy)?;
    let curated = curation::curate_playlist(graph, &args.brief, &policy)?;
    let stored = match args.store {
        None => None,
        // A failed result remains valuable structured evidence, but it must
        // never create the playlist directory or write a partial artifact.
        Some(_) if !curated.exportable || !curated.audit.passed => None,
        Some(store) => {
            let graph_path = graph_path.ok_or_else(|| {
                SonagramError::Config(
                    "active MCP graph has no persistence path for playlist provenance".into(),
                )
            })?;
            if store.name.trim().is_empty() {
                return Err(SonagramError::Playlist(
                    "stored playlist name must not be empty".into(),
                ));
            }
            let entries = playlist::entries_from_graph(graph, Path::new(""), &curated.track_ids)?;
            let dir = Config::load()?.resolved_playlists_dir()?;
            let saved = playlist::save_curated_playlist(
                &dir,
                &store.name,
                store.description.as_deref(),
                &curated,
                &entries,
                graph_path,
                None,
            )?;
            Some(StoredPaths {
                slug: saved.slug,
                m3u8_path: saved.m3u8_path,
                meta_path: saved.meta_path,
            })
        }
    };
    Ok(CurateResult { curated, stored })
}

fn audit_on_graph(graph: &DirGraph, args: OrderedPlaylistArgs) -> Result<PlaylistAudit> {
    if args.track_ids.is_empty() {
        return Err(SonagramError::Playlist("track_ids must not be empty".into()));
    }
    let policy = resolve_policy(args.brief.as_ref(), args.preset, args.policy)?;
    match args.brief {
        Some(brief) => {
            curation::audit_playlist_for_brief(graph, &args.track_ids, &brief, &policy)
        }
        None => curation::audit_playlist(graph, &args.track_ids, &policy),
    }
}

fn explain_on_graph(graph: &DirGraph, args: OrderedPlaylistArgs) -> Result<PlaylistExplanation> {
    if args.track_ids.is_empty() {
        return Err(SonagramError::Playlist("track_ids must not be empty".into()));
    }
    let policy = resolve_policy(args.brief.as_ref(), args.preset, args.policy)?;
    match args.brief {
        Some(brief) => {
            curation::explain_playlist_for_brief(graph, &args.track_ids, &brief, &policy)
        }
        None => curation::explain_playlist(graph, &args.track_ids, &policy),
    }
}

fn resolve_policy(
    brief: Option<&PlaylistBrief>,
    preset: Option<PlaylistPreset>,
    policy: Option<PlaylistPolicy>,
) -> Result<PlaylistPolicy> {
    if let (Some(brief), Some(preset)) = (brief, preset) {
        if brief.preset != preset {
            return Err(SonagramError::Playlist(
                "preset does not match the original brief".into(),
            ));
        }
    }
    let expected = brief.map(|value| value.preset).or(preset);
    let resolved = policy.unwrap_or_else(|| PlaylistPolicy::for_preset(expected.unwrap_or_default()));
    if resolved.version != CURATION_POLICY_VERSION {
        return Err(SonagramError::Playlist(format!(
            "unsupported policy version {}; expected {}",
            resolved.version, CURATION_POLICY_VERSION
        )));
    }
    if expected.is_some_and(|value| value != resolved.preset) {
        return Err(SonagramError::Playlist(
            "resolved policy preset does not match the request".into(),
        ));
    }
    Ok(resolved)
}

fn configured_playlist_dir() -> Result<PathBuf> {
    Config::load()?.resolved_playlists_dir()
}

fn list_playlists() -> Result<Vec<PlaylistSummary>> {
    Ok(playlist::list_playlists(&configured_playlist_dir()?)?
        .into_iter()
        .map(|meta| PlaylistSummary {
            name: meta.name,
            slug: meta.slug,
            created_at: meta.created_at,
            request: meta.request,
            n_tracks: meta.n_tracks,
            total_duration_sec: meta.total_duration_sec,
            preset: meta.curation.as_ref().map(|value| value.policy.preset),
            audit_passed: meta.curation.as_ref().map(|value| value.audit.passed),
        })
        .collect())
}

fn show_playlist(args: PlaylistSlugArgs) -> Result<PlaylistMeta> {
    playlist::load_playlist_meta(&configured_playlist_dir()?, &args.slug)
}

fn update_playlist(args: PlaylistUpdateArgs) -> Result<PlaylistMeta> {
    if args.description.is_some() == args.clear_description {
        return Err(SonagramError::Playlist(
            "pass exactly one of description or clear_description=true".into(),
        ));
    }
    let description = if args.clear_description {
        None
    } else {
        args.description.as_deref()
    };
    playlist::update_playlist_request(&configured_playlist_dir()?, &args.slug, description)
}

fn delete_playlist(args: PlaylistDeleteArgs) -> Result<DeleteResult> {
    if args.confirm_slug != args.slug {
        return Err(SonagramError::Playlist(
            "confirm_slug must exactly match slug".into(),
        ));
    }
    let deleted = playlist::delete_playlist(&configured_playlist_dir()?, &args.slug)?;
    Ok(DeleteResult {
        slug: args.slug,
        deleted,
    })
}

fn respond<T: Serialize>(result: Result<T>) -> String {
    match result {
        Ok(value) => success(value),
        Err(error) => failure("sonagram_error", &error.to_string()),
    }
}

fn success<T: Serialize>(result: T) -> String {
    serde_json::to_string(&json!({ "ok": true, "result": result })).unwrap_or_else(|error| {
        failure("serialization_error", &format!("serialize MCP result: {error}"))
    })
}

fn failure(code: &str, message: &str) -> String {
    serde_json::to_string(&json!({
        "ok": false,
        "error": { "code": code, "message": message }
    }))
    .unwrap_or_else(|_| {
        "{\"ok\":false,\"error\":{\"code\":\"serialization_error\",\"message\":\"failed to serialize MCP error\"}}".into()
    })
}

fn startup_graph_path(argv: &[OsString]) -> Result<PathBuf> {
    let mut graph = None;
    let mut index = 1;
    while index < argv.len() {
        let value = argv[index].to_string_lossy();
        if value == "--graph" {
            index += 1;
            let path = argv.get(index).ok_or_else(|| {
                SonagramError::Config("sonagram-mcp-server: --graph requires a path".into())
            })?;
            set_graph_arg(&mut graph, PathBuf::from(path))?;
        } else if let Some(path) = value.strip_prefix("--graph=") {
            if path.is_empty() {
                return Err(SonagramError::Config(
                    "sonagram-mcp-server: --graph requires a path".into(),
                ));
            }
            set_graph_arg(&mut graph, PathBuf::from(path))?;
        } else if value == "--writable" {
            return Err(SonagramError::Config(
                "sonagram-mcp-server is intentionally read-only; --writable is unsupported".into(),
            ));
        }
        index += 1;
    }
    let graph = graph.ok_or_else(|| {
        SonagramError::Config(
            "sonagram-mcp-server requires static --graph <music.kgl> mode".into(),
        )
    })?;
    std::fs::canonicalize(&graph).map_err(|error| {
        SonagramError::Config(format!(
            "canonicalize MCP graph {}: {error}",
            graph.display()
        ))
    })
}

fn set_graph_arg(slot: &mut Option<PathBuf>, value: PathBuf) -> Result<()> {
    if slot.replace(value).is_some() {
        return Err(SonagramError::Config(
            "sonagram-mcp-server accepts exactly one --graph argument".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{self, LibraryInfo};
    use crate::record::AnalysisRecord;

    fn fixture_graph() -> std::sync::Arc<DirGraph> {
        let record: AnalysisRecord = serde_json::from_str(include_str!(
            "../tests/fixtures/analyses/04-marry-you.json"
        ))
        .unwrap();
        graph::build_graph(
            &[record],
            &LibraryInfo {
                root: "mcp-fixture".into(),
                n_tracks: 1,
            },
        )
        .unwrap()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "sonagram-mcp-server-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn startup_requires_one_existing_graph_and_canonicalizes_it() {
        let root = temp_dir("argv");
        let graph = root.join("music.kgl");
        std::fs::write(&graph, b"fixture").unwrap();
        let argv = vec![OsString::from("sonagram-mcp-server"), OsString::from("--graph"), graph.clone().into_os_string()];
        assert_eq!(startup_graph_path(&argv).unwrap(), std::fs::canonicalize(&graph).unwrap());
        assert!(startup_graph_path(&[OsString::from("sonagram-mcp-server")]).is_err());
        let writable = vec![
            OsString::from("sonagram-mcp-server"),
            OsString::from("--graph"),
            graph.clone().into_os_string(),
            OsString::from("--writable"),
        ];
        let error = startup_graph_path(&writable).unwrap_err().to_string();
        assert!(error.contains("read-only"));
        let duplicate = vec![
            OsString::from("server"),
            OsString::from("--graph=a.kgl"),
            OsString::from("--graph=b.kgl"),
        ];
        assert!(startup_graph_path(&duplicate).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn policy_resolution_rejects_version_and_preset_drift() {
        let brief = PlaylistBrief {
            preset: PlaylistPreset::Focus,
            ..PlaylistBrief::default()
        };
        let policy = resolve_policy(Some(&brief), None, None).unwrap();
        assert_eq!(policy.preset, PlaylistPreset::Focus);

        let mut stale = policy.clone();
        stale.version += 1;
        assert!(resolve_policy(Some(&brief), None, Some(stale)).is_err());
        assert!(resolve_policy(
            Some(&brief),
            Some(PlaylistPreset::Party),
            Some(policy)
        )
        .is_err());
    }

    #[test]
    fn failed_curation_never_creates_the_requested_store() {
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap();
        let root = temp_dir("failed-store");
        std::env::set_var("SONAGRAM_HOME", &root);
        let graph_path = root.join("music.kgl");
        std::fs::write(&graph_path, b"provenance-placeholder").unwrap();
        let brief = PlaylistBrief {
            target_tracks: 100,
            ..PlaylistBrief::default()
        };
        let result = curate_on_graph(
            fixture_graph().as_ref(),
            Some(&graph_path),
            CurateArgs {
                brief,
                policy: None,
                store: Some(StoreRequest {
                    name: "Must Not Exist".into(),
                    description: None,
                }),
            },
        )
        .unwrap();
        assert!(!result.curated.exportable);
        assert!(result.stored.is_none());
        assert!(!root.join("playlists").exists());
        std::env::remove_var("SONAGRAM_HOME");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delete_requires_exact_confirmation_before_config_access() {
        let error = delete_playlist(PlaylistDeleteArgs {
            slug: "focus".into(),
            confirm_slug: "other".into(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("exactly match"));
    }

    #[test]
    fn envelopes_are_machine_readable_and_distinguish_failures() {
        let ok: serde_json::Value = serde_json::from_str(&success(vec!["a"])).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["result"][0], "a");
        let error: serde_json::Value =
            serde_json::from_str(&failure("invalid_request", "bad input")).unwrap();
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["code"], "invalid_request");
    }
}
