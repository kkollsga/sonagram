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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use kglite::api::cypher::resolve_node_property;
use kglite::api::session::{execute_read, ExecuteOptions};
use kglite::api::{DirGraph, Value};

use crate::{Result, SonagramError};

/// The `Track` node type and its unique-id property (the audio content hash).
const TRACK: &str = "Track";
const ID_PROP: &str = "content_hash";

/// One resolved playlist entry: an absolute path plus the metadata the
/// `#EXTINF` line carries. All metadata is optional — a graph built from a
/// tag-less file still yields a playable entry (path only, `-1` duration).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistEntry {
    /// Absolute path to the audio file on disk (library root + relative path).
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

    let mut body = String::from("#EXTM3U\n");
    for entry in entries {
        body.push_str(&extinf_line(entry));
        body.push('\n');
        body.push_str(&entry.abs_path.to_string_lossy());
        body.push('\n');
    }

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    // UTF-8, no BOM: `String` bytes are written verbatim.
    fs::write(out_path, body)?;
    Ok(())
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
        entries.push(PlaylistEntry {
            abs_path: library_root.join(rel_path),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(dur: Option<f32>, artist: Option<&str>, title: Option<&str>, path: &str) -> PlaylistEntry {
        PlaylistEntry {
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
}
