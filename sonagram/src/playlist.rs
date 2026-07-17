//! Native playlist materialization — turn an agent's answer (a Cypher result or
//! an explicit list of `Track` ids) into a playable `.m3u8` file.
//!
//! sonagram is a mapper: the graph is the source of truth, and this module maps
//! a *chosen set of tracks, in a chosen order*, onto the extended-M3U format
//! players understand. It never reorders — playlist order is the agent's
//! decision (a Cypher `ORDER BY`, or the caller's id sequence), so the input
//! order is preserved verbatim.
//!
//! ## The `.m3u8` we write
//! `.m3u8` is UTF-8 extended M3U. We write, in order:
//! - a `#EXTM3U` header line;
//! - per track, a `#EXTINF:<seconds>,<label>` line then the absolute path line.
//!
//! `<seconds>` is the duration rounded to the nearest whole second, or `-1` when
//! unknown (the M3U "unknown length" sentinel). `<label>` degrades gracefully:
//! `Artist - Title`, else `Title`, else the file name. **No BOM** is written —
//! the `.m3u8` extension already declares UTF-8, and a leading BOM confuses some
//! players (VLC/mpv/foobar2000 all read BOM-less UTF-8 `.m3u8` correctly).
//!
//! ## Cypher result resolution
//! [`entries_from_cypher`] accepts either result shape an agent naturally
//! produces:
//! - a column of **Track nodes** (`RETURN t`) — the content hash is read from
//!   each node's `content_hash` property; or
//! - a **string column of content hashes** (`RETURN t.content_hash`).
//!
//! The id column is chosen by this rule (first match wins):
//! 1. a column named `content_hash` / `id` (exact, or the `t.content_hash`
//!    qualified form);
//! 2. otherwise the first column whose first row is a Track node or a string
//!    that resolves to an existing `Track` id.
//!
//! Row order is preserved; every extracted id is then resolved through
//! [`entries_from_graph`], so a hash that matches no `Track` surfaces in the
//! same "missing ids" error regardless of which input path produced it.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kglite::api::cypher::resolve_node_property;
use kglite::api::session::{execute_read, ExecuteOptions};
use kglite::api::{DirGraph, Value};
use serde::{Deserialize, Serialize};

use crate::curation::{
    CuratedPlaylist, PlaylistAudit, PlaylistBrief, PlaylistExplanation, PlaylistPolicy,
};
use crate::{Result, SonagramError};

/// The `Track` node type and its unique-id property (the audio content hash).
const TRACK: &str = "Track";
const ID_PROP: &str = "content_hash";

/// One resolved playlist entry: an absolute path plus the metadata the
/// `#EXTINF` line carries. All metadata is optional — a graph built from a
/// tag-less file still yields a playable entry (path only, `-1` duration).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistEntry {
    /// The audio content hash — the `Track` node id this entry resolved from.
    /// Recorded in the playlist-store metadata (P17) so a stored playlist can be
    /// re-resolved later.
    pub content_hash: String,
    /// Absolute path to the audio file on disk. Resolved from the `Track`'s
    /// `source_root` (P17) joined with its relative `path`, falling back to the
    /// `library_root` argument for graphs built before `source_root` existed.
    pub abs_path: PathBuf,
    /// Track duration in seconds, if known. `None` → `#EXTINF:-1`.
    pub duration_sec: Option<f32>,
    /// Track artist, if known.
    pub artist: Option<String>,
    /// Track title, if known.
    pub title: Option<String>,
}

/// Write `entries` to `out_path` as a UTF-8 (BOM-less) extended-M3U playlist.
///
/// Creates any missing parent directories. Rejects an empty `entries` list with
/// a clear error — an empty `.m3u8` is never what the caller wants (usually it
/// means a query matched nothing).
pub fn write_m3u8(entries: &[PlaylistEntry], out_path: &Path) -> Result<()> {
    if entries.is_empty() {
        return Err(SonagramError::Playlist(
            "refusing to write an empty playlist (no tracks to export — did the \
             query match anything?)"
                .to_string(),
        ));
    }

    let abs_lines: Vec<String> = entries
        .iter()
        .map(|e| e.abs_path.to_string_lossy().into_owned())
        .collect();
    let body = m3u8_body(entries, &abs_lines);

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    // UTF-8, no BOM: `String` bytes are written verbatim.
    fs::write(out_path, body)?;
    Ok(())
}

/// Build the extended-M3U body: a `#EXTM3U` header, then per entry an
/// `#EXTINF:<seconds>,<label>` line followed by its path line. `path_lines`
/// carries the exact text of each path line — the absolute path for
/// [`write_m3u8`], the bare copied filename (relative) for [`export_folder`] —
/// and must be the same length as `entries`. The `#EXTINF` formatting is shared
/// so the two writers never drift.
fn m3u8_body(entries: &[PlaylistEntry], path_lines: &[String]) -> String {
    debug_assert_eq!(entries.len(), path_lines.len());
    let mut body = String::from("#EXTM3U\n");
    for (entry, path_line) in entries.iter().zip(path_lines) {
        body.push_str(&extinf_line(entry));
        body.push('\n');
        body.push_str(path_line);
        body.push('\n');
    }
    body
}

/// The outcome of an [`export_folder`] run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderExportReport {
    /// Number of audio files copied into the destination folder.
    pub copied: usize,
    /// Total bytes copied (sum of the copied files' sizes).
    pub bytes: u64,
    /// Absolute-or-relative path to the written `.m3u8` inside the folder.
    pub playlist_path: PathBuf,
}

/// Characters forbidden in a filesystem component on the common platforms
/// (Windows/macOS/Linux union). Replaced with `_` during sanitization.
const FORBIDDEN_CHARS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Maximum bytes for a copied filename (stem + extension). 200 keeps well under
/// the 255-byte limit common filesystems impose on a single component while
/// leaving headroom for a ` (2)` dedupe suffix.
const MAX_FILENAME_BYTES: usize = 200;

/// Export a **self-contained, portable playlist folder**: copy each entry's
/// audio file into `dest_dir` as `NN - Artist - Title.<ext>` and write a
/// relative-path `<playlist_name>.m3u8` alongside them, so the folder can be
/// moved to any device and still play.
///
/// **Copies only** — source files are never moved, retagged, or otherwise
/// modified; this is explicitly *not* library maintenance. `dest_dir` is created
/// (`create_dir_all`); a same-named file already there is overwritten. The
/// `.m3u8` uses the same `#EXTM3U`/`#EXTINF` format as [`write_m3u8`] but its
/// path lines are the bare copied filenames (relative), so the folder is
/// self-referential.
///
/// On a per-file copy failure the error lists **all** failures (mirroring the
/// missing-ids style); any files copied before the failure are left in place
/// (partial folder — the caller may retry or clean up).
pub fn export_folder(
    entries: &[PlaylistEntry],
    dest_dir: &Path,
    playlist_name: &str,
) -> Result<FolderExportReport> {
    if entries.is_empty() {
        return Err(SonagramError::Playlist(
            "refusing to export an empty playlist folder (no tracks to copy — did \
             the query match anything?)"
                .to_string(),
        ));
    }

    fs::create_dir_all(dest_dir)?;

    // Resolve every copied filename up-front (position-prefixed, sanitized,
    // deduped) so the .m3u8 path lines match the files exactly.
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut filenames: Vec<String> = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let base = copy_filename(entry, i + 1);
        filenames.push(dedupe_name(&base, &mut used));
    }

    let mut copied = 0usize;
    let mut bytes = 0u64;
    let mut failures: Vec<String> = Vec::new();
    for (entry, name) in entries.iter().zip(&filenames) {
        let dest = dest_dir.join(name);
        match fs::copy(&entry.abs_path, &dest) {
            Ok(n) => {
                copied += 1;
                bytes += n;
            }
            Err(e) => failures.push(format!("{}: {e}", entry.abs_path.display())),
        }
    }

    if !failures.is_empty() {
        return Err(SonagramError::Playlist(format!(
            "{} file(s) failed to copy into {} (partial copies left in place): [{}]",
            failures.len(),
            dest_dir.display(),
            failures.join(", ")
        )));
    }

    // Write the relative-path playlist inside the folder.
    let body = m3u8_body(entries, &filenames);
    let pl_name = {
        let s = sanitize_component(playlist_name);
        if s.is_empty() { "playlist".to_string() } else { s }
    };
    let playlist_path = dest_dir.join(format!("{pl_name}.m3u8"));
    fs::write(&playlist_path, body)?;

    Ok(FolderExportReport {
        copied,
        bytes,
        playlist_path,
    })
}

/// Build the copied filename for `entry` at 1-based `position`:
/// `NN - Artist - Title.<ext>`, sanitized and byte-capped. Falls back to the
/// source file's stem when artist/title are both absent, and preserves the
/// source extension. CJK and other printable Unicode survive verbatim (no
/// ASCII-folding).
fn copy_filename(entry: &PlaylistEntry, position: usize) -> String {
    let prefix = format!("{position:02} - ");

    let ext_suffix = entry
        .abs_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", sanitize_component(e)))
        .filter(|s| s.len() > 1)
        .unwrap_or_default();

    let artist = entry.artist.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let title = entry.title.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let stem = match (artist, title) {
        (Some(a), Some(t)) => format!("{} - {}", sanitize_component(a), sanitize_component(t)),
        (None, Some(t)) => sanitize_component(t),
        // No title → fall back to the source file's stem.
        _ => entry
            .abs_path
            .file_stem()
            .map(|s| sanitize_component(&s.to_string_lossy()))
            .filter(|s| !s.is_empty())
            .unwrap_or_default(),
    };
    let stem = if stem.is_empty() { "track".to_string() } else { stem };

    // Cap the whole filename at MAX_FILENAME_BYTES, truncating the stem on a
    // UTF-8 boundary and preserving the NN prefix + extension.
    let budget = MAX_FILENAME_BYTES.saturating_sub(prefix.len() + ext_suffix.len());
    let stem = truncate_on_char_boundary(&stem, budget);
    // Truncation can re-expose a trailing dot/space — re-trim the ends.
    let stem = stem.trim_matches(|c: char| c == '.' || c == ' ');
    let stem = if stem.is_empty() { "track" } else { stem };

    format!("{prefix}{stem}{ext_suffix}")
}

/// Sanitize one filename component: replace filesystem-forbidden characters and
/// control characters with `_`, then trim leading/trailing dots and spaces
/// (which some filesystems treat specially). Interior Unicode — including CJK —
/// passes through unchanged.
fn sanitize_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if FORBIDDEN_CHARS.contains(&ch) || ch.is_control() {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    out.trim_matches(|c: char| c == '.' || c == ' ').to_string()
}

/// Truncate `s` to at most `max` bytes, never splitting a UTF-8 code point.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Return a filename not already present in `used` (compared case-insensitively,
/// since macOS/Windows filesystems are case-insensitive), appending ` (2)`,
/// ` (3)`, … before the extension until unique. Records the chosen name.
fn dedupe_name(base: &str, used: &mut BTreeSet<String>) -> String {
    let mut candidate = base.to_string();
    let mut n = 2;
    while used.contains(&candidate.to_lowercase()) {
        candidate = with_dedupe_suffix(base, n);
        n += 1;
    }
    used.insert(candidate.to_lowercase());
    candidate
}

/// Insert ` (n)` before the extension of `filename` (or at the end when there is
/// no extension). A leading dot (hidden file) is not treated as an extension.
fn with_dedupe_suffix(filename: &str, n: usize) -> String {
    match filename.rfind('.') {
        Some(dot) if dot > 0 && dot < filename.len() - 1 => {
            format!("{} ({}){}", &filename[..dot], n, &filename[dot..])
        }
        _ => format!("{filename} ({n})"),
    }
}

/// The `#EXTINF:<seconds>,<label>` line for one entry (no trailing newline).
fn extinf_line(entry: &PlaylistEntry) -> String {
    let secs = match entry.duration_sec {
        Some(d) if d.is_finite() && d >= 0.0 => d.round() as i64,
        _ => -1,
    };
    format!("#EXTINF:{secs},{}", label(entry))
}

/// The human-readable label for an entry: `Artist - Title`, else `Title`, else
/// the file name. Blank artist/title are treated as absent.
fn label(entry: &PlaylistEntry) -> String {
    let artist = entry.artist.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let title = entry.title.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match (artist, title) {
        (Some(a), Some(t)) => format!("{a} - {t}"),
        (None, Some(t)) => t.to_string(),
        // No title → fall back to the file name (the last path component).
        _ => entry
            .abs_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

/// Resolve `track_ids` (content hashes) into [`PlaylistEntry`]s, **preserving
/// the input order** — playlist order is the caller's choice.
///
/// Reads each `Track`'s `path` (relative to the library root), `duration_sec`,
/// `artist_name` and `title` properties, joining the relative path onto
/// `library_root` to recover the absolute on-disk path. If *any* id resolves to
/// no `Track`, returns an error listing **all** missing ids (not just the first)
/// so the caller can fix the whole set at once.
pub fn entries_from_graph(
    graph: &DirGraph,
    library_root: &Path,
    track_ids: &[String],
) -> Result<Vec<PlaylistEntry>> {
    let mut entries = Vec::with_capacity(track_ids.len());
    let mut missing = Vec::new();

    for id in track_ids {
        let node_idx = match graph.lookup_by_id_readonly(TRACK, &Value::String(id.clone())) {
            Some(ni) => ni,
            None => {
                missing.push(id.clone());
                continue;
            }
        };
        let node = match graph.get_node(node_idx) {
            Some(n) => n,
            None => {
                missing.push(id.clone());
                continue;
            }
        };

        let rel_path = prop_string(node, "path", graph).unwrap_or_default();
        // P17: prefer the Track's own absolute `source_root`; fall back to the
        // caller's `library_root` for pre-P17 graphs that carry no source_root.
        let abs_path = match prop_string(node, "source_root", graph) {
            Some(src) => Path::new(&src).join(&rel_path),
            None => library_root.join(&rel_path),
        };
        entries.push(PlaylistEntry {
            content_hash: id.clone(),
            abs_path,
            duration_sec: prop_f32(node, "duration_sec", graph),
            artist: prop_string(node, "artist_name", graph),
            title: prop_string(node, "title", graph),
        });
    }

    if !missing.is_empty() {
        return Err(SonagramError::Playlist(format!(
            "{} track id(s) not found in graph: [{}]",
            missing.len(),
            missing.join(", ")
        )));
    }
    Ok(entries)
}

/// Run a **read-only** Cypher `query` against `graph` and resolve its answer to
/// [`PlaylistEntry`]s, preserving row order.
///
/// See the module docs for the id-column resolution rule. The query must be
/// read-only ([`execute_read`] rejects mutations). No parameters are bound.
pub fn entries_from_cypher(
    graph: &DirGraph,
    library_root: &Path,
    query: &str,
) -> Result<Vec<PlaylistEntry>> {
    let params: HashMap<String, Value> = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let outcome = execute_read(graph, query, &opts)
        .map_err(|e| SonagramError::Graph(format!("cypher query failed: {e}")))?;

    // Eager execution materializes a bare-node return as `Value::Node` (the
    // transient `Value::NodeRef` never reaches the output here), so we read node
    // properties directly and do not run `resolve_noderefs` — that helper would
    // collapse a node to its *title*, discarding the content hash we need.
    let ids = extract_track_ids(graph, &outcome.result.columns, &outcome.result.rows)?;
    entries_from_graph(graph, library_root, &ids)
}

/// Pick the id column, then pull one content hash per row (order preserved).
fn extract_track_ids(
    graph: &DirGraph,
    columns: &[String],
    rows: &[Vec<Value>],
) -> Result<Vec<String>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let col = choose_id_column(graph, columns, rows)?;
    let mut ids = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        match row.get(col).and_then(id_from_value) {
            Some(id) => ids.push(id),
            None => {
                return Err(SonagramError::Playlist(format!(
                    "Cypher result row {i} has no Track id in the '{}' column \
                     (expected a Track node or a content-hash string)",
                    columns.get(col).map(String::as_str).unwrap_or("?")
                )))
            }
        }
    }
    Ok(ids)
}

/// Choose which result column carries the Track id (see module-doc rule).
fn choose_id_column(graph: &DirGraph, columns: &[String], rows: &[Vec<Value>]) -> Result<usize> {
    // 1. A column explicitly named for the id, in bare or qualified form.
    for (i, name) in columns.iter().enumerate() {
        let n = name.to_ascii_lowercase();
        if n == ID_PROP
            || n == "id"
            || n.ends_with(&format!(".{ID_PROP}"))
            || n.ends_with(".id")
        {
            return Ok(i);
        }
    }
    // 2. The first column whose first row is a Track node or a resolvable id.
    let first = &rows[0];
    for (i, value) in first.iter().enumerate() {
        if value_as_track_id(graph, value).is_some() {
            return Ok(i);
        }
    }
    Err(SonagramError::Playlist(format!(
        "no Track id column in Cypher result (columns: {columns:?}); \
         RETURN a Track node (e.g. `RETURN t`) or its `t.content_hash`"
    )))
}

/// The content hash carried by `value`, if it is a Track node or a string that
/// resolves to an existing `Track` — used only for column auto-detection.
fn value_as_track_id(graph: &DirGraph, value: &Value) -> Option<String> {
    match value {
        Value::Node(node) => match node.properties.get(ID_PROP) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        },
        Value::String(s) => graph
            .lookup_by_id_readonly(TRACK, &Value::String(s.clone()))
            .map(|_| s.clone()),
        _ => None,
    }
}

/// The content hash carried by `value` — a Track node's `content_hash`, or the
/// string itself. Existence is checked later by [`entries_from_graph`] so a
/// genuinely-missing id flows into the "missing ids" error.
fn id_from_value(value: &Value) -> Option<String> {
    match value {
        Value::Node(node) => match node.properties.get(ID_PROP) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        },
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Read a non-empty string property from a node, or `None`.
fn prop_string(
    node: &kglite::api::NodeData,
    prop: &str,
    graph: &DirGraph,
) -> Option<String> {
    match resolve_node_property(node, prop, graph) {
        Value::String(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// Read a numeric property from a node as `f32`, or `None`.
fn prop_f32(node: &kglite::api::NodeData, prop: &str, graph: &DirGraph) -> Option<f32> {
    match resolve_node_property(node, prop, graph) {
        Value::Float64(v) => Some(v as f32),
        Value::Int64(v) => Some(v as f32),
        _ => None,
    }
}

// ─────────────────────────── central playlist store (P17) ───────────────────
//
// A named playlist is materialized into a **central store dir**: a `<slug>.m3u8`
// (absolute paths, directly openable in any music app) next to a
// `<slug>.meta.json` sidecar carrying enough to retrieve/re-run it later. Both
// are written together; the store is a flat directory of such pairs.

/// One track's metadata row inside a stored playlist's `<slug>.meta.json`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PlaylistTrackMeta {
    /// 1-based position in the playlist.
    pub position: usize,
    /// The audio content hash (Track node id).
    pub content_hash: String,
    /// Track artist, if known.
    pub artist: Option<String>,
    /// Track title, if known.
    pub title: Option<String>,
    /// Track duration in seconds, if known.
    pub duration_sec: Option<f32>,
}

/// Library-owned curation provenance persisted with a curated playlist.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CurationProvenance {
    pub brief: PlaylistBrief,
    pub policy: PlaylistPolicy,
    pub audit: PlaylistAudit,
    pub explanation: PlaylistExplanation,
    pub repair_attempts: usize,
}

/// The `<slug>.meta.json` sidecar of a stored playlist (P17).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PlaylistMeta {
    /// The human name the user (or agent) gave the playlist.
    pub name: String,
    /// Filesystem-safe kebab slug derived from `name` (collision-suffixed).
    pub slug: String,
    /// ISO-8601 UTC creation timestamp (e.g. `2026-07-17T09:30:00Z`). Wall-clock
    /// metadata — never part of any graph digest.
    pub created_at: String,
    /// Free-text request/brief this playlist answers (the `--description`).
    pub request: Option<String>,
    /// The Cypher query used to curate it, when curated by query.
    pub cypher: Option<String>,
    /// The explicit content-hash ids used, when curated by id list.
    pub ids: Option<Vec<String>>,
    /// Number of tracks.
    pub n_tracks: usize,
    /// Sum of the known track durations (whole seconds).
    pub total_duration_sec: i64,
    /// Per-track metadata rows, in playlist order.
    pub tracks: Vec<PlaylistTrackMeta>,
    /// The graph the playlist was resolved against.
    pub graph: String,
    /// The portable copy-folder that was also written, if any.
    pub copy_to: Option<String>,
    /// Library-owned policy, audit, and score provenance. Absent on legacy and
    /// manually materialized playlists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curation: Option<CurationProvenance>,
}

/// The outcome of [`save_playlist`]: where the two files landed + the metadata.
#[derive(Debug, Clone)]
pub struct StoredPlaylist {
    /// The chosen (collision-resolved) slug.
    pub slug: String,
    /// Absolute path of the written `<slug>.m3u8`.
    pub m3u8_path: PathBuf,
    /// Absolute path of the written `<slug>.meta.json`.
    pub meta_path: PathBuf,
    /// The metadata written.
    pub meta: PlaylistMeta,
}

/// Save a named playlist into the central store `dir`: write `<slug>.m3u8`
/// (absolute paths) and its `<slug>.meta.json` sidecar. The slug is derived from
/// `name` and suffixed (`-2`, `-3`, …) on collision with an existing pair.
///
/// Exactly one of `cypher` / `ids` describes how the set was curated (recorded in
/// the metadata); `copy_to` is the portable folder that was also written, if any.
#[allow(clippy::too_many_arguments)]
pub fn save_playlist(
    dir: &Path,
    name: &str,
    request: Option<&str>,
    cypher: Option<&str>,
    ids: Option<&[String]>,
    entries: &[PlaylistEntry],
    graph_path: &Path,
    copy_to: Option<&Path>,
) -> Result<StoredPlaylist> {
    save_playlist_inner(
        dir, name, request, cypher, ids, entries, graph_path, copy_to, None,
    )
}

/// Store a library-curated playlist and its complete policy/audit provenance.
/// Non-exportable results are rejected before the store directory is created.
#[allow(clippy::too_many_arguments)]
pub fn save_curated_playlist(
    dir: &Path,
    name: &str,
    request: Option<&str>,
    curated: &CuratedPlaylist,
    entries: &[PlaylistEntry],
    graph_path: &Path,
    copy_to: Option<&Path>,
) -> Result<StoredPlaylist> {
    if !curated.exportable || !curated.audit.passed {
        return Err(SonagramError::Playlist(
            "refusing to store a curated playlist that failed its library audit".into(),
        ));
    }
    let entry_ids: Vec<&str> = entries.iter().map(|entry| entry.content_hash.as_str()).collect();
    let curated_ids: Vec<&str> = curated.track_ids.iter().map(String::as_str).collect();
    if entry_ids != curated_ids {
        return Err(SonagramError::Playlist(
            "curated entries do not match the audited track ids and order".into(),
        ));
    }
    let provenance = CurationProvenance {
        brief: curated.brief.clone(),
        policy: curated.policy.clone(),
        audit: curated.audit.clone(),
        explanation: curated.explanation.clone(),
        repair_attempts: curated.repair_attempts,
    };
    save_playlist_inner(
        dir,
        name,
        request,
        None,
        Some(&curated.track_ids),
        entries,
        graph_path,
        copy_to,
        Some(provenance),
    )
}

#[allow(clippy::too_many_arguments)]
fn save_playlist_inner(
    dir: &Path,
    name: &str,
    request: Option<&str>,
    cypher: Option<&str>,
    ids: Option<&[String]>,
    entries: &[PlaylistEntry],
    graph_path: &Path,
    copy_to: Option<&Path>,
    curation: Option<CurationProvenance>,
) -> Result<StoredPlaylist> {
    if entries.is_empty() {
        return Err(SonagramError::Playlist(
            "refusing to store an empty playlist (no tracks — did the query match anything?)"
                .to_string(),
        ));
    }
    fs::create_dir_all(dir)?;
    let slug = unique_slug(dir, &slugify(name));

    let tracks: Vec<PlaylistTrackMeta> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| PlaylistTrackMeta {
            position: i + 1,
            content_hash: e.content_hash.clone(),
            artist: e.artist.clone(),
            title: e.title.clone(),
            duration_sec: e.duration_sec,
        })
        .collect();
    let total_duration_sec: i64 = entries
        .iter()
        .filter_map(|e| e.duration_sec)
        .filter(|d| d.is_finite() && *d >= 0.0)
        .map(|d| d.round() as i64)
        .sum();

    let meta = PlaylistMeta {
        name: name.to_string(),
        slug: slug.clone(),
        created_at: iso8601_utc_now(),
        request: request.map(str::to_string),
        cypher: cypher.map(str::to_string),
        ids: ids.map(|v| v.to_vec()),
        n_tracks: entries.len(),
        total_duration_sec,
        tracks,
        graph: graph_path.to_string_lossy().into_owned(),
        copy_to: copy_to.map(|p| p.to_string_lossy().into_owned()),
        curation,
    };

    let m3u8_path = dir.join(format!("{slug}.m3u8"));
    let meta_path = dir.join(format!("{slug}.meta.json"));
    write_m3u8(entries, &m3u8_path)?;
    let json = serde_json::to_string_pretty(&meta)
        .map_err(|e| SonagramError::Playlist(format!("serialize playlist meta: {e}")))?;
    fs::write(&meta_path, json)?;

    Ok(StoredPlaylist {
        slug,
        m3u8_path,
        meta_path,
        meta,
    })
}

/// List every stored playlist in `dir` (its `*.meta.json` files), **newest
/// first**. A missing dir yields an empty list. A meta file that fails to parse
/// is skipped (a stray file never breaks the listing).
pub fn list_playlists(dir: &Path) -> Result<Vec<PlaylistMeta>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut metas: Vec<PlaylistMeta> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(".meta.json"))
            .unwrap_or(false)
        {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(meta) = serde_json::from_str::<PlaylistMeta>(&text) {
                metas.push(meta);
            }
        }
    }
    // ISO-8601 sorts lexicographically = chronologically; newest first, slug tie.
    metas.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.slug.cmp(&b.slug))
    });
    Ok(metas)
}

/// Load one stored playlist's metadata by slug from `dir`.
pub fn load_playlist_meta(dir: &Path, slug: &str) -> Result<PlaylistMeta> {
    validate_slug(slug)?;
    let path = dir.join(format!("{slug}.meta.json"));
    let text = fs::read_to_string(&path).map_err(|e| {
        SonagramError::Playlist(format!("no stored playlist '{slug}' in {}: {e}", dir.display()))
    })?;
    serde_json::from_str(&text)
        .map_err(|e| SonagramError::Playlist(format!("parse {}: {e}", path.display())))
}

/// Update the free-text request of a stored playlist. The slug is validated
/// before any path is resolved, and the sidecar replacement is atomic.
pub fn update_playlist_request(
    dir: &Path,
    slug: &str,
    request: Option<&str>,
) -> Result<PlaylistMeta> {
    validate_slug(slug)?;
    let mut meta = load_playlist_meta(dir, slug)?;
    meta.request = request.map(str::to_string);
    let path = dir.join(format!("{slug}.meta.json"));
    let json = serde_json::to_string_pretty(&meta)
        .map_err(|e| SonagramError::Playlist(format!("serialize playlist meta: {e}")))?;
    let tmp = dir.join(format!(".{slug}.meta.json.tmp.{}", std::process::id()));
    fs::write(&tmp, json).map_err(|e| {
        SonagramError::Playlist(format!("write playlist metadata {}: {e}", tmp.display()))
    })?;
    fs::rename(&tmp, &path).map_err(|e| {
        SonagramError::Playlist(format!("replace playlist metadata {}: {e}", path.display()))
    })?;
    Ok(meta)
}

/// Delete a stored playlist pair. Returns `false` when neither file exists.
/// The slug is validated before any filesystem access.
pub fn delete_playlist(dir: &Path, slug: &str) -> Result<bool> {
    validate_slug(slug)?;
    let paths = [
        dir.join(format!("{slug}.m3u8")),
        dir.join(format!("{slug}.meta.json")),
    ];
    let mut removed = false;
    for path in paths {
        if path.exists() {
            fs::remove_file(&path).map_err(|e| {
                SonagramError::Playlist(format!("delete stored playlist {}: {e}", path.display()))
            })?;
            removed = true;
        }
    }
    Ok(removed)
}

fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slugify(slug) != slug {
        return Err(SonagramError::Playlist(format!(
            "invalid playlist slug '{slug}'"
        )));
    }
    Ok(())
}

/// Slugify a playlist name to a filesystem-safe kebab string: lowercase, keep
/// alphanumerics (including CJK, which `char::is_alphanumeric` accepts), collapse
/// every other run into a single `-`, trim leading/trailing `-`. Empty → `"playlist"`.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "playlist".to_string()
    } else {
        trimmed
    }
}

/// Return a slug not colliding with an existing `<slug>.m3u8` / `<slug>.meta.json`
/// in `dir`, appending `-2`, `-3`, … until free.
fn unique_slug(dir: &Path, base: &str) -> String {
    let taken = |s: &str| dir.join(format!("{s}.m3u8")).exists() || dir.join(format!("{s}.meta.json")).exists();
    if !taken(base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let cand = format!("{base}-{n}");
        if !taken(&cand) {
            return cand;
        }
        n += 1;
    }
}

/// The current time as an ISO-8601 UTC string, `YYYY-MM-DDTHH:MM:SSZ`.
fn iso8601_utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso8601_utc(secs)
}

/// Format whole seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ` (UTC).
/// Pure integer arithmetic (Howard Hinnant's civil-from-days), no `chrono`.
fn iso8601_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // days since 1970-01-01 → civil (y, m, d).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(dur: Option<f32>, artist: Option<&str>, title: Option<&str>, path: &str) -> PlaylistEntry {
        PlaylistEntry {
            content_hash: "h".to_string(),
            abs_path: PathBuf::from(path),
            duration_sec: dur,
            artist: artist.map(String::from),
            title: title.map(String::from),
        }
    }

    #[test]
    fn extinf_rounds_duration_to_nearest_second() {
        let e = entry(Some(183.4), Some("America"), Some("Tin Man"), "/m/a.mp3");
        assert_eq!(extinf_line(&e), "#EXTINF:183,America - Tin Man");
        let e = entry(Some(183.6), Some("America"), Some("Tin Man"), "/m/a.mp3");
        assert_eq!(extinf_line(&e), "#EXTINF:184,America - Tin Man");
    }

    #[test]
    fn extinf_unknown_duration_is_minus_one() {
        let e = entry(None, Some("A"), Some("T"), "/m/a.mp3");
        assert_eq!(extinf_line(&e), "#EXTINF:-1,A - T");
        // A NaN/negative duration is also treated as unknown.
        let e = entry(Some(f32::NAN), Some("A"), Some("T"), "/m/a.mp3");
        assert_eq!(extinf_line(&e), "#EXTINF:-1,A - T");
    }

    #[test]
    fn label_missing_artist_is_title_only() {
        let e = entry(Some(100.0), None, Some("Just the Title"), "/m/song.mp3");
        assert_eq!(extinf_line(&e), "#EXTINF:100,Just the Title");
        // A blank artist counts as absent.
        let e = entry(Some(100.0), Some("   "), Some("Just the Title"), "/m/song.mp3");
        assert_eq!(extinf_line(&e), "#EXTINF:100,Just the Title");
    }

    #[test]
    fn label_missing_both_falls_back_to_filename() {
        let e = entry(Some(100.0), None, None, "/music/library/song file.mp3");
        assert_eq!(extinf_line(&e), "#EXTINF:100,song file.mp3");
        // Blank title with an artist still falls back to the file name.
        let e = entry(Some(100.0), Some("Some Artist"), Some(""), "/music/x/only.mp3");
        assert_eq!(extinf_line(&e), "#EXTINF:100,only.mp3");
    }

    // ───────────────── portable playlist folder (P13) ──────────────────

    #[test]
    fn sanitize_replaces_forbidden_and_control_chars() {
        assert_eq!(sanitize_component("AC/DC"), "AC_DC");
        assert_eq!(sanitize_component(r#"a:b*c?"d<e>f|g\h"#), "a_b_c__d_e_f_g_h");
        // Control chars (tab, newline) become underscores.
        assert_eq!(sanitize_component("a\tb\nc"), "a_b_c");
        // Leading/trailing dots and spaces are trimmed; interior kept.
        assert_eq!(sanitize_component("  . hello . world .  "), "hello . world");
    }

    #[test]
    fn sanitize_preserves_cjk_verbatim() {
        // No ASCII-folding: CJK passes through byte-for-byte.
        assert_eq!(sanitize_component("薔薇と雨"), "薔薇と雨");
        assert_eq!(sanitize_component("布袋寅泰"), "布袋寅泰");
        // A forbidden char amid CJK is replaced, the rest survives.
        assert_eq!(sanitize_component("東京/大阪"), "東京_大阪");
    }

    #[test]
    fn truncate_respects_multibyte_boundaries() {
        // Each CJK char is 3 bytes in UTF-8. Cap at 7 bytes → 2 chars (6 bytes),
        // never a split 3rd char.
        let s = "薔薇と雨"; // 12 bytes
        let t = truncate_on_char_boundary(s, 7);
        assert_eq!(t, "薔薇");
        assert_eq!(t.len(), 6);
        // Cap >= len returns the whole string.
        assert_eq!(truncate_on_char_boundary(s, 100), s);
        // ASCII truncates exactly.
        assert_eq!(truncate_on_char_boundary("abcdef", 3), "abc");
    }

    #[test]
    fn copy_filename_position_artist_title_ext() {
        let e = entry(Some(230.0), Some("Bruno Mars"), Some("Marry You"), "/lib/04 Marry You.mp3");
        assert_eq!(copy_filename(&e, 4), "04 - Bruno Mars - Marry You.mp3");
    }

    #[test]
    fn copy_filename_cjk_intact() {
        let e = entry(Some(212.0), Some("布袋寅泰"), Some("薔薇と雨"), "/lib/08 薔薇と雨.mp3");
        assert_eq!(copy_filename(&e, 2), "02 - 布袋寅泰 - 薔薇と雨.mp3");
    }

    #[test]
    fn copy_filename_sanitizes_slashes() {
        let e = entry(Some(200.0), Some("AC/DC"), Some("Back in Black"), "/lib/x.mp3");
        assert_eq!(copy_filename(&e, 1), "01 - AC_DC - Back in Black.mp3");
    }

    #[test]
    fn copy_filename_missing_tags_falls_back_to_source_stem() {
        // No artist/title → use the source file stem, keep NN + ext.
        let e = entry(None, None, None, "/lib/some folder/mystery track.mp3");
        assert_eq!(copy_filename(&e, 7), "07 - mystery track.mp3");
        // Blank artist + present title → title only.
        let e = entry(Some(10.0), Some("   "), Some("Just Title"), "/lib/z.flac");
        assert_eq!(copy_filename(&e, 3), "03 - Just Title.flac");
    }

    #[test]
    fn copy_filename_truncates_long_stem_at_200_bytes() {
        let long_title = "é".repeat(300); // 2 bytes each → 600 bytes
        let e = PlaylistEntry {
            content_hash: "h".to_string(),
            abs_path: PathBuf::from("/lib/x.mp3"),
            duration_sec: None,
            artist: None,
            title: Some(long_title),
        };
        let name = copy_filename(&e, 1);
        assert!(name.len() <= MAX_FILENAME_BYTES, "capped: {} bytes", name.len());
        assert!(name.starts_with("01 - "));
        assert!(name.ends_with(".mp3"));
        // The stem is valid UTF-8 (no split code point) and non-empty.
        assert!(name.chars().count() > 5);
    }

    #[test]
    fn dedupe_appends_numeric_suffix_before_extension() {
        let mut used = BTreeSet::new();
        assert_eq!(dedupe_name("01 - A - T.mp3", &mut used), "01 - A - T.mp3");
        // Same base again → " (2)" before the extension.
        assert_eq!(dedupe_name("01 - A - T.mp3", &mut used), "01 - A - T (2).mp3");
        assert_eq!(dedupe_name("01 - A - T.mp3", &mut used), "01 - A - T (3).mp3");
        // Case-insensitive collision (macOS/Windows) also dedupes.
        assert_eq!(dedupe_name("01 - a - t.MP3", &mut used), "01 - a - t (4).MP3");
        // No extension → suffix appended at the end.
        let mut used2 = BTreeSet::new();
        assert_eq!(dedupe_name("noext", &mut used2), "noext");
        assert_eq!(dedupe_name("noext", &mut used2), "noext (2)");
    }

    #[test]
    fn export_folder_copies_and_writes_relative_m3u8() {
        let dir = std::env::temp_dir().join(format!(
            "sonagram-p13-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src = dir.join("src");
        let dest = dir.join("dest");
        std::fs::create_dir_all(&src).unwrap();

        // Two fake source files with known byte sizes.
        let a = src.join("a.mp3");
        let b = src.join("b.mp3");
        std::fs::write(&a, b"AAAA").unwrap(); // 4 bytes
        std::fs::write(&b, b"BBBBBB").unwrap(); // 6 bytes

        let entries = vec![
            PlaylistEntry {
                content_hash: "ha".to_string(),
                abs_path: a,
                duration_sec: Some(100.0),
                artist: Some("布袋寅泰".to_string()),
                title: Some("薔薇と雨".to_string()),
            },
            PlaylistEntry {
                content_hash: "hb".to_string(),
                abs_path: b,
                duration_sec: None,
                artist: Some("AC/DC".to_string()),
                title: Some("Back in Black".to_string()),
            },
        ];

        let report = export_folder(&entries, &dest, "My Set").unwrap();
        assert_eq!(report.copied, 2);
        assert_eq!(report.bytes, 10);
        assert_eq!(report.playlist_path, dest.join("My Set.m3u8"));

        // Copied files exist under sanitized, position-prefixed names.
        assert!(dest.join("01 - 布袋寅泰 - 薔薇と雨.mp3").exists());
        assert!(dest.join("02 - AC_DC - Back in Black.mp3").exists());

        // The .m3u8 path lines are the bare relative filenames, in order.
        let text = std::fs::read_to_string(&report.playlist_path).unwrap();
        assert!(!text.starts_with('\u{feff}'), "no BOM");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "#EXTM3U");
        assert_eq!(lines[1], "#EXTINF:100,布袋寅泰 - 薔薇と雨");
        assert_eq!(lines[2], "01 - 布袋寅泰 - 薔薇と雨.mp3");
        assert_eq!(lines[3], "#EXTINF:-1,AC/DC - Back in Black");
        assert_eq!(lines[4], "02 - AC_DC - Back in Black.mp3");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_folder_empty_is_rejected() {
        let dest = std::env::temp_dir().join(format!("sonagram-p13-empty-{}", std::process::id()));
        let err = export_folder(&[], &dest, "x").unwrap_err();
        assert!(matches!(err, SonagramError::Playlist(_)), "got {err:?}");
    }

    #[test]
    fn export_folder_missing_source_lists_failure() {
        let dir = std::env::temp_dir().join(format!(
            "sonagram-p13-fail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dest = dir.join("dest");
        let entries = vec![entry(
            Some(1.0),
            Some("A"),
            Some("T"),
            "/nonexistent/does-not-exist.mp3",
        )];
        let err = export_folder(&entries, &dest, "x").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does-not-exist.mp3"), "names the failing source: {msg}");
        assert!(msg.contains("failed to copy"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_empty_list_is_rejected() {
        let dir = std::env::temp_dir().join(format!("sonagram-pl-empty-{}", std::process::id()));
        let out = dir.join("empty.m3u8");
        let err = write_m3u8(&[], &out).unwrap_err();
        assert!(matches!(err, SonagramError::Playlist(_)), "got {err:?}");
        assert!(!out.exists(), "no file should be written for an empty list");
    }

    #[test]
    fn write_creates_parent_dirs_and_expected_content() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir()
            .join(format!("sonagram-pl-{}-{stamp}", std::process::id()))
            .join("nested/deeper");
        let out = dir.join("set.m3u8");
        assert!(!dir.exists(), "parent must not exist yet");

        let entries = vec![
            entry(Some(210.5), Some("America"), Some("Tin Man"), "/music/tin man.mp3"),
            entry(None, None, None, "/music/mystery.mp3"),
        ];
        write_m3u8(&entries, &out).unwrap();

        let text = std::fs::read_to_string(&out).unwrap();
        // No BOM.
        assert!(!text.starts_with('\u{feff}'), "must not start with a BOM");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "#EXTM3U");
        assert_eq!(lines[1], "#EXTINF:211,America - Tin Man");
        assert_eq!(lines[2], "/music/tin man.mp3");
        assert_eq!(lines[3], "#EXTINF:-1,mystery.mp3");
        assert_eq!(lines[4], "/music/mystery.mp3");

        let _ = std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("sonagram-pl-{}-{stamp}", std::process::id())),
        );
    }

    // ───────────────── central playlist store (P17) ──────────────────

    #[test]
    fn slugify_kebabs_and_defaults() {
        assert_eq!(slugify("My Focus Mix!"), "my-focus-mix");
        assert_eq!(slugify("  Songs like X (but calmer)  "), "songs-like-x-but-calmer");
        assert_eq!(slugify("薔薇と雨"), "薔薇と雨"); // CJK is alphanumeric, kept
        assert_eq!(slugify("***"), "playlist"); // nothing left → default
    }

    #[test]
    fn iso8601_epoch_is_stable() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        // 2026-07-17T09:30:00Z = 1_784_280_600 seconds since the epoch.
        assert_eq!(iso8601_utc(1_784_280_600), "2026-07-17T09:30:00Z");
        // A leap-day boundary: 2024-02-29T23:59:59Z = 1_709_251_199.
        assert_eq!(iso8601_utc(1_709_251_199), "2024-02-29T23:59:59Z");
    }

    #[test]
    fn store_writes_reads_and_dedupes_slug() {
        let dir = std::env::temp_dir().join(format!(
            "sonagram-p17store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let entries = vec![
            entry(Some(200.0), Some("A"), Some("T1"), "/m/a.mp3"),
            entry(Some(100.4), Some("B"), Some("T2"), "/m/b.mp3"),
        ];
        let ids = vec!["h1".to_string(), "h2".to_string()];

        let stored = save_playlist(
            &dir,
            "My Focus Mix!",
            Some("deep work session"),
            None,
            Some(&ids),
            &entries,
            Path::new("/g.kgl"),
            None,
        )
        .unwrap();
        assert_eq!(stored.slug, "my-focus-mix");
        assert!(stored.m3u8_path.exists(), "wrote .m3u8");
        assert!(stored.meta_path.exists(), "wrote .meta.json");

        // Metadata parses with the expected fields.
        let m = load_playlist_meta(&dir, "my-focus-mix").unwrap();
        assert_eq!(m.name, "My Focus Mix!");
        assert_eq!(m.n_tracks, 2);
        assert_eq!(m.total_duration_sec, 300, "200 + round(100.4)");
        assert_eq!(m.request.as_deref(), Some("deep work session"));
        assert!(m.cypher.is_none());
        assert_eq!(m.ids.as_deref(), Some(&["h1".to_string(), "h2".to_string()][..]));
        assert_eq!(m.tracks.len(), 2);
        assert_eq!(m.tracks[0].position, 1);
        assert_eq!(m.tracks[0].title.as_deref(), Some("T1"));
        assert!(m.curation.is_none(), "legacy/manual metadata stays compatible");

        // A second playlist of the same name collides → `-2` slug.
        let s2 = save_playlist(
            &dir, "My Focus Mix!", None, None, Some(&ids), &entries, Path::new("/g.kgl"), None,
        )
        .unwrap();
        assert_eq!(s2.slug, "my-focus-mix-2");

        // list_playlists returns both.
        let list = list_playlists(&dir).unwrap();
        assert_eq!(list.len(), 2, "both stored playlists listed");

        // An empty selection is rejected.
        assert!(save_playlist(&dir, "x", None, None, Some(&[]), &[], Path::new("/g.kgl"), None).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn playlist_meta_load_rejects_traversal_slug() {
        let dir = std::env::temp_dir();
        let err = load_playlist_meta(&dir, "../escape").unwrap_err();
        assert!(err.to_string().contains("invalid playlist slug"));
    }

    #[test]
    fn legacy_metadata_without_curation_deserializes() {
        let json = r#"{
            "name":"Legacy","slug":"legacy","created_at":"2026-01-01T00:00:00Z",
            "request":null,"cypher":null,"ids":["h"],"n_tracks":1,
            "total_duration_sec":120,"tracks":[{"position":1,"content_hash":"h",
            "artist":null,"title":null,"duration_sec":120.0}],
            "graph":"/g.kgl","copy_to":null
        }"#;
        let meta: PlaylistMeta = serde_json::from_str(json).unwrap();
        assert!(meta.curation.is_none());
    }

    #[test]
    fn playlist_store_update_delete_and_traversal_are_safe() {
        let parent = std::env::temp_dir().join(format!(
            "sonagram-store-crud-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let dir = parent.join("store");
        let entries = vec![entry(Some(120.0), Some("A"), Some("T"), "/m/a.mp3")];
        let ids = vec!["h".to_string()];
        save_playlist(
            &dir,
            "CRUD",
            None,
            None,
            Some(&ids),
            &entries,
            Path::new("/g.kgl"),
            None,
        )
        .unwrap();
        let updated = update_playlist_request(&dir, "crud", Some("updated request")).unwrap();
        assert_eq!(updated.request.as_deref(), Some("updated request"));

        let outside = parent.join("outside.meta.json");
        fs::write(&outside, "do not delete").unwrap();
        assert!(update_playlist_request(&dir, "../outside", Some("bad")).is_err());
        assert!(delete_playlist(&dir, "../outside").is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "do not delete");

        assert!(delete_playlist(&dir, "crud").unwrap());
        assert!(!dir.join("crud.m3u8").exists());
        assert!(!dir.join("crud.meta.json").exists());
        assert!(!delete_playlist(&dir, "crud").unwrap());
        let _ = fs::remove_dir_all(parent);
    }
}
