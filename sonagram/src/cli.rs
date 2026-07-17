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

use crate::enrich::{self, EnrichOptions, EnrichmentData};
use crate::graph::{self, LibraryInfo};
use crate::playlist;
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
FLAGS:\n\
    -h, --help       Print this help\n\
    -V, --version    Print version\n\
\n\
EXAMPLES:\n\
    sonagram scan  ~/Music\n\
    sonagram build ~/Music music.kgl\n\
    sonagram status ~/Music --format json\n\
    sonagram playlist ~/Music music.kgl \\\n\
        --cypher 'MATCH (t:Track) WHERE t.bpm > 120 RETURN t.content_hash ORDER BY t.energy' \\\n\
        --out set.m3u8\n\
    sonagram playlist ~/Music music.kgl \\\n\
        --ids h1,h2,h3 --copy-to ~/Desktop/roadtrip"
    );
}

fn cmd_scan(args: &[String]) -> Result<()> {
    let root = positional(args, 0, "scan", "<library_root>")?;
    let root = PathBuf::from(root);

    let opts = ScanOptions {
        progress: Some(Box::new(stage_line)),
        ..Default::default()
    };
    let report = scan_library(&root, &opts)?;

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
    Ok(())
}

fn cmd_enrich(args: &[String]) -> Result<()> {
    let root = positional(args, 0, "enrich", "<library_root>")?;
    let root = PathBuf::from(root);

    let opts = EnrichOptions {
        api_key: None,
        progress: Some(Box::new(|p: enrich::EnrichProgress| {
            // One line per kind boundary, so a large library stays readable.
            if p.done == p.total || p.done == 1 {
                eprintln!("[enrich] {:?} {}/{}", p.kind, p.done, p.total);
            }
        })),
    };
    let report = enrich::enrich_library(&root, &opts)?;

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
    let root = positional(args, 0, "build", "<library_root>")?;
    let out = positional(args, 1, "build", "<out.kgl>")?;
    let root = PathBuf::from(root);
    let out = PathBuf::from(out);

    eprintln!("[build] loading cached records from {}", root.display());
    let records = scan::load_records(&root)?;
    if records.is_empty() {
        return Err(SonagramError::Playlist(format!(
            "no cached records under {} — run `sonagram scan` first",
            root.display()
        )));
    }
    // Auto-load the Last.fm enrichment cache when present.
    let enrichment = EnrichmentData::load(&root)?;
    match &enrichment {
        Some(e) if !e.is_empty() => eprintln!(
            "[build] enriched build: {} artists, {} tracks",
            e.artists_present(),
            e.tracks_present()
        ),
        _ => eprintln!("[build] no enrichment cache — plain build"),
    }
    eprintln!("[build] {} records → building graph", records.len());
    let library = LibraryInfo {
        root: library_label(&root),
        n_tracks: records.len(),
    };
    let mut g = graph::build_graph_with_enrichment(&records, enrichment.as_ref(), &library)?;
    graph::save(&mut g, &out)?;
    println!(
        "built graph from {} tracks → {}",
        records.len(),
        out.display()
    );
    Ok(())
}

fn cmd_playlist(args: &[String]) -> Result<()> {
    // Local --help for the subcommand (its flags are non-trivial).
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }
    let root = positional(args, 0, "playlist", "<library_root>")?;
    let graph_path = positional(args, 1, "playlist", "<graph.kgl>")?;
    let root = PathBuf::from(root);

    let mut cypher: Option<String> = None;
    let mut ids: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut copy_to: Option<PathBuf> = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--cypher" => cypher = Some(flag_value(args, &mut i, "--cypher")?),
            "--ids" => ids = Some(flag_value(args, &mut i, "--ids")?),
            "--out" => out = Some(PathBuf::from(flag_value(args, &mut i, "--out")?)),
            "--copy-to" => copy_to = Some(PathBuf::from(flag_value(args, &mut i, "--copy-to")?)),
            other => {
                return Err(SonagramError::Playlist(format!(
                    "unexpected argument '{other}' to `playlist`"
                )))
            }
        }
        i += 1;
    }

    // --out is required unless --copy-to gives the playlist a home of its own.
    if out.is_none() && copy_to.is_none() {
        return Err(SonagramError::Playlist(
            "playlist: pass --out <file.m3u8> and/or --copy-to <dir>".into(),
        ));
    }
    if cypher.is_some() == ids.is_some() {
        return Err(SonagramError::Playlist(
            "playlist: pass exactly one of --cypher '<query>' or --ids <hashes>".into(),
        ));
    }

    let g = kglite::api::io::load_file(path_str(Path::new(graph_path))?)
        .map_err(|e| SonagramError::Graph(format!("load {graph_path}: {e}")))?;

    let entries = if let Some(q) = cypher {
        playlist::entries_from_cypher(g.as_ref(), &root, &q)?
    } else {
        let id_list: Vec<String> = ids
            .unwrap()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        playlist::entries_from_graph(g.as_ref(), &root, &id_list)?
    };

    // Optional absolute-path .m3u8 (honored even alongside --copy-to).
    if let Some(out) = &out {
        playlist::write_m3u8(&entries, out)?;
        println!("wrote {} tracks → {}", entries.len(), out.display());
    }

    // Optional portable copy-folder: copied audio + a relative-path .m3u8.
    if let Some(dir) = &copy_to {
        let name = playlist_name(out.as_deref(), dir);
        let report = playlist::export_folder(&entries, dir, &name)?;
        println!(
            "copied {} tracks ({} bytes) → {}",
            report.copied,
            report.bytes,
            report.playlist_path.display()
        );
    }

    Ok(())
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

    let root = match root {
        Some(r) => PathBuf::from(r),
        None => {
            eprintln!("error: status: missing <library_root>");
            return 1;
        }
    };

    let report = match scan::probe_freshness(&root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    // Enrichment presence: the Last.fm cache exists and carries at least one
    // non-empty map. Consistent with what `build` folds in.
    let has_enrichment = matches!(EnrichmentData::load(&root), Ok(Some(e)) if !e.is_empty());

    let exit_code = status_exit_code(&report);
    let status_str = status_label(exit_code);
    let needs_scan = exit_code == 1;

    if as_json {
        let obj = json!({
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
            "needs_scan": needs_scan,
            "status": status_str,
            "exit_code": exit_code,
        });
        println!("{}", serde_json::to_string(&obj).expect("JSON value"));
    } else {
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

    exit_code
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

fn positional<'a>(args: &'a [String], idx: usize, cmd: &str, name: &str) -> Result<&'a str> {
    args.get(idx)
        .map(String::as_str)
        .filter(|a| !a.starts_with("--"))
        .ok_or_else(|| SonagramError::Playlist(format!("{cmd}: missing {name}")))
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
