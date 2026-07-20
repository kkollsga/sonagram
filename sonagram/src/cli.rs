//! The `sonagram` command-line surface, shared by **both** frontends.
//!
//! [`run`] is the single entry point. The standalone `sonagram` binary
//! (`src/bin/sonagram.rs`) is a five-line shim that forwards its argv here, and
//! the `pip install sonagram` wheel's console script does the same through
//! `sonagram-python`'s `_run_cli`. Keeping every subcommand — parsing, output
//! strings, and exit codes — in this one library function means the cargo binary
//! and the pip CLI **cannot drift**. This mirrors codingest's `codingest-cli`
//! design.
//!
//! Plain `std::env`-style argument parsing (no `clap`), matching the upstream
//! no-heavy-CLI-deps discipline. Subcommands:
//!
//! ```text
//! sonagram scan     <library_root>
//! sonagram enrich   <library_root>
//! sonagram build    <library_root> <out.kgl>
//! sonagram playlist <library_root> <graph.kgl> (--cypher '<q>' | --ids h1,h2)
//!                   (--out <file.m3u8> and/or --copy-to <dir>)
//! sonagram status   <library_root> [--format json]
//! ```
//!
//! Progress and stage lines go to stderr; results (reports, paths, counts) go
//! to stdout, so the CLI composes in a pipeline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::config::Config;
use crate::curation::{self, PlaylistBrief, PlaylistPolicy, PlaylistPreset};
use crate::enrich::{self, EnrichOptions, EnrichProgress, EnrichmentData};
use crate::graph::{self, LibraryInfo, SourceInput};
use crate::pipeline;
use crate::playlist;
use crate::record::AnalysisRecord;
use crate::scan::{self, scan_library, FreshnessReport, ScanOptions, ScanProgress};
use crate::{Result, SonagramError, VERSION};

/// Run the CLI over `args` — the argument vector **without** the program name
/// (so `args[0]` is the subcommand), exactly as both `std::env::args().skip(1)`
/// and the Python shim's `sys.argv[1:]` supply it.
///
/// Returns the process exit code: `0` on success, `1` on error, and — for
/// `status` only — a freshness code (`0` fresh, `1` needs scan, `2` no cache).
pub fn run(args: &[String]) -> i32 {
    // Global --help, except `playlist --help` which the subcommand handles.
    if args.iter().any(|a| a == "--help" || a == "-h")
        && args.first().map(String::as_str) != Some("playlist")
    {
        print_help();
        return 0;
    }
    match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") | Some("help") => {
            print_help();
            return 0;
        }
        Some("--version") | Some("-V") | Some("version") => {
            println!("sonagram {VERSION}");
            return 0;
        }
        _ => {}
    }

    match args[0].as_str() {
        "scan" => finish(cmd_scan(&args[1..])),
        "enrich" => finish(cmd_enrich(&args[1..])),
        "build" => finish(cmd_build(&args[1..])),
        "playlist" => finish(cmd_playlist(&args[1..])),
        "profile" => finish(cmd_profile(&args[1..])),
        "policy" => finish(cmd_policy(&args[1..])),
        "curate" => finish(cmd_curate(&args[1..])),
        "audit" => finish(cmd_audit(&args[1..])),
        "explain" => finish(cmd_explain(&args[1..])),
        "playlists" => finish(cmd_playlists(&args[1..])),
        "sources" => finish(cmd_sources(&args[1..])),
        "config" => finish(cmd_config(&args[1..])),
        "skill" => finish(cmd_skill(&args[1..])),
        "mcp" => finish(cmd_mcp(&args[1..])),
        "progress" => finish(cmd_progress(&args[1..])),
        // `status` owns its exit code (0 fresh / 1 needs-scan / 2 no-cache), so
        // it is not funnelled through `finish`.
        "status" => cmd_status(&args[1..]),
        other => {
            eprintln!("error: unknown subcommand '{other}' — try `sonagram --help`");
            1
        }
    }
}

/// Map a subcommand `Result` to a process exit code, printing errors to stderr.
fn finish(result: Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn print_help() {
    println!(
        "sonagram {VERSION} — map a music library into a queryable kglite graph\n\
\n\
USAGE:\n\
    sonagram <SUBCOMMAND>\n\
\n\
SUBCOMMANDS:\n\
    scan     <library_root> [--no-enrich]\n\
             Scan a library (walk, hash, analyze unseen files) and cache\n\
             per-track analysis under <library_root>/.sonagram/. Analysis\n\
             streams: records persist as they complete (a killed scan resumes\n\
             where it stopped) and Last.fm enrichment runs IN PARALLEL by\n\
             default when a key is configured (--no-enrich opts out; a missing\n\
             key just skips it). Prints a scan report.\n\
\n\
    progress [<library_root>] [--format json]\n\
             Read the live on-disk progress snapshots (scan_progress.json /\n\
             enrich_progress.json, written by every scan/enrich regardless of\n\
             entry point) with derived %, rate, ETA, and staleness. No args =\n\
             every configured source.\n\
\n\
    enrich   <library_root>\n\
             Fetch Last.fm metadata (popularity, folksonomy tags, MBIDs,\n\
             similar artists/tracks, original-album mapping) for the library's\n\
             artists/tracks/albums and cache it under\n\
             <library_root>/.sonagram/lastfm/. Needs LASTFM_API_KEY (env or a\n\
             .env file). Re-runs skip already-fetched entities (incremental).\n\
\n\
    build    <library_root> <out.kgl>\n\
             Build the knowledge graph from the cached analysis records and\n\
             save it to <out.kgl>. Run `scan` first. Auto-loads the Last.fm\n\
             enrichment cache when present (run `enrich` to populate it).\n\
\n\
    playlist <library_root> <graph.kgl>\n\
             (--cypher '<query>' | --ids <hash1,hash2,...>)\n\
             (--out <file.m3u8> and/or --copy-to <dir>)\n\
             Resolve a track set from the graph and materialize it. --cypher\n\
             runs a read-only query whose result is a Track-node or\n\
             content-hash column; --ids takes content hashes directly. Track\n\
             order is preserved (never re-sorted).\n\
             --out writes an absolute-path .m3u8. --copy-to <dir> writes a\n\
             SELF-CONTAINED, PORTABLE folder: the tracks copied as\n\
             'NN - Artist - Title.<ext>' next to a relative-path .m3u8 (named\n\
             after --out's stem, else the folder). Copies only — source files\n\
             are never moved, retagged, or modified. Pass either flag or both;\n\
             at least one is required.\n\
\n\
    profile  [--format json|human]\n\
             Summarize curation-relevant statistics from the configured graph.\n\
\n\
    policy   [--preset <name>] [--format json|human]\n\
             Print the complete versioned policy for a preset, ready to amend\n\
             and pass through --policy-json or the Python API.\n\
\n\
    curate   [--preset <name>] [--tracks N] [--duration-sec N]\n\
             [--seed-ids <hashes>] [--brief-json <json>]\n\
             [--policy-json <json>] [--name <name>] [--description <text>]\n\
             [--format json|human]\n\
             Select, sequence, audit, and optionally store a playlist through\n\
             the library-owned curation engine. Failed audits are never saved.\n\
\n\
    audit    --ids <hashes> [--brief-json <json>]\n\
             [--preset <name> | --policy-json <json>]\n\
             [--format json|human]\n\
             Independently audit an ordered playlist against a typed policy.\n\
\n\
    explain  --ids <hashes> [--brief-json <json>]\n\
             [--preset <name> | --policy-json <json>]\n\
             [--format json|human]\n\
             Explain track, transition, and arc contributions for an order.\n\
\n\
    status   <library_root> [--format json]\n\
             Read-only freshness probe (mutates nothing): report how the cache\n\
             under <library_root>/.sonagram/ compares to the files on disk.\n\
             Exit code: 0 = fresh, 1 = needs scan, 2 = no cache. The default is\n\
             human lines; --format json emits one stable object with keys:\n\
               library_root        (string) the probed root\n\
               has_cache           (bool)   .sonagram/index.json exists\n\
               total_files         (int)    *.mp3 files on disk\n\
               fresh               (int)    indexed, stats + record still fresh\n\
               stale               (int)    stats changed or record stale/missing\n\
               missing_from_index  (int)    on disk, never scanned\n\
               deleted_in_index    (int)    indexed, file now gone\n\
               has_enrichment      (bool)   Last.fm cache present & non-empty\n\
               schema_version      (int)    current sonara analysis schema\n\
               similarity_version  (int)    current sonara embedding version\n\
               needs_scan          (bool)   any stale/missing/deleted\n\
               status              (string) 'fresh' | 'needs_scan' | 'no_cache'\n\
               exit_code           (int)    0 | 1 | 2, matching the exit status\n\
\n\
    sources  add <dir> | remove <dir> | list\n\
             Manage the configured source registry (~/.sonagram/config.json).\n\
             `add` canonicalizes + dedupes; the dir must exist.\n\
\n\
    config   [show] | set graph|playlists_dir <path>\n\
             Show the resolved config (incl. defaults + whether files exist), or\n\
             set the central graph / playlist-store location.\n\
\n\
    skill    show | install [--dir <skills_root>] [--force]\n\
             Print the bundled sonagram-playlist agent skill, or install it to\n\
             <skills_root>/sonagram-playlist/SKILL.md (default ~/.claude/skills).\n\
             Install fills in your configured library path, refuses to overwrite\n\
             without --force, and tells the agent to read + follow it in-session.\n\
\n\
    mcp      install [--force]\n\
             Install a kglite-native manifest and live-gated music skills next\n\
             to the configured graph. Identical assets are an idempotent no-op;\n\
             differing operator files require --force.\n\
\n\
    playlists                 List stored playlists (from `curate --name`).\n\
    playlists show <slug>     Full metadata + tracklist for one stored playlist.\n\
    playlists update <slug>   Set/clear the stored request description.\n\
    playlists delete <slug>   Delete the stored .m3u8 + metadata pair.\n\
\n\
CONFIG-DRIVEN FORMS (no path args — fan out over configured sources):\n\
    sonagram scan                 scan every configured source\n\
    sonagram status               probe source + graph freshness separately; JSON\n\
                                  adds `graph_current_for_cache` (`graph_current`\n\
                                  compatibility alias) + top-level `graph_stale`.\n\
                                  Retryable scan failures can coexist with a graph\n\
                                  current for every usable cached analysis.\n\
    sonagram enrich               enrich all sources\n\
    sonagram build                multi-source build → the configured graph\n\
    sonagram playlist (--cypher|--ids ...) --name <name> [--description <text>]\n\
                                  manually materialize caller-selected IDs\n\
    sonagram curate --preset focus --name <name>\n\
                                  library-select, audit, and store a playlist\n\
\n\
FLAGS:\n\
    -h, --help       Print this help\n\
    -V, --version    Print version\n\
\n\
EXAMPLES:\n\
    sonagram sources add ~/Music\n\
    sonagram scan\n\
    sonagram build\n\
    sonagram curate --preset focus --tracks 25 --name 'Deep Focus' --description 'work playlist'\n\
    sonagram playlists\n\
    # explicit-path forms still work:\n\
    sonagram build ~/Music music.kgl\n\
    sonagram playlist ~/Music music.kgl \\\n\
        --cypher 'MATCH (t:Track) WHERE t.bpm > 120 RETURN t.content_hash ORDER BY t.energy' \\\n\
        --out set.m3u8"
    );
}

fn cmd_scan(args: &[String]) -> Result<()> {
    // P20: scan and Last.fm enrichment run in PARALLEL by default (scan is
    // CPU-heavy, enrichment network-heavy). --no-enrich opts out; a missing
    // API key degrades to a plain scan with a note, never an error.
    let mut with_enrich = true;
    let mut root: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "--no-enrich" => with_enrich = false,
            other if other.starts_with("--") => {
                return Err(SonagramError::Config(format!(
                    "scan: unexpected argument '{other}' — try [<library_root>] [--no-enrich]"
                )))
            }
            other => {
                if root.is_some() {
                    return Err(SonagramError::Config(format!(
                        "scan: unexpected extra argument '{other}'"
                    )));
                }
                root = Some(other);
            }
        }
    }

    // Explicit `scan <library_root>` (backward-compatible) vs config-driven
    // `scan` (every configured source, sequentially).
    match root {
        Some(root) => {
            scan_one(&PathBuf::from(root), with_enrich)?;
        }
        None => {
            let sources = configured_source_paths(&Config::load()?)?;
            let mut totals = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize); // files, analyzed, migrated, hash, stat, failed
            for src in &sources {
                let r = scan_one(src, with_enrich)?;
                totals.0 += r.total_files;
                totals.1 += r.analyzed;
                totals.2 += r.migrated_analysis;
                totals.3 += r.reused_hash_match;
                totals.4 += r.reused_stat_match;
                totals.5 += r.failed.len();
            }
            println!("combined scan over {} source(s):", sources.len());
            println!("  total files:        {}", totals.0);
            println!("  analyzed (new):     {}", totals.1);
            println!("  migrated (cached):  {}", totals.2);
            println!("  reused (hash match):{}", totals.3);
            println!("  reused (stat match):{}", totals.4);
            println!("  failed:             {}", totals.5);
        }
    }
    Ok(())
}

/// Scan one library root (with concurrent Last.fm enrichment unless opted
/// out), print its per-source report, and return the scan report.
fn scan_one(root: &Path, with_enrich: bool) -> Result<crate::scan::ScanReport> {
    let opts = ScanOptions {
        progress: Some(Box::new(stage_line)),
        ..Default::default()
    };
    let report = if with_enrich {
        let enrich_opts = EnrichOptions {
            api_key: None,
            progress: Some(Box::new(enrich_line)),
        };
        let combined = pipeline::scan_and_enrich_library(root, &opts, &enrich_opts)?;
        match &combined.enrich {
            Some(e) => print_enrich_report(root, e),
            None => eprintln!(
                "[enrich] no LASTFM_API_KEY configured — scanned without enrichment \
                 (see `sonagram enrich --help`)"
            ),
        }
        combined.scan
    } else {
        scan_library(root, &opts)?
    };

    println!("scan report for {}", root.display());
    println!("  total files:        {}", report.total_files);
    println!("  analyzed (new):     {}", report.analyzed);
    println!("  migrated (cached):  {}", report.migrated_analysis);
    println!("  reused (hash match):{}", report.reused_hash_match);
    println!("  reused (stat match):{}", report.reused_stat_match);
    println!("  failed:             {}", report.failed.len());
    for (path, msg) in &report.failed {
        println!("    - {}: {msg}", path.display());
    }
    println!("  elapsed:            {:.2?}", report.elapsed);
    Ok(report)
}

fn cmd_enrich(args: &[String]) -> Result<()> {
    // Explicit `enrich <library_root>` vs config-driven `enrich` (all sources).
    match optional_positional(args, 0) {
        Some(root) => enrich_one(&PathBuf::from(root)),
        None => {
            let sources = configured_source_paths(&Config::load()?)?;
            for src in &sources {
                enrich_one(src)?;
            }
            Ok(())
        }
    }
}

fn enrich_one(root: &Path) -> Result<()> {
    let opts = EnrichOptions {
        api_key: None,
        progress: Some(Box::new(enrich_line)),
    };
    let report = enrich::enrich_library(root, &opts)?;
    print_enrich_report(root, &report);
    Ok(())
}

/// Print the enrich report block for one source (shared by `enrich` and the
/// parallel `scan` pipeline).
fn print_enrich_report(root: &Path, report: &enrich::EnrichReport) {
    println!("enrich report for {}", root.display());
    println!(
        "  artists: {} fetched, {} skipped, {} failed",
        report.artists_fetched, report.artists_skipped, report.artists_failed
    );
    println!(
        "  tracks:  {} fetched, {} skipped, {} failed",
        report.tracks_fetched, report.tracks_skipped, report.tracks_failed
    );
    println!(
        "  albums:  {} fetched, {} skipped, {} failed",
        report.albums_fetched, report.albums_skipped, report.albums_failed
    );
    println!("  elapsed: {:.2?}", report.elapsed);
}

fn cmd_build(args: &[String]) -> Result<()> {
    let positionals: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with("--"))
        .collect();
    match positionals.len() {
        // Explicit `build <library_root> <out.kgl>` — single-source (P17 stamps a
        // Source node id = the absolute root + `Track.source_root`).
        2 => cmd_build_single(&PathBuf::from(positionals[0]), &PathBuf::from(positionals[1])),
        // Config-driven `build` — multi-source over every configured source → the
        // configured graph path.
        0 => cmd_build_multi(),
        _ => Err(SonagramError::Graph(
            "build: pass `<library_root> <out.kgl>` (explicit) or no args (config-driven)".into(),
        )),
    }
}

/// Explicit single-source build. `Source` node id + every `Track.source_root` is
/// the absolute library root (canonicalized when possible), while the `Library`
/// node keeps its short label.
fn cmd_build_single(root: &Path, out: &Path) -> Result<()> {
    eprintln!("[build] loading cached records from {}", root.display());
    let records = scan::load_records(root)?;
    if records.is_empty() {
        return Err(SonagramError::Playlist(format!(
            "no cached records under {} — run `sonagram scan` first",
            root.display()
        )));
    }
    let enrichment = EnrichmentData::load(root)?;
    report_enrichment(enrichment.as_ref());
    eprintln!("[build] {} records → building graph", records.len());
    let source_root = abs_string(root);
    // P19: stamp this source's scan-state fingerprint onto its Source node.
    let scan_fingerprint = scan::load_scan_fingerprint(root)?;
    let sources = [SourceInput {
        root: source_root,
        records: &records,
        scan_fingerprint,
    }];
    let library = LibraryInfo {
        root: library_label(root),
        n_tracks: records.len(),
    };
    let mut g = graph::build_graph_from_sources(&sources, enrichment.as_ref(), &library)?;
    graph::save(&mut g, out)?;
    println!("built graph from {} tracks → {}", records.len(), out.display());
    Ok(())
}

/// Config-driven multi-source build: load records from every configured source,
/// merge (one Track per content hash, first source wins), and write the graph to
/// the configured graph path.
fn cmd_build_multi() -> Result<()> {
    let cfg = Config::load()?;
    let sources = configured_source_paths(&cfg)?;
    let out = cfg.resolved_graph()?;

    // Load each source's records + merge its enrichment cache.
    let mut loaded: Vec<(String, Vec<AnalysisRecord>, Option<String>)> = Vec::new();
    let mut enrichment = EnrichmentData::default();
    let mut any_enrichment = false;
    for src in &sources {
        let records = scan::load_records(src)?;
        // Content-addressed dedup means N files can share one record — say so,
        // or a user counting files wonders where tracks "went".
        let n_files = crate::scan::cache::Cache::new(src)
            .load_index()
            .map(|i| i.len())
            .unwrap_or(0);
        if n_files > records.len() {
            eprintln!(
                "[build] {} records from {} ({} files — {} duplicate file(s) share a recording)",
                records.len(),
                src.display(),
                n_files,
                n_files - records.len()
            );
        } else {
            eprintln!("[build] {} records from {}", records.len(), src.display());
        }
        if let Some(e) = EnrichmentData::load(src)? {
            merge_enrichment(&mut enrichment, e);
            any_enrichment = true;
        }
        // P19: stamp each source's scan-state fingerprint onto its Source node.
        let scan_fingerprint = scan::load_scan_fingerprint(src)?;
        loaded.push((abs_string(src), records, scan_fingerprint));
    }
    if loaded.iter().all(|(_, r, _)| r.is_empty()) {
        return Err(SonagramError::Playlist(
            "no cached records under any configured source — run `sonagram scan` first".into(),
        ));
    }
    let enr = if any_enrichment && !enrichment.is_empty() {
        report_enrichment(Some(&enrichment));
        Some(&enrichment)
    } else {
        eprintln!("[build] no enrichment cache — plain build");
        None
    };

    let source_inputs: Vec<SourceInput> = loaded
        .iter()
        .map(|(root, records, scan_fingerprint)| SourceInput {
            root: root.clone(),
            records,
            scan_fingerprint: scan_fingerprint.clone(),
        })
        .collect();
    // One configured source keeps its real path as the Library label; the
    // "multi-source" label is reserved for genuinely merged builds.
    let library = LibraryInfo {
        root: if source_inputs.len() == 1 {
            source_inputs[0].root.clone()
        } else {
            "multi-source".to_string()
        },
        n_tracks: 0, // overridden by the deduped track count in the builder
    };
    let mut g = graph::build_graph_from_sources(&source_inputs, enr, &library)?;
    graph::save(&mut g, &out)?;
    let n = g
        .type_indices
        .get("Track")
        .map(|r| r.len())
        .unwrap_or(0);
    println!(
        "built multi-source graph from {} source(s), {} tracks → {}",
        sources.len(),
        n,
        out.display()
    );
    Ok(())
}

/// Fold `src`'s enrichment maps into `dst`. Source order remains the stable
/// tiebreak, but a usable fetched record replaces an earlier failed/unfetched
/// duplicate so content-hash dedup cannot hide successful enrichment.
fn merge_enrichment(dst: &mut EnrichmentData, src: EnrichmentData) {
    merge_map_preferring_usable(&mut dst.artists, src.artists, |v| v.fetched && !v.failed);
    merge_map_preferring_usable(&mut dst.tracks, src.tracks, |v| v.fetched && !v.failed);
    merge_map_preferring_usable(&mut dst.albums, src.albums, |v| v.fetched && !v.failed);
}

fn merge_map_preferring_usable<T>(
    dst: &mut BTreeMap<String, T>,
    src: BTreeMap<String, T>,
    usable: impl Fn(&T) -> bool,
) {
    use std::collections::btree_map::Entry;

    for (key, value) in src {
        match dst.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            Entry::Occupied(mut entry) if !usable(entry.get()) && usable(&value) => {
                entry.insert(value);
            }
            Entry::Occupied(_) => {}
        }
    }
}

/// Print the one-line enrichment status a build reports to stderr.
fn report_enrichment(enrichment: Option<&EnrichmentData>) {
    match enrichment {
        Some(e) if !e.is_empty() => eprintln!(
            "[build] enriched build: {} artists, {} tracks",
            e.artists_present(),
            e.tracks_present()
        ),
        _ => eprintln!("[build] no enrichment cache — plain build"),
    }
}

fn load_configured_graph() -> Result<(Config, PathBuf, std::sync::Arc<kglite::api::DirGraph>)> {
    let cfg = Config::load()?;
    let graph_path = cfg.resolved_graph()?;
    let graph = kglite::api::io::load_file(path_str(&graph_path)?)
        .map_err(|e| SonagramError::Graph(format!("load {}: {e}", graph_path.display())))?;
    Ok((cfg, graph_path, graph))
}

fn cmd_profile(args: &[String]) -> Result<()> {
    let as_json = parse_output_format("profile", args)?;
    let (_, _, graph) = load_configured_graph()?;
    let profile = curation::profile_library(&graph)?;
    if as_json {
        print_json(&profile)?;
    } else {
        println!(
            "{} tracks ({} music, {} canonical), {} artists, {} albums, {} Songs, {} styles",
            profile.tracks,
            profile.music_tracks,
            profile.canonical_tracks,
            profile.unique_artists,
            profile.unique_albums,
            profile.unique_songs,
            profile.unique_styles
        );
        for (name, stat) in &profile.stats {
            println!(
                "  {name}: {}/{} present, median {}",
                stat.present,
                stat.total,
                stat.median
                    .map(|value| format!("{value:.3}"))
                    .unwrap_or_else(|| "?".into())
            );
        }
    }
    Ok(())
}

fn cmd_policy(args: &[String]) -> Result<()> {
    let mut preset = PlaylistPreset::General;
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--preset" => preset = parse_preset(&flag_value(args, &mut i, "--preset")?)?,
            "--format" => {
                as_json = parse_format_value("policy", &flag_value(args, &mut i, "--format")?)?;
            }
            "--json" => as_json = true,
            other => {
                return Err(SonagramError::Playlist(format!(
                    "policy: unexpected argument '{other}'"
                )))
            }
        }
        i += 1;
    }
    let policy = PlaylistPolicy::for_preset(preset);
    if as_json {
        print_json(&policy)?;
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&policy).map_err(|error| {
                SonagramError::Playlist(format!("serialize policy JSON: {error}"))
            })?
        );
    }
    Ok(())
}

fn cmd_curate(args: &[String]) -> Result<()> {
    let mut preset = None;
    let mut target_tracks = None;
    let mut target_duration_sec = None;
    let mut seed_ids = None;
    let mut brief_json = None;
    let mut policy_json = None;
    let mut name = None;
    let mut description = None;
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--preset" => preset = Some(parse_preset(&flag_value(args, &mut i, "--preset")?)?),
            "--tracks" => {
                let value = flag_value(args, &mut i, "--tracks")?;
                target_tracks = Some(value.parse::<usize>().map_err(|_| {
                    SonagramError::Playlist(format!("curate: --tracks expects an integer, got {value}"))
                })?);
            }
            "--duration-sec" => {
                let value = flag_value(args, &mut i, "--duration-sec")?;
                target_duration_sec = Some(value.parse::<u64>().map_err(|_| {
                    SonagramError::Playlist(format!(
                        "curate: --duration-sec expects an integer, got {value}"
                    ))
                })?);
            }
            "--seed-ids" => seed_ids = Some(parse_ids(&flag_value(args, &mut i, "--seed-ids")?)),
            "--brief-json" => brief_json = Some(flag_value(args, &mut i, "--brief-json")?),
            "--policy-json" => policy_json = Some(flag_value(args, &mut i, "--policy-json")?),
            "--name" => name = Some(flag_value(args, &mut i, "--name")?),
            "--description" => description = Some(flag_value(args, &mut i, "--description")?),
            "--format" => {
                as_json = parse_format_value("curate", &flag_value(args, &mut i, "--format")?)?;
            }
            "--json" => as_json = true,
            other => {
                return Err(SonagramError::Playlist(format!(
                    "curate: unexpected argument '{other}'"
                )))
            }
        }
        i += 1;
    }
    let brief_from_json = brief_json.is_some();
    let mut brief = if let Some(raw) = brief_json {
        if preset.is_some() || target_tracks.is_some() || target_duration_sec.is_some() || seed_ids.is_some() {
            return Err(SonagramError::Playlist(
                "curate: --brief-json cannot be combined with --preset/--tracks/--duration-sec/--seed-ids"
                    .into(),
            ));
        }
        serde_json::from_str::<PlaylistBrief>(&raw).map_err(|e| {
            SonagramError::Playlist(format!("curate: invalid --brief-json: {e}"))
        })?
    } else {
        let mut brief = PlaylistBrief {
            preset: preset.unwrap_or_default(),
            ..PlaylistBrief::default()
        };
        if let Some(value) = target_tracks {
            brief.target_tracks = value;
        }
        brief.target_duration_sec = target_duration_sec;
        if let Some(value) = seed_ids {
            brief.seed_ids = value;
        }
        brief
    };
    let expected_preset = (brief_from_json || preset.is_some()).then_some(brief.preset);
    let policy = parse_policy(policy_json.as_deref(), expected_preset, "curate")?;
    if expected_preset.is_none() && policy_json.is_some() {
        brief.preset = policy.preset;
    }
    let (cfg, graph_path, graph) = load_configured_graph()?;
    let result = curation::curate_playlist(&graph, &brief, &policy)?;
    if !result.exportable {
        emit_curate_result(&result, None, as_json)?;
        return Err(SonagramError::Playlist(
            "curate: library audit failed; no playlist was written".into(),
        ));
    }
    let stored = if let Some(name) = name {
        let entries = playlist::entries_from_graph(&graph, Path::new(""), &result.track_ids)?;
        let dir = cfg.resolved_playlists_dir()?;
        let saved = playlist::save_curated_playlist(
            &dir,
            &name,
            description.as_deref(),
            &result,
            &entries,
            &graph_path,
            None,
        )?;
        Some(json!({
            "slug": saved.slug,
            "m3u8_path": saved.m3u8_path,
            "meta_path": saved.meta_path,
        }))
    } else {
        None
    };
    emit_curate_result(&result, stored, as_json)
}

fn emit_curate_result(
    result: &curation::CuratedPlaylist,
    stored: Option<serde_json::Value>,
    as_json: bool,
) -> Result<()> {
    if as_json {
        print_json(&json!({ "result": result, "stored": stored }))
    } else {
        println!(
            "curated {} tracks; audit {}; mean transition {}; arc error {}",
            result.track_ids.len(),
            if result.audit.passed { "passed" } else { "failed" },
            result
                .audit
                .mean_transition_score
                .map(|value| format!("{value:.3}"))
                .unwrap_or_else(|| "?".into()),
            result
                .audit
                .mean_arc_error
                .map(|value| format!("{value:.3}"))
                .unwrap_or_else(|| "?".into())
        );
        for issue in &result.audit.issues {
            println!("  {:?} {}: {}", issue.severity, issue.code, issue.message);
        }
        if let Some(stored) = stored {
            println!("stored: {}", stored["m3u8_path"].as_str().unwrap_or("?"));
        }
        Ok(())
    }
}

fn cmd_audit(args: &[String]) -> Result<()> {
    let (ids, policy, brief, as_json) = parse_order_policy_args("audit", args)?;
    let (_, _, graph) = load_configured_graph()?;
    let audit = match brief {
        Some(brief) => curation::audit_playlist_for_brief(&graph, &ids, &brief, &policy)?,
        None => curation::audit_playlist(&graph, &ids, &policy)?,
    };
    if as_json {
        print_json(&audit)?;
    } else {
        println!(
            "audit {}: {} tracks, {} artists, mean transition {}",
            if audit.passed { "passed" } else { "failed" },
            audit.track_count,
            audit.unique_artists,
            audit
                .mean_transition_score
                .map(|value| format!("{value:.3}"))
                .unwrap_or_else(|| "?".into())
        );
        for issue in &audit.issues {
            println!("  {:?} {}: {}", issue.severity, issue.code, issue.message);
        }
    }
    Ok(())
}

fn cmd_explain(args: &[String]) -> Result<()> {
    let (ids, policy, brief, as_json) = parse_order_policy_args("explain", args)?;
    let (_, _, graph) = load_configured_graph()?;
    let explanation = match brief {
        Some(brief) => curation::explain_playlist_for_brief(&graph, &ids, &brief, &policy)?,
        None => curation::explain_playlist(&graph, &ids, &policy)?,
    };
    if as_json {
        print_json(&explanation)?;
    } else {
        for line in &explanation.summary {
            println!("{line}");
        }
        for track in &explanation.tracks {
            println!(
                "  {:>3}. {} - {} [{}]",
                track.position,
                track.artist.as_deref().unwrap_or("?"),
                track.title.as_deref().unwrap_or("?"),
                track
                    .contributions
                    .iter()
                    .map(|item| format!("{}={:.3}", item.component, item.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

fn parse_order_policy_args(
    command: &str,
    args: &[String],
) -> Result<(Vec<String>, PlaylistPolicy, Option<PlaylistBrief>, bool)> {
    let mut ids = None;
    let mut preset = None;
    let mut policy_json = None;
    let mut brief_json = None;
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--ids" => ids = Some(parse_ids(&flag_value(args, &mut i, "--ids")?)),
            "--preset" => preset = Some(parse_preset(&flag_value(args, &mut i, "--preset")?)?),
            "--policy-json" => policy_json = Some(flag_value(args, &mut i, "--policy-json")?),
            "--brief-json" if matches!(command, "audit" | "explain") => {
                brief_json = Some(flag_value(args, &mut i, "--brief-json")?)
            }
            "--format" => {
                as_json = parse_format_value(command, &flag_value(args, &mut i, "--format")?)?;
            }
            "--json" => as_json = true,
            other => {
                return Err(SonagramError::Playlist(format!(
                    "{command}: unexpected argument '{other}'"
                )))
            }
        }
        i += 1;
    }
    let ids = ids.filter(|value| !value.is_empty()).ok_or_else(|| {
        SonagramError::Playlist(format!("{command}: pass --ids <hash1,hash2,...>"))
    })?;
    let brief = brief_json
        .map(|raw| {
            serde_json::from_str::<PlaylistBrief>(&raw).map_err(|error| {
                SonagramError::Playlist(format!("{command}: invalid --brief-json: {error}"))
            })
        })
        .transpose()?;
    if preset.is_some_and(|value| brief.as_ref().is_some_and(|brief| brief.preset != value)) {
        return Err(SonagramError::Playlist(
            format!("{command}: --preset does not match --brief-json"),
        ));
    }
    let expected_preset = brief.as_ref().map(|brief| brief.preset).or(preset);
    let policy = parse_policy(policy_json.as_deref(), expected_preset, command)?;
    Ok((ids, policy, brief, as_json))
}

fn parse_policy(
    raw: Option<&str>,
    preset: Option<PlaylistPreset>,
    command: &str,
) -> Result<PlaylistPolicy> {
    match raw {
        Some(raw) => {
            let policy = serde_json::from_str::<PlaylistPolicy>(raw).map_err(|e| {
                SonagramError::Playlist(format!("{command}: invalid --policy-json: {e}"))
            })?;
            if let Some(preset) = preset {
                if policy.preset != preset {
                    return Err(SonagramError::Playlist(format!(
                        "{command}: --preset does not match --policy-json"
                    )));
                }
            }
            Ok(policy)
        }
        None => Ok(PlaylistPolicy::for_preset(preset.unwrap_or_default())),
    }
}

fn parse_preset(value: &str) -> Result<PlaylistPreset> {
    match value.to_ascii_lowercase().as_str() {
        "general" => Ok(PlaylistPreset::General),
        "focus" => Ok(PlaylistPreset::Focus),
        "party" => Ok(PlaylistPreset::Party),
        "workout" => Ok(PlaylistPreset::Workout),
        "chill" => Ok(PlaylistPreset::Chill),
        "discovery" => Ok(PlaylistPreset::Discovery),
        _ => Err(SonagramError::Playlist(format!(
            "unknown preset '{value}' (expected general|focus|party|workout|chill|discovery)"
        ))),
    }
}

fn parse_ids(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_output_format(command: &str, args: &[String]) -> Result<bool> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        [flag, value] if flag == "--format" => parse_format_value(command, value),
        _ => Err(SonagramError::Playlist(format!(
            "{command}: expected only [--format json|human]"
        ))),
    }
}

fn parse_format_value(command: &str, value: &str) -> Result<bool> {
    match value {
        "json" => Ok(true),
        "human" => Ok(false),
        _ => Err(SonagramError::Playlist(format!(
            "{command}: --format expects json or human, got {value}"
        ))),
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(value)
            .map_err(|e| SonagramError::Playlist(format!("serialize JSON output: {e}")))?
    );
    Ok(())
}

fn cmd_playlist(args: &[String]) -> Result<()> {
    // Local --help for the subcommand (its flags are non-trivial).
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    // Leading positionals (up to the first `--flag`): explicit form takes two
    // (`<library_root> <graph.kgl>`), config-driven form takes none.
    let n_pos = args.iter().take_while(|a| !a.starts_with("--")).count();
    let (root_arg, graph_arg) = match n_pos {
        2 => (
            Some(PathBuf::from(&args[0])),
            Some(PathBuf::from(&args[1])),
        ),
        0 => (None, None),
        _ => {
            return Err(SonagramError::Playlist(
                "playlist: pass `<library_root> <graph.kgl>` (explicit) or neither (config-driven)"
                    .into(),
            ))
        }
    };

    let mut cypher: Option<String> = None;
    let mut ids: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut copy_to: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;

    let mut i = n_pos;
    while i < args.len() {
        match args[i].as_str() {
            "--cypher" => cypher = Some(flag_value(args, &mut i, "--cypher")?),
            "--ids" => ids = Some(flag_value(args, &mut i, "--ids")?),
            "--out" => out = Some(PathBuf::from(flag_value(args, &mut i, "--out")?)),
            "--copy-to" => copy_to = Some(PathBuf::from(flag_value(args, &mut i, "--copy-to")?)),
            "--name" => name = Some(flag_value(args, &mut i, "--name")?),
            "--description" => description = Some(flag_value(args, &mut i, "--description")?),
            other => {
                return Err(SonagramError::Playlist(format!(
                    "unexpected argument '{other}' to `playlist`"
                )))
            }
        }
        i += 1;
    }

    if cypher.is_some() == ids.is_some() {
        return Err(SonagramError::Playlist(
            "playlist: pass exactly one of --cypher '<query>' or --ids <hashes>".into(),
        ));
    }
    // A destination is required: a central-store name, an explicit .m3u8, and/or
    // a portable copy-folder.
    if name.is_none() && out.is_none() && copy_to.is_none() {
        return Err(SonagramError::Playlist(
            "playlist: pass --name <name>, --out <file.m3u8>, and/or --copy-to <dir>".into(),
        ));
    }

    // Config is needed for the graph path (config-driven form) and/or the
    // playlist-store dir (--name).
    let cfg = if graph_arg.is_none() || name.is_some() {
        Some(Config::load()?)
    } else {
        None
    };
    let graph_file = match &graph_arg {
        Some(g) => g.clone(),
        None => cfg.as_ref().expect("cfg loaded").resolved_graph()?,
    };
    // Fallback library root for pre-P17 graphs; P17 graphs resolve off each
    // Track's own `source_root`, so an empty root is fine there.
    let library_root = root_arg.clone().unwrap_or_default();

    let g = kglite::api::io::load_file(path_str(&graph_file)?)
        .map_err(|e| SonagramError::Graph(format!("load {}: {e}", graph_file.display())))?;

    let ids_vec: Option<Vec<String>> = ids.as_ref().map(|s| {
        s.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });
    let entries = match (&cypher, &ids_vec) {
        (Some(q), _) => playlist::entries_from_cypher(g.as_ref(), &library_root, q)?,
        (None, Some(list)) => playlist::entries_from_graph(g.as_ref(), &library_root, list)?,
        _ => unreachable!("exactly-one guard above"),
    };

    // Optional portable copy-folder (shared by both output modes).
    let copy_report = if let Some(dir) = &copy_to {
        let folder = name
            .clone()
            .unwrap_or_else(|| playlist_name(out.as_deref(), dir));
        Some(playlist::export_folder(&entries, dir, &folder)?)
    } else {
        None
    };

    if let Some(name) = &name {
        // Central store: <playlists_dir>/<slug>.m3u8 + <slug>.meta.json.
        let dir = cfg.as_ref().expect("cfg loaded").resolved_playlists_dir()?;
        let stored = playlist::save_playlist(
            &dir,
            name,
            description.as_deref(),
            cypher.as_deref(),
            ids_vec.as_deref(),
            &entries,
            &graph_file,
            copy_to.as_deref(),
        )?;
        // An explicit --out alongside --name writes an extra copy there.
        if let Some(out) = &out {
            playlist::write_m3u8(&entries, out)?;
        }
        println!(
            "stored playlist '{}' ({} tracks) → {}",
            stored.meta.name,
            stored.meta.n_tracks,
            stored.m3u8_path.display()
        );
        println!("  metadata: {}", stored.meta_path.display());
        if let Some(rep) = &copy_report {
            println!("  portable copy: {}", rep.playlist_path.display());
        }
        if let Some(out) = &out {
            println!("  also wrote: {}", out.display());
        }
        println!(
            "  retrieve: `sonagram playlists`  (details: `sonagram playlists show {}`)",
            stored.slug
        );
    } else {
        if let Some(out) = &out {
            playlist::write_m3u8(&entries, out)?;
            println!("wrote {} tracks → {}", entries.len(), out.display());
        }
        if let Some(rep) = &copy_report {
            println!(
                "copied {} tracks ({} bytes) → {}",
                rep.copied,
                rep.bytes,
                rep.playlist_path.display()
            );
        }
    }

    Ok(())
}

/// `sonagram playlists` (list) and `sonagram playlists show <slug>` — read the
/// central playlist store built by `playlist --name`.
fn cmd_playlists(args: &[String]) -> Result<()> {
    let dir = Config::load()?.resolved_playlists_dir()?;
    match args.first().map(String::as_str) {
        None | Some("list") => {
            let metas = playlist::list_playlists(&dir)?;
            if metas.is_empty() {
                println!("no stored playlists in {}", dir.display());
                return Ok(());
            }
            println!("stored playlists in {} (newest first):", dir.display());
            for m in &metas {
                let dur = format!("{}m{:02}s", m.total_duration_sec / 60, m.total_duration_sec % 60);
                println!(
                    "  {}  [{} tracks, {}, {}]",
                    m.slug, m.n_tracks, dur, m.created_at
                );
                println!("      {}", m.name);
                if let Some(req) = &m.request {
                    println!("      request: {}", one_line(req));
                }
            }
            Ok(())
        }
        Some("show") => {
            let slug = optional_positional(&args[1..], 0).ok_or_else(|| {
                SonagramError::Playlist("playlists show: missing <slug>".into())
            })?;
            let m = playlist::load_playlist_meta(&dir, slug)?;
            println!("playlist: {} ({})", m.name, m.slug);
            println!("  created:  {}", m.created_at);
            if let Some(req) = &m.request {
                println!("  request:  {req}");
            }
            if let Some(q) = &m.cypher {
                println!("  cypher:   {q}");
            }
            println!(
                "  tracks:   {} ({}m{:02}s total)",
                m.n_tracks,
                m.total_duration_sec / 60,
                m.total_duration_sec % 60
            );
            println!("  graph:    {}", m.graph);
            if let Some(c) = &m.copy_to {
                println!("  copy_to:  {c}");
            }
            if let Some(curation) = &m.curation {
                println!("  preset:   {:?}", curation.policy.preset);
                println!(
                    "  audit:    {} ({} repair attempt(s))",
                    if curation.audit.passed { "passed" } else { "failed" },
                    curation.repair_attempts
                );
            }
            for t in &m.tracks {
                let dur = t
                    .duration_sec
                    .map(|d| format!("{}s", d.round() as i64))
                    .unwrap_or_else(|| "?".to_string());
                println!(
                    "  {:>3}. {} - {} ({dur})",
                    t.position,
                    t.artist.as_deref().unwrap_or("?"),
                    t.title.as_deref().unwrap_or("?")
                );
            }
            Ok(())
        }
        Some("update") => {
            let slug = args.get(1).ok_or_else(|| {
                SonagramError::Playlist("playlists update: missing <slug>".into())
            })?;
            let mut description = None;
            let mut clear = false;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--description" => {
                        description = Some(flag_value(args, &mut i, "--description")?)
                    }
                    "--clear-description" => clear = true,
                    other => {
                        return Err(SonagramError::Playlist(format!(
                            "playlists update: unexpected argument '{other}'"
                        )))
                    }
                }
                i += 1;
            }
            if description.is_some() == clear {
                return Err(SonagramError::Playlist(
                    "playlists update: pass exactly one of --description <text> or --clear-description"
                        .into(),
                ));
            }
            let meta = playlist::update_playlist_request(&dir, slug, description.as_deref())?;
            println!("updated playlist '{}' ({})", meta.name, meta.slug);
            Ok(())
        }
        Some("delete") => {
            let slug = args.get(1).ok_or_else(|| {
                SonagramError::Playlist("playlists delete: missing <slug>".into())
            })?;
            if args.len() != 2 {
                return Err(SonagramError::Playlist(
                    "playlists delete: expected exactly one <slug>".into(),
                ));
            }
            if playlist::delete_playlist(&dir, slug)? {
                println!("deleted stored playlist '{slug}'");
            } else {
                println!("stored playlist '{slug}' does not exist");
            }
            Ok(())
        }
        Some(other) => Err(SonagramError::Playlist(format!(
            "playlists: unknown subcommand '{other}' — try list|show|update|delete"
        ))),
    }
}

/// `sonagram sources add|remove|list` — manage the configured source registry.
fn cmd_sources(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("add") => {
            let dir = optional_positional(&args[1..], 0)
                .ok_or_else(|| SonagramError::Config("sources add: missing <dir>".into()))?;
            let mut cfg = Config::load()?;
            let (canon, added) = cfg.add_source(Path::new(dir))?;
            cfg.save()?;
            if added {
                println!("added source: {canon}");
            } else {
                println!("already a source: {canon}");
            }
            Ok(())
        }
        Some("remove") => {
            let dir = optional_positional(&args[1..], 0)
                .ok_or_else(|| SonagramError::Config("sources remove: missing <dir>".into()))?;
            let mut cfg = Config::load()?;
            let removed = cfg.remove_source(Path::new(dir));
            cfg.save()?;
            if removed {
                println!("removed source: {dir}");
            } else {
                println!("not a configured source: {dir}");
            }
            Ok(())
        }
        None | Some("list") => {
            let cfg = Config::load()?;
            if cfg.sources.is_empty() {
                println!("no configured sources — add one with `sonagram sources add <dir>`");
            } else {
                println!("configured sources ({}):", cfg.sources.len());
                for s in &cfg.sources {
                    println!("  {s}");
                }
            }
            Ok(())
        }
        Some(other) => Err(SonagramError::Config(format!(
            "sources: unknown subcommand '{other}' — try add/remove/list"
        ))),
    }
}

/// `sonagram config` (show resolved) and `sonagram config set graph|playlists_dir <path>`.
fn cmd_config(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("show") => {
            let cfg = Config::load()?;
            let path = crate::config::config_path()?;
            let graph = cfg.resolved_graph()?;
            let playlists = cfg.resolved_playlists_dir()?;
            println!("config file: {} ({})", path.display(), exists_note(&path));
            println!("sources ({}):", cfg.sources.len());
            for s in &cfg.sources {
                println!("  {s}");
            }
            println!(
                "graph:         {} ({}){}",
                graph.display(),
                exists_note(&graph),
                if cfg.graph.is_none() { " [default]" } else { "" }
            );
            println!(
                "playlists_dir: {} ({}){}",
                playlists.display(),
                exists_note(&playlists),
                if cfg.playlists_dir.is_none() {
                    " [default]"
                } else {
                    ""
                }
            );
            // Last.fm key: report only WHERE it's configured, never the key.
            match enrich::api_key_source() {
                Some(src) => println!("lastfm_key:    configured (via {src})"),
                None => println!("lastfm_key:    not configured"),
            }
            Ok(())
        }
        Some("set") => {
            let key = optional_positional(&args[1..], 0)
                .ok_or_else(|| SonagramError::Config("config set: missing <key> (graph|playlists_dir)".into()))?;
            let val = optional_positional(&args[1..], 1)
                .ok_or_else(|| SonagramError::Config("config set: missing <path>".into()))?;
            let mut cfg = Config::load()?;
            match key {
                "graph" => cfg.graph = Some(val.to_string()),
                "playlists_dir" => cfg.playlists_dir = Some(val.to_string()),
                other => {
                    return Err(SonagramError::Config(format!(
                        "config set: unknown key '{other}' (expected graph|playlists_dir)"
                    )))
                }
            }
            cfg.save()?;
            println!("set {key} = {val}");
            Ok(())
        }
        Some(other) => Err(SonagramError::Config(format!(
            "config: unknown subcommand '{other}' — try `config` or `config set <key> <path>`"
        ))),
    }
}

/// `sonagram skill show` (print the embedded skill) and
/// `sonagram skill install [--dir <skills_root>] [--force]` (write it to
/// `~/.claude/skills/` — P19 cold-start bootstrap).
fn cmd_skill(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("show") => {
            // The file already ends with a newline; `print!` avoids a double one.
            print!("{}", crate::skill::SKILL_MD);
            Ok(())
        }
        Some("install") => {
            let mut dir: Option<PathBuf> = None;
            let mut force = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--dir" => dir = Some(PathBuf::from(flag_value(args, &mut i, "--dir")?)),
                    "--force" => force = true,
                    other => {
                        return Err(SonagramError::Config(format!(
                            "skill install: unexpected argument '{other}' — try [--dir <skills_root>] [--force]"
                        )))
                    }
                }
                i += 1;
            }
            let report = crate::skill::install(dir.as_deref(), force)?;
            println!(
                "installed the sonagram-playlist skill → {}",
                report.path.display()
            );
            if let Some(lib) = &report.library_root {
                println!("  library root: {lib}");
            }
            if let Some(bin) = &report.binary {
                println!("  cli path:     {bin}");
            }
            // Skills load at session start, so this freshly written one is NOT yet
            // active — the agent must read + follow it now, in-session.
            println!(
                "NEXT: READ AND FOLLOW {} NOW, in this session — a skill loads at session start, so \
                 this newly written one is not yet active; open it and follow its steps to serve \
                 the user's request.",
                report.path.display()
            );
            Ok(())
        }
        None => Err(SonagramError::Config(
            "skill: pass `show` or `install [--dir <skills_root>] [--force]`".into(),
        )),
        Some(other) => Err(SonagramError::Config(format!(
            "skill: unknown subcommand '{other}' — try `show` or `install`"
        ))),
    }
}

/// Install the embedded kglite manifest and project skill layer beside the
/// configured graph, where kglite's `<basename>_mcp.yaml` discovery finds it.
fn cmd_mcp(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("install") => {
            let mut force = false;
            for arg in &args[1..] {
                match arg.as_str() {
                    "--force" => force = true,
                    other => {
                        return Err(SonagramError::Config(format!(
                            "mcp install: unexpected argument '{other}' — try [--force]"
                        )))
                    }
                }
            }
            let report = crate::mcp::install(force)?;
            println!("installed Sonagram MCP assets");
            println!("  graph:     {}", report.graph_path.display());
            println!("  manifest:  {}", report.manifest_path.display());
            println!("  skills:    {}", report.skills_dir.display());
            println!("  sandbox:   {}", report.public_source_dir.display());
            if let Some(binary) = &report.server_binary {
                println!("  server:    {}", binary.display());
            }
            println!("  changed:   {}", report.written);
            println!("  unchanged: {}", report.unchanged);
            match report.launch_command() {
                Some(command) => println!("{}: {command}", crate::mcp::launch_label()),
                None => println!(
                    "NEXT: configure your MCP client with an absolute path to \
                     `sonagram-mcp-server` and --graph {}",
                    report.graph_path.display()
                ),
            }
            Ok(())
        }
        None => Err(SonagramError::Config(
            "mcp: pass `install [--force]`".into(),
        )),
        Some(other) => Err(SonagramError::Config(format!(
            "mcp: unknown subcommand '{other}' — try `install`"
        ))),
    }
}

/// `status <library_root> [--format json]` — the read-only freshness probe.
///
/// Returns the exit code directly: `0` fresh, `1` needs scan, `2` no cache.
/// A usage/probe error prints to stderr and returns `1`.
fn cmd_status(args: &[String]) -> i32 {
    let mut root: Option<&str> = None;
    let mut as_json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                i += 1;
                match args.get(i).map(String::as_str) {
                    Some("json") => as_json = true,
                    Some("human") => as_json = false,
                    Some(other) => {
                        eprintln!("error: status: --format expects 'json' or 'human', got '{other}'");
                        return 1;
                    }
                    None => {
                        eprintln!("error: status: --format requires a value ('json' or 'human')");
                        return 1;
                    }
                }
            }
            "--json" => as_json = true,
            other if other.starts_with("--") => {
                eprintln!("error: status: unexpected argument '{other}'");
                return 1;
            }
            other => {
                if root.is_some() {
                    eprintln!("error: status: unexpected extra argument '{other}'");
                    return 1;
                }
                root = Some(other);
            }
        }
        i += 1;
    }

    // Explicit `status <root>` vs config-driven `status` (all sources; the JSON
    // gains a per-source array and the exit code is the worst across sources).
    match root {
        Some(r) => {
            let root = PathBuf::from(r);
            let (exit_code, report, has_enrichment) = match status_one(&root) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            if as_json {
                let obj = status_json_obj(&root, &report, has_enrichment, exit_code);
                println!("{}", serde_json::to_string(&obj).expect("JSON value"));
            } else {
                print_status_human(&root, &report, has_enrichment, exit_code);
            }
            exit_code
        }
        None => {
            let cfg = match Config::load() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            let sources = match configured_source_paths(&cfg) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            // Load the configured graph once (if built) so each Source's exact
            // cached-analysis fingerprint can be compared to the currently
            // usable cache. Source scan failures remain a separate status axis:
            // a graph can be current for every analyzable record while files
            // still need retrying.
            let graph_path = cfg.resolved_graph().ok();
            let graph = graph_path
                .as_ref()
                .filter(|p| p.exists())
                .and_then(|p| p.to_str())
                .and_then(|s| kglite::api::io::load_file(s).ok());
            let graph_present = graph.is_some();

            let mut worst = 0i32;
            let mut any_graph_stale = false;
            let mut objs: Vec<serde_json::Value> = Vec::new();
            let mut humans: Vec<(PathBuf, FreshnessReport, bool, i32, Option<bool>)> = Vec::new();
            for src in &sources {
                let (code, report, has_enr) = match status_one(src) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("error: {}: {e}", src.display());
                        return 1;
                    }
                };
                worst = worst.max(code);
                // Per-source graph freshness for the usable analysis cache.
                // `None` when there is no graph to compare against.
                let graph_current: Option<bool> = graph.as_ref().map(|g| {
                    let src_abs = abs_string(src);
                    match graph_source_build_fingerprint(g.as_ref(), &src_abs) {
                        // Source present and stamped → compare exact fresh cache
                        // inputs, including analysis/model provenance.
                        Some(Some(stored)) => scan::load_records(src)
                            .and_then(|records| graph::build_input_fingerprint(&records))
                            .map(|current| current == stored)
                            .unwrap_or(false),
                        // Source present but no fingerprint (legacy graph), or the
                        // source isn't in the graph at all → the graph must be rebuilt.
                        _ => false,
                    }
                });
                if graph_current == Some(false) {
                    any_graph_stale = true;
                }
                if as_json {
                    let mut obj = status_json_obj(src, &report, has_enr, code);
                    obj["graph_current"] = match graph_current {
                        Some(b) => json!(b),
                        None => serde_json::Value::Null,
                    };
                    obj["graph_current_for_cache"] = obj["graph_current"].clone();
                    objs.push(obj);
                } else {
                    humans.push((src.clone(), report, has_enr, code, graph_current));
                }
            }

            // A missing graph (with sources configured) needs a build; so does any
            // stale Source. Graph-staleness is action-worthy (exit 1) even when the
            // caches are fully fresh.
            let graph_stale = !graph_present || any_graph_stale;
            let final_exit = worst.max(if graph_stale { 1 } else { 0 });
            let overall = if worst != 0 {
                status_label(worst)
            } else if graph_stale {
                "needs_build"
            } else {
                "fresh"
            };

            if as_json {
                let agg = json!({
                    "sources": objs,
                    "n_sources": sources.len(),
                    "needs_scan": worst != 0,
                    "graph": graph_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    "graph_present": graph_present,
                    "graph_stale": graph_stale,
                    "status": overall,
                    "exit_code": final_exit,
                });
                println!("{}", serde_json::to_string(&agg).expect("JSON value"));
            } else {
                println!("status over {} configured source(s):", sources.len());
                for (root, report, has_enr, code, graph_current) in &humans {
                    print_status_human(root, report, *has_enr, *code);
                    match graph_current {
                        Some(true) => println!("  graph:              current"),
                        Some(false) => println!("  graph:              STALE — run `sonagram build`"),
                        None => {}
                    }
                }
                println!("=> worst: {}", status_label(worst));
                if !graph_present {
                    println!("=> graph: not built yet — run `sonagram build`");
                } else if graph_stale {
                    println!("=> graph: STALE — run `sonagram build` (~1s from cache)");
                } else {
                    println!("=> graph: current");
                }
            }
            final_exit
        }
    }
}

/// The exact cached-analysis fingerprint stamped on the graph's `Source` node.
/// Returns `None` when there is no such `Source` node, `Some(None)` when the
/// node exists but carries no fingerprint (a legacy graph), and
/// `Some(Some(fp))` otherwise.
fn graph_source_build_fingerprint(
    graph: &kglite::api::DirGraph,
    source_root: &str,
) -> Option<Option<String>> {
    use kglite::api::cypher::resolve_node_property;
    use kglite::api::Value;
    let ni = graph.lookup_by_id_readonly("Source", &Value::String(source_root.to_string()))?;
    let node = graph.get_node(ni)?;
    let fp = match resolve_node_property(node, "build_input_fingerprint", graph) {
        Value::String(s) if !s.is_empty() => Some(s),
        _ => None,
    };
    Some(fp)
}

/// Probe one library root: `(exit_code, report, has_enrichment)`.
fn status_one(root: &Path) -> Result<(i32, FreshnessReport, bool)> {
    let report = scan::probe_freshness(root)?;
    // Enrichment presence: the Last.fm cache exists and carries at least one
    // non-empty map. Consistent with what `build` folds in.
    let has_enrichment = matches!(EnrichmentData::load(root), Ok(Some(e)) if !e.is_empty());
    Ok((status_exit_code(&report), report, has_enrichment))
}

/// The stable per-source status JSON object.
fn status_json_obj(
    root: &Path,
    report: &FreshnessReport,
    has_enrichment: bool,
    exit_code: i32,
) -> serde_json::Value {
    // P20: attach any live (or interrupted) progress snapshot, so one status
    // probe answers "is something running / how far did it get" too.
    let now = crate::progress::unix_now();
    let scan_progress = scan::load_scan_progress(root)
        .filter(|p| p.stage != "done")
        .map(|p| scan_progress_json(&p, now));
    let enrich_progress = enrich::load_enrich_progress(root)
        .filter(|p| p.kind != "done")
        .map(|p| enrich_progress_json(&p, now));
    json!({
        "library_root": root.to_string_lossy(),
        "scan_progress": scan_progress,
        "enrich_progress": enrich_progress,
        "has_cache": report.has_cache,
        "total_files": report.total_files,
        "fresh": report.fresh,
        "stale": report.stale,
        "missing_from_index": report.missing_from_index,
        "deleted_in_index": report.deleted_in_index,
        "has_enrichment": has_enrichment,
        "schema_version": sonara::analyze::ANALYSIS_SCHEMA_VERSION,
        "similarity_version": sonara::similarity::SIMILARITY_VERSION,
        "needs_scan": exit_code == 1,
        "status": status_label(exit_code),
        "exit_code": exit_code,
    })
}

/// Print the human-readable status block for one source.
fn print_status_human(root: &Path, report: &FreshnessReport, has_enrichment: bool, exit_code: i32) {
    println!("status for {}", root.display());
    println!(
        "  cache:              {}",
        if report.has_cache {
            "present"
        } else {
            "absent — run `sonagram scan`"
        }
    );
    println!("  total files:        {}", report.total_files);
    println!("  fresh:              {}", report.fresh);
    println!("  stale:              {}", report.stale);
    println!("  new (unindexed):    {}", report.missing_from_index);
    println!("  deleted:            {}", report.deleted_in_index);
    println!(
        "  enrichment cache:   {}",
        if has_enrichment { "present" } else { "absent" }
    );
    let now = crate::progress::unix_now();
    if let Some(p) = scan::load_scan_progress(root).filter(|p| p.stage != "done") {
        println!("  scan progress:      {}", scan_progress_human(&p, now));
    }
    if let Some(p) = enrich::load_enrich_progress(root).filter(|p| p.kind != "done") {
        println!("  enrich progress:    {}", enrich_progress_human(&p, now));
    }
    match exit_code {
        0 => println!("  => fresh"),
        2 => println!("  => no cache (run `sonagram scan {}`)", root.display()),
        _ => println!("  => needs scan (run `sonagram scan {}`)", root.display()),
    }
}

/// The freshness exit code for a probe report: `2` when there is no cache at
/// all, `1` when any file is stale / unindexed / deleted, else `0`.
fn status_exit_code(report: &FreshnessReport) -> i32 {
    if !report.has_cache {
        2
    } else if report.stale > 0 || report.missing_from_index > 0 || report.deleted_in_index > 0 {
        1
    } else {
        0
    }
}

/// The stable `status` string for a freshness exit code.
fn status_label(exit_code: i32) -> &'static str {
    match exit_code {
        0 => "fresh",
        2 => "no_cache",
        _ => "needs_scan",
    }
}

/// The playlist name for a copy-folder `.m3u8`: the `--out` file stem when
/// given, else the destination folder's own name, else `"playlist"`.
fn playlist_name(out: Option<&Path>, dest_dir: &Path) -> String {
    out.and_then(|p| p.file_stem())
        .or_else(|| dest_dir.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "playlist".to_string())
}

// ───────────────────────────── helpers ──────────────────────────────

/// Print a scan stage line to stderr roughly every 1% (at least every 500
/// items) plus every stage boundary — the multi-hour analyze phase must never
/// go silent (the old boundary-only rule left it stuck on `Analyze 0/N`).
/// The on-disk snapshot (`sonagram progress`) is the machine-readable view.
fn stage_line(p: ScanProgress) {
    let step = (p.total / 100).clamp(1, 500);
    if p.done == p.total || p.done.is_multiple_of(step) {
        eprintln!("[scan] {:?} {}/{}", p.stage, p.done, p.total);
    }
}

/// Print an enrich progress line to stderr, throttled like [`stage_line`].
fn enrich_line(p: EnrichProgress) {
    let step = (p.total / 100).clamp(1, 500);
    if p.done == p.total || p.done == 1 || p.done.is_multiple_of(step) {
        eprintln!("[enrich] {:?} {}/{}", p.kind, p.done, p.total);
    }
}

/// `sonagram progress [<library_root>] [--format json]` — read the on-disk
/// scan/enrich progress snapshots (written live by any scan/enrich, whatever
/// the entry point) and render them with derived rate / ETA / staleness.
fn cmd_progress(args: &[String]) -> Result<()> {
    let mut as_json = false;
    let mut root: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                i += 1;
                match args.get(i).map(String::as_str) {
                    Some("json") => as_json = true,
                    Some("human") => as_json = false,
                    other => {
                        return Err(SonagramError::Config(format!(
                            "progress: --format expects 'json' or 'human', got {other:?}"
                        )))
                    }
                }
            }
            "--json" => as_json = true,
            other if other.starts_with("--") => {
                return Err(SonagramError::Config(format!(
                    "progress: unexpected argument '{other}'"
                )))
            }
            other => root = Some(other),
        }
        i += 1;
    }

    let roots: Vec<PathBuf> = match root {
        Some(r) => vec![PathBuf::from(r)],
        None => configured_source_paths(&Config::load()?)?,
    };

    let now = crate::progress::unix_now();
    let mut objs: Vec<serde_json::Value> = Vec::new();
    for root in &roots {
        let scan_p = scan::load_scan_progress(root);
        let enrich_p = enrich::load_enrich_progress(root);
        if as_json {
            objs.push(json!({
                "library_root": root.to_string_lossy(),
                "scan": scan_p.as_ref().map(|p| scan_progress_json(p, now)),
                "enrich": enrich_p.as_ref().map(|p| enrich_progress_json(p, now)),
            }));
            continue;
        }
        println!("progress for {}", root.display());
        match &scan_p {
            None => println!("  scan:    no scan has run"),
            Some(p) => println!("  scan:    {}", scan_progress_human(p, now)),
        }
        match &enrich_p {
            None => println!("  enrich:  no enrichment has run"),
            Some(p) => println!("  enrich:  {}", enrich_progress_human(p, now)),
        }
    }
    if as_json {
        println!(
            "{}",
            serde_json::to_string(&json!({ "sources": objs })).expect("JSON value")
        );
    }
    Ok(())
}

/// `(pct, rate/sec, eta_sec)` for a counter that started at `started_unix` and
/// was last updated at `updated_unix`. Rate/ETA are `None` until measurable.
fn derive_rate(done: usize, total: usize, started_unix: i64, updated_unix: i64) -> (f64, Option<f64>, Option<i64>) {
    let pct = if total > 0 {
        100.0 * done as f64 / total as f64
    } else {
        100.0
    };
    let elapsed = (updated_unix - started_unix).max(0) as f64;
    let rate = (elapsed > 0.0 && done > 0).then(|| done as f64 / elapsed);
    let eta = rate
        .filter(|r| *r > 0.0)
        .map(|r| ((total.saturating_sub(done)) as f64 / r).round() as i64);
    (pct, rate, eta)
}

/// `"3m20s"`-style rendering of a second count.
fn human_secs(s: i64) -> String {
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// A staleness marker for a snapshot that claims to be live but has not been
/// updated recently — the writing process is likely gone.
fn staleness(now: i64, updated_unix: i64, finished: bool) -> &'static str {
    if !finished && now - updated_unix > 120 {
        " [STALE — writer gone?]"
    } else {
        ""
    }
}

fn scan_progress_human(p: &scan::ScanProgressSnapshot, now: i64) -> String {
    if p.stage == "done" {
        return format!(
            "complete — {} files ({} analyzed, {} reused, {} failed)",
            p.total,
            p.analyzed,
            p.reused_stat + p.reused_hash,
            p.failed
        );
    }
    // ETA from the analysis counters — the dominant cost. While hashing is
    // still discovering (`stage == "hash"`), analyze_total is a lower bound,
    // so the ETA is one too; say so.
    let (pct, rate, eta) = derive_rate(p.analyze_done, p.analyze_total, p.started_unix, p.updated_unix);
    let discovering = p.stage == "hash";
    let mut line = format!(
        "{} — {}/{} files decided; analysis {}/{}{} ({:.0}%)",
        p.stage,
        p.done,
        p.total,
        p.analyze_done,
        p.analyze_total,
        if discovering { "+" } else { "" },
        pct
    );
    if let Some(r) = rate {
        line.push_str(&format!(", {:.1}/min", r * 60.0));
    }
    if let Some(e) = eta {
        line.push_str(&format!(
            ", eta {}{}",
            human_secs(e),
            if discovering { "+" } else { "" }
        ));
    }
    line.push_str(staleness(now, p.updated_unix, false));
    line
}

fn scan_progress_json(p: &scan::ScanProgressSnapshot, now: i64) -> serde_json::Value {
    let (pct, rate, eta) = derive_rate(p.analyze_done, p.analyze_total, p.started_unix, p.updated_unix);
    let mut obj = serde_json::to_value(p).expect("snapshot serializes");
    obj["analyze_pct"] = json!(pct);
    obj["analyze_per_sec"] = json!(rate);
    obj["eta_sec"] = json!(eta);
    obj["age_sec"] = json!(now - p.updated_unix);
    obj["live"] = json!(p.stage != "done" && now - p.updated_unix <= 120);
    obj
}

fn enrich_progress_human(p: &enrich::EnrichProgressSnapshot, now: i64) -> String {
    let fetched = p.artists_fetched + p.tracks_fetched + p.albums_fetched;
    let failed = p.artists_failed + p.tracks_failed + p.albums_failed;
    if p.kind == "done" {
        return format!("complete — {fetched} fetched, {failed} failed this run");
    }
    let (pct, rate, eta) = derive_rate(p.done, p.total, p.started_unix, p.updated_unix);
    let mut line = format!(
        "fetching {}s {}/{} ({:.0}%); run total {fetched} fetched, {failed} failed",
        p.kind, p.done, p.total, pct
    );
    if let Some(r) = rate {
        line.push_str(&format!(", {:.1}/min", r * 60.0));
    }
    if let Some(e) = eta {
        line.push_str(&format!(", eta {}", human_secs(e)));
    }
    line.push_str(staleness(now, p.updated_unix, false));
    line
}

fn enrich_progress_json(p: &enrich::EnrichProgressSnapshot, now: i64) -> serde_json::Value {
    let (pct, rate, eta) = derive_rate(p.done, p.total, p.started_unix, p.updated_unix);
    let mut obj = serde_json::to_value(p).expect("snapshot serializes");
    obj["pct"] = json!(pct);
    obj["per_sec"] = json!(rate);
    obj["eta_sec"] = json!(eta);
    obj["age_sec"] = json!(now - p.updated_unix);
    obj["live"] = json!(p.kind != "done" && now - p.updated_unix <= 120);
    obj
}

/// A short library label for the `Library` root node — the last path component
/// (never the full user directory tree; the scanner keeps paths relative).
fn library_label(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// The `idx`-th positional (non-`--`) argument, if present.
fn optional_positional(args: &[String], idx: usize) -> Option<&str> {
    args.get(idx)
        .map(String::as_str)
        .filter(|a| !a.starts_with("--"))
}

/// The configured source directories (P17), erroring with a helpful hint when the
/// registry is empty.
fn configured_source_paths(cfg: &Config) -> Result<Vec<PathBuf>> {
    if cfg.sources.is_empty() {
        return Err(SonagramError::Config(
            "no sources configured — add one with `sonagram sources add <dir>`".into(),
        ));
    }
    Ok(cfg.sources.iter().map(PathBuf::from).collect())
}

/// The absolute path of `p` as a string: canonicalized when the dir exists (so a
/// `Source` node id is stable across relative/symlinked spellings), else the path
/// as given.
fn abs_string(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// `"exists"` / `"missing"` for a config path report.
fn exists_note(p: &Path) -> &'static str {
    if p.exists() {
        "exists"
    } else {
        "missing"
    }
}

/// The first line of `s`, trimmed, for a one-line listing.
fn one_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

fn flag_value(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| SonagramError::Playlist(format!("{flag} requires a value")))
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .ok_or_else(|| SonagramError::Graph(format!("non-UTF-8 path: {}", p.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrich::TrackEnrich;
    use crate::scan::FreshnessReport;

    fn report(
        has_cache: bool,
        fresh: usize,
        stale: usize,
        missing: usize,
        deleted: usize,
    ) -> FreshnessReport {
        FreshnessReport {
            total_files: fresh + stale + missing,
            fresh,
            stale,
            missing_from_index: missing,
            deleted_in_index: deleted,
            has_cache,
        }
    }

    #[test]
    fn no_cache_is_exit_two() {
        assert_eq!(status_exit_code(&report(false, 0, 0, 0, 0)), 2);
        // Even with files present, no index means no cache.
        assert_eq!(status_exit_code(&report(false, 3, 0, 0, 0)), 2);
        assert_eq!(status_label(2), "no_cache");
    }

    #[test]
    fn all_fresh_is_exit_zero() {
        assert_eq!(status_exit_code(&report(true, 10, 0, 0, 0)), 0);
        // A fully empty but present cache is still "fresh" (nothing to do).
        assert_eq!(status_exit_code(&report(true, 0, 0, 0, 0)), 0);
        assert_eq!(status_label(0), "fresh");
    }

    #[test]
    fn any_drift_is_exit_one() {
        assert_eq!(status_exit_code(&report(true, 5, 1, 0, 0)), 1); // stale
        assert_eq!(status_exit_code(&report(true, 5, 0, 1, 0)), 1); // unindexed
        assert_eq!(status_exit_code(&report(true, 5, 0, 0, 1)), 1); // deleted
        assert_eq!(status_label(1), "needs_scan");
    }

    #[test]
    fn enrichment_merge_replaces_failed_duplicate_with_usable_record() {
        let mut dst = EnrichmentData::default();
        dst.tracks.insert(
            "same-hash".to_string(),
            TrackEnrich {
                fetched: true,
                failed: true,
                reason: Some("not found".to_string()),
                ..TrackEnrich::default()
            },
        );
        let mut src = EnrichmentData::default();
        src.tracks.insert(
            "same-hash".to_string(),
            TrackEnrich {
                fetched: true,
                listeners: Some(42),
                ..TrackEnrich::default()
            },
        );

        merge_enrichment(&mut dst, src);
        let merged = dst.tracks.get("same-hash").expect("merged track");
        assert!(!merged.failed);
        assert_eq!(merged.listeners, Some(42));
    }

    #[test]
    fn enrichment_merge_keeps_first_when_both_records_are_usable() {
        let mut dst = EnrichmentData::default();
        dst.tracks.insert(
            "same-hash".to_string(),
            TrackEnrich {
                fetched: true,
                listeners: Some(1),
                ..TrackEnrich::default()
            },
        );
        let mut src = EnrichmentData::default();
        src.tracks.insert(
            "same-hash".to_string(),
            TrackEnrich {
                fetched: true,
                listeners: Some(2),
                ..TrackEnrich::default()
            },
        );

        merge_enrichment(&mut dst, src);
        assert_eq!(dst.tracks["same-hash"].listeners, Some(1));
    }
}
