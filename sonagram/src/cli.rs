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

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::config::Config;
use crate::enrich::{self, EnrichOptions, EnrichmentData};
use crate::graph::{self, LibraryInfo, SourceInput};
use crate::playlist;
use crate::record::AnalysisRecord;
use crate::scan::{self, scan_library, FreshnessReport, ScanOptions, ScanProgress, ScanStage};
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
        "playlists" => finish(cmd_playlists(&args[1..])),
        "sources" => finish(cmd_sources(&args[1..])),
        "config" => finish(cmd_config(&args[1..])),
        "skill" => finish(cmd_skill(&args[1..])),
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
    scan     <library_root>\n\
             Scan a library (walk, hash, analyze unseen files) and cache\n\
             per-track analysis under <library_root>/.sonagram/. Prints a\n\
             scan report.\n\
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
    playlists            List stored playlists (from `playlist --name`).\n\
    playlists show <slug>  Full metadata + tracklist for one stored playlist.\n\
\n\
CONFIG-DRIVEN FORMS (no path args — fan out over configured sources):\n\
    sonagram scan                 scan every configured source\n\
    sonagram status               probe all sources + graph freshness; JSON adds\n\
                                  per-source `graph_current` + top-level\n\
                                  `graph_stale` (a stale graph is exit 1 even when\n\
                                  caches are fresh — rebuild with `sonagram build`)\n\
    sonagram enrich               enrich all sources\n\
    sonagram build                multi-source build → the configured graph\n\
    sonagram playlist (--cypher|--ids ...) --name <name> [--description <text>]\n\
                                  curate from the configured graph and store the\n\
                                  playlist (.m3u8 + .meta.json) centrally\n\
\n\
FLAGS:\n\
    -h, --help       Print this help\n\
    -V, --version    Print version\n\
\n\
EXAMPLES:\n\
    sonagram sources add ~/Music\n\
    sonagram scan\n\
    sonagram build\n\
    sonagram playlist --ids h1,h2,h3 --name 'Deep Focus' --description 'work playlist'\n\
    sonagram playlists\n\
    # explicit-path forms still work:\n\
    sonagram build ~/Music music.kgl\n\
    sonagram playlist ~/Music music.kgl \\\n\
        --cypher 'MATCH (t:Track) WHERE t.bpm > 120 RETURN t.content_hash ORDER BY t.energy' \\\n\
        --out set.m3u8"
    );
}

fn cmd_scan(args: &[String]) -> Result<()> {
    // Explicit `scan <library_root>` (backward-compatible) vs config-driven
    // `scan` (every configured source, sequentially).
    match optional_positional(args, 0) {
        Some(root) => {
            scan_one(&PathBuf::from(root))?;
        }
        None => {
            let sources = configured_source_paths(&Config::load()?)?;
            let mut totals = (0usize, 0usize, 0usize, 0usize, 0usize); // files, analyzed, hash, stat, failed
            for src in &sources {
                let r = scan_one(src)?;
                totals.0 += r.total_files;
                totals.1 += r.analyzed;
                totals.2 += r.reused_hash_match;
                totals.3 += r.reused_stat_match;
                totals.4 += r.failed.len();
            }
            println!("combined scan over {} source(s):", sources.len());
            println!("  total files:        {}", totals.0);
            println!("  analyzed (new):     {}", totals.1);
            println!("  reused (hash match):{}", totals.2);
            println!("  reused (stat match):{}", totals.3);
            println!("  failed:             {}", totals.4);
        }
    }
    Ok(())
}

/// Scan one library root and print its per-source report; return the report.
fn scan_one(root: &Path) -> Result<crate::scan::ScanReport> {
    let opts = ScanOptions {
        progress: Some(Box::new(stage_line)),
        ..Default::default()
    };
    let report = scan_library(root, &opts)?;

    println!("scan report for {}", root.display());
    println!("  total files:        {}", report.total_files);
    println!("  analyzed (new):     {}", report.analyzed);
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
        progress: Some(Box::new(|p: enrich::EnrichProgress| {
            // One line per kind boundary, so a large library stays readable.
            if p.done == p.total || p.done == 1 {
                eprintln!("[enrich] {:?} {}/{}", p.kind, p.done, p.total);
            }
        })),
    };
    let report = enrich::enrich_library(root, &opts)?;

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
    Ok(())
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
        eprintln!("[build] {} records from {}", records.len(), src.display());
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

/// Fold `src`'s enrichment maps into `dst`, first-writer-wins per key.
fn merge_enrichment(dst: &mut EnrichmentData, src: EnrichmentData) {
    for (k, v) in src.artists {
        dst.artists.entry(k).or_insert(v);
    }
    for (k, v) in src.tracks {
        dst.tracks.entry(k).or_insert(v);
    }
    for (k, v) in src.albums {
        dst.albums.entry(k).or_insert(v);
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
        Some(other) => Err(SonagramError::Playlist(format!(
            "playlists: unknown subcommand '{other}' — try `playlists` or `playlists show <slug>`"
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
            // P19: load the configured graph once (if built) so we can compare
            // each Source's stamped scan_fingerprint against the current on-disk
            // state — the graph self-describes its own freshness.
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
                // Per-source graph freshness: does the graph's Source node carry a
                // scan_fingerprint equal to the current disk state? `None` when
                // there is no graph to compare against.
                let graph_current: Option<bool> = graph.as_ref().map(|g| {
                    let src_abs = abs_string(src);
                    match graph_source_fingerprint(g.as_ref(), &src_abs) {
                        // Source present and stamped → compare to a fresh disk walk.
                        Some(Some(stored)) => scan::compute_scan_fingerprint(src)
                            .map(|disk| disk == stored)
                            .unwrap_or(false),
                        // Source present but no fingerprint (pre-P19 graph), or the
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

/// The `scan_fingerprint` stamped on the graph's `Source` node for `source_root`
/// (P19). Returns `None` when there is no such `Source` node, `Some(None)` when
/// the node exists but carries no fingerprint (a pre-P19 graph), and
/// `Some(Some(fp))` otherwise.
fn graph_source_fingerprint(
    graph: &kglite::api::DirGraph,
    source_root: &str,
) -> Option<Option<String>> {
    use kglite::api::cypher::resolve_node_property;
    use kglite::api::Value;
    let ni = graph.lookup_by_id_readonly("Source", &Value::String(source_root.to_string()))?;
    let node = graph.get_node(ni)?;
    let fp = match resolve_node_property(node, "scan_fingerprint", graph) {
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
    json!({
        "library_root": root.to_string_lossy(),
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

/// Print a coarse stage line to stderr — one per stage boundary (plus the
/// start of analysis), not per file, so a 10k-track scan stays readable.
fn stage_line(p: ScanProgress) {
    let at_boundary = p.done == p.total;
    let analyze_start = p.stage == ScanStage::Analyze && p.done == 0;
    if at_boundary || analyze_start {
        eprintln!("[scan] {:?} {}/{}", p.stage, p.done, p.total);
    }
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
}
