//! PyO3 bindings for sonagram.
//!
//! This crate is the thin `_sonagram` extension module. All pure-Rust logic
//! lives in the `sonagram` core crate (PyO3-free by design); this layer only
//! maps it to Python, mirroring sonara's binding discipline.
//!
//! ## The `.kgl`-bytes handoff (why `build` returns a real `kglite` graph)
//!
//! Two compiled extensions (this one and the separately installed `kglite`
//! wheel) can't share a live Rust `DirGraph` — each links its own copy of the
//! engine. So [`build`] hands the graph off through serialization: it builds
//! the `Arc<DirGraph>` natively, writes it to a `.kgl` file, then calls the
//! *Python* `kglite.load(path)` and returns that object — the same
//! `kglite.KnowledgeGraph` the `kglite` wheel produces, so every downstream API
//! (`.cypher()`, `.describe()`, …) works unchanged. This is the codingest
//! precedent. When `out_path` is given the `.kgl` is persisted (that's the file
//! the MCP server serves); otherwise a temp file carries the bytes and is
//! deleted once the graph is fully materialized in the loaded object — a
//! ~one-write round-trip cost over an in-memory return.
//!
//! ## Why `export_m3u` takes a `.kgl` path, not a graph object
//!
//! For the same reason: a live `kglite.KnowledgeGraph` is a foreign
//! extension's Rust object and cannot cross back into this extension. So
//! [`export_m3u`] loads the graph Rust-side from a `.kgl` path via
//! `kglite::api::io::load_file`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde::de::DeserializeOwned;
use serde::Serialize;

use kglite::api::DirGraph;
use sonagram::curation::{
    self as core_curation, PlaylistBrief, PlaylistPolicy, PlaylistPreset,
};
use sonagram::enrich::{self, EnrichOptions, EnrichReport, EnrichmentData};
use sonagram::graph::{self, LibraryInfo, SourceInput};
use sonagram::pipeline;
use sonagram::playlist;
use sonagram::scan as core_scan;
use sonagram::scan::{ScanOptions, ScanProgress, ScanReport, ScanStage};
use sonagram::SonagramError;

// ───────────────────────────── pure helpers (unit-tested) ───────────────────

/// The stage string passed to a Python progress callback, mapped from a
/// [`ScanStage`]. Stable — part of the public callback contract.
fn stage_name(stage: ScanStage) -> &'static str {
    match stage {
        ScanStage::Walk => "walk",
        ScanStage::Hash => "hash",
        ScanStage::Analyze => "analyze",
        ScanStage::Done => "done",
    }
}

/// A short label for the `Library` root node — the last path component (never
/// the full user directory tree; the scanner keeps paths relative). Mirrors the
/// CLI's `library_label`.
fn library_label(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// The resolved track selection for [`export_m3u`]: exactly one of a Cypher
/// query or an explicit id list.
#[derive(Debug, Clone, PartialEq)]
enum Selection {
    Cypher(String),
    Ids(Vec<String>),
}

/// Validate the mutually-exclusive `cypher` / `track_ids` arguments of
/// `export_m3u`, returning the chosen selection or an error message. Pure so it
/// is unit-testable without a Python interpreter.
fn validate_export_selection(
    cypher: Option<&str>,
    track_ids: Option<&[String]>,
) -> std::result::Result<Selection, String> {
    match (cypher, track_ids) {
        (Some(_), Some(_)) => Err(
            "export_m3u(): pass exactly one of `cypher=` or `track_ids=`, not both".to_string(),
        ),
        (None, None) => {
            Err("export_m3u(): pass exactly one of `cypher=` or `track_ids=`".to_string())
        }
        (Some(q), None) => {
            let q = q.trim();
            if q.is_empty() {
                Err("export_m3u(): `cypher=` is empty".to_string())
            } else {
                Ok(Selection::Cypher(q.to_string()))
            }
        }
        (None, Some(ids)) => {
            if ids.is_empty() {
                Err("export_m3u(): `track_ids=` is empty".to_string())
            } else {
                Ok(Selection::Ids(ids.to_vec()))
            }
        }
    }
}

// ───────────────────────────── error mapping ────────────────────────────────

/// Map a [`SonagramError`] to a Python exception: `ValueError` for
/// bad-argument / bad-input failures (playlist resolution: missing ids, empty
/// set, unusable Cypher result), `RuntimeError` for scan/graph/IO failures.
fn to_pyerr(err: SonagramError) -> PyErr {
    match &err {
        SonagramError::Playlist(_) => PyValueError::new_err(err.to_string()),
        _ => PyRuntimeError::new_err(err.to_string()),
    }
}

// ───────────────────────────── shared internals ─────────────────────────────

/// Run a library scan with the GIL released, re-attaching only to fire the
/// optional Python progress callback (`progress(stage: str, done: int,
/// total: int)`).
fn run_scan(
    py: Python<'_>,
    root: &Path,
    progress: Option<Py<PyAny>>,
) -> PyResult<ScanReport> {
    if let Some(cb) = &progress {
        if !cb.bind(py).is_callable() {
            return Err(PyValueError::new_err(
                "progress must be callable: progress(stage: str, done: int, total: int) -> None",
            ));
        }
    }

    let opts = ScanOptions {
        features: core_scan::default_features(),
        progress: progress.map(|cb| {
            Box::new(move |p: ScanProgress| {
                // Re-attach to the interpreter only to invoke the callback. A
                // raising callback must not abort the scan — drop its error.
                Python::attach(|py| {
                    let _ = cb.call1(py, (stage_name(p.stage), p.done, p.total));
                });
            }) as Box<dyn Fn(ScanProgress) + Send + Sync>
        }),
    };

    // Scan (walk + hash + sonara analysis) is CPU/IO-heavy pure Rust — release
    // the GIL so other Python threads run and the callback can re-attach.
    py.detach(|| core_scan::scan_library(root, &opts)).map_err(to_pyerr)
}

/// Load the cached records for `root` and build the graph, GIL released.
/// Auto-loads the Last.fm enrichment cache under `<root>/.sonagram/lastfm/` when
/// present, so a build after `enrich()` is enriched with no extra argument.
fn build_graph_from_lib(py: Python<'_>, root: &Path) -> PyResult<Arc<DirGraph>> {
    let root = root.to_path_buf();
    py.detach(move || {
        let records = core_scan::load_records(&root)?;
        if records.is_empty() {
            return Err(SonagramError::Graph(format!(
                "no cached records under {} — run sonagram.scan(...) first",
                root.display()
            )));
        }
        let enrichment = EnrichmentData::load(&root)?;
        // P17: the Source node id + every Track.source_root is the ABSOLUTE root
        // (so `export_m3u` resolves paths off source_root), while the Library node
        // keeps its short label.
        let source_root = std::fs::canonicalize(&root)
            .unwrap_or_else(|_| root.clone())
            .to_string_lossy()
            .into_owned();
        // P19: stamp the source's scan-state fingerprint onto its Source node so
        // `sonagram status` can tell whether the graph reflects the current disk.
        let scan_fingerprint = core_scan::load_scan_fingerprint(&root)?;
        let sources = [SourceInput {
            root: source_root,
            records: &records,
            scan_fingerprint,
        }];
        let library = LibraryInfo {
            root: library_label(&root),
            n_tracks: records.len(),
        };
        graph::build_graph_from_sources(&sources, enrichment.as_ref(), &library)
    })
    .map_err(to_pyerr)
}

/// Serialize the built graph to a `.kgl` and load it back through the *Python*
/// `kglite` wheel, returning that wheel's own `KnowledgeGraph` (the codingest
/// handoff). The GIL is held on entry (the `py.import` needs it); the save is
/// pure Rust.
fn handoff_via_kgl(
    py: Python<'_>,
    mut graph: Arc<DirGraph>,
    save_to: Option<PathBuf>,
) -> PyResult<Py<PyAny>> {
    match save_to {
        // Caller wants the `.kgl` persisted — write there and load it back.
        Some(path) => {
            graph::save(&mut graph, &path).map_err(to_pyerr)?;
            load_via_kglite(py, &path)
        }
        // No target path — carry the bytes through a temp file that is deleted
        // once the graph is fully materialized in the loaded object.
        None => {
            let tmp = tempfile::Builder::new()
                .prefix("sonagram-")
                .suffix(".kgl")
                .tempfile()
                .map_err(|e| PyRuntimeError::new_err(format!("temp file for handoff: {e}")))?;
            graph::save(&mut graph, tmp.path()).map_err(to_pyerr)?;
            let obj = load_via_kglite(py, tmp.path())?;
            // `tmp` drops here, removing the file; the graph now lives entirely
            // inside the returned kglite object.
            Ok(obj)
        }
    }
}

/// Import the installed `kglite` wheel and call its top-level `load(path)`.
fn load_via_kglite(py: Python<'_>, path: &Path) -> PyResult<Py<PyAny>> {
    let kglite = py.import("kglite").map_err(|e| {
        PyRuntimeError::new_err(format!(
            "sonagram.build()/scan_and_build() return a kglite.KnowledgeGraph, but \
             importing `kglite` failed ({e}). Install it: `pip install kglite>=0.14`."
        ))
    })?;
    let graph = kglite.call_method1("load", (path.to_string_lossy().as_ref(),))?;
    Ok(graph.unbind())
}

/// Build the `dict` form of an [`EnrichReport`] — per-entity fetched/skipped/
/// failed counts and `elapsed_sec`.
fn enrich_report_to_dict<'py>(py: Python<'py>, r: &EnrichReport) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("artists_fetched", r.artists_fetched)?;
    d.set_item("artists_skipped", r.artists_skipped)?;
    d.set_item("artists_failed", r.artists_failed)?;
    d.set_item("tracks_fetched", r.tracks_fetched)?;
    d.set_item("tracks_skipped", r.tracks_skipped)?;
    d.set_item("tracks_failed", r.tracks_failed)?;
    d.set_item("albums_fetched", r.albums_fetched)?;
    d.set_item("albums_skipped", r.albums_skipped)?;
    d.set_item("albums_failed", r.albums_failed)?;
    d.set_item("elapsed_sec", r.elapsed.as_secs_f64())?;
    Ok(d)
}

/// Build the `dict` form of a [`ScanReport`] — plain counts, a `failed` list of
/// `(path, message)` tuples, and `elapsed_sec`.
fn report_to_dict<'py>(py: Python<'py>, report: &ScanReport) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("total_files", report.total_files)?;
    d.set_item("analyzed", report.analyzed)?;
    d.set_item("reused_hash_match", report.reused_hash_match)?;
    d.set_item("reused_stat_match", report.reused_stat_match)?;
    let failed = PyList::empty(py);
    for (path, msg) in &report.failed {
        failed.append((path.to_string_lossy().into_owned(), msg.clone()))?;
    }
    d.set_item("failed", failed)?;
    d.set_item("elapsed_sec", report.elapsed.as_secs_f64())?;
    Ok(d)
}

fn decode_json<T: DeserializeOwned>(raw: &str, label: &str) -> PyResult<T> {
    serde_json::from_str(raw)
        .map_err(|error| PyValueError::new_err(format!("{label} is not valid: {error}")))
}

fn from_python_json<T: DeserializeOwned>(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    label: &str,
) -> PyResult<T> {
    let raw: String = py.import("json")?.call_method1("dumps", (value,))?.extract()?;
    decode_json(&raw, label)
}

fn to_python_json<T: Serialize>(py: Python<'_>, value: &T) -> PyResult<Py<PyAny>> {
    let raw = serde_json::to_string(value)
        .map_err(|error| PyRuntimeError::new_err(format!("serialize result: {error}")))?;
    Ok(py
        .import("json")?
        .call_method1("loads", (raw,))?
        .unbind())
}

fn load_saved_graph(path: &Path) -> std::result::Result<Arc<DirGraph>, SonagramError> {
    let path_str = path
        .to_str()
        .ok_or_else(|| SonagramError::Graph(format!("non-UTF-8 path: {}", path.display())))?;
    kglite::api::io::load_file(path_str)
        .map_err(|error| SonagramError::Graph(format!("load {path_str}: {error}")))
}

fn resolve_curation_policy(
    brief: &PlaylistBrief,
    policy: Option<PlaylistPolicy>,
) -> std::result::Result<PlaylistPolicy, String> {
    let policy = policy.unwrap_or_else(|| PlaylistPolicy::for_preset(brief.preset));
    if policy.preset != brief.preset {
        return Err(format!(
            "brief preset {:?} does not match policy preset {:?}",
            brief.preset, policy.preset
        ));
    }
    Ok(policy)
}

fn validate_ordered_track_ids(track_ids: &[String]) -> std::result::Result<(), String> {
    if track_ids.is_empty() {
        return Err("track_ids must contain at least one content hash".into());
    }
    if track_ids.iter().any(|id| id.trim().is_empty()) {
        return Err("track_ids must not contain an empty content hash".into());
    }
    Ok(())
}

// ───────────────────────────── Python surface ───────────────────────────────

/// Scan a music library and return its [`ScanReport`] as a plain `dict`.
///
/// Walks `library_root` for MP3s, reuses cached analysis wherever the content
/// hash is unchanged, and analyzes only unseen files. `progress`, if given,
/// must be callable and is invoked as `progress(stage: str, done: int,
/// total: int)` where `stage` is one of `"walk"`, `"hash"`, `"analyze"`,
/// `"done"`.
///
/// Returns a dict: `total_files`, `analyzed`, `reused_hash_match`,
/// `reused_stat_match`, `failed` (list of `(path, message)`), `elapsed_sec`.
#[pyfunction]
#[pyo3(signature = (library_root, *, progress=None))]
fn scan(py: Python<'_>, library_root: PathBuf, progress: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
    let report = run_scan(py, &library_root, progress)?;
    Ok(report_to_dict(py, &report)?.into_any().unbind())
}

/// Scan a library while enriching it from Last.fm **in parallel** (P20).
///
/// The scan (CPU-heavy) runs on this thread's worker pool; an enrich loop
/// (network-heavy) concurrently re-passes the growing analysis cache, then a
/// final pass catches the tail. Without a resolvable Last.fm key the run
/// degrades to a plain scan and `enrich` is `None`. `progress` receives the
/// scan callbacks only — enrichment progress is observable via the on-disk
/// `enrich_progress.json` (`sonagram progress`).
///
/// Returns `{"scan": <scan report dict>, "enrich": <enrich report dict>|None}`.
#[pyfunction]
#[pyo3(signature = (library_root, *, api_key=None, progress=None))]
fn scan_and_enrich(
    py: Python<'_>,
    library_root: PathBuf,
    api_key: Option<String>,
    progress: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    if let Some(cb) = &progress {
        if !cb.bind(py).is_callable() {
            return Err(PyValueError::new_err(
                "progress must be callable: progress(stage: str, done: int, total: int) -> None",
            ));
        }
    }
    let scan_opts = ScanOptions {
        features: core_scan::default_features(),
        progress: progress.map(|cb| {
            Box::new(move |p: ScanProgress| {
                Python::attach(|py| {
                    let _ = cb.call1(py, (stage_name(p.stage), p.done, p.total));
                });
            }) as Box<dyn Fn(ScanProgress) + Send + Sync>
        }),
    };
    let enrich_opts = EnrichOptions {
        api_key,
        progress: None,
    };
    let combined = py
        .detach(|| pipeline::scan_and_enrich_library(&library_root, &scan_opts, &enrich_opts))
        .map_err(to_pyerr)?;
    let d = PyDict::new(py);
    d.set_item("scan", report_to_dict(py, &combined.scan)?)?;
    match &combined.enrich {
        Some(e) => d.set_item("enrich", enrich_report_to_dict(py, e)?)?,
        None => d.set_item("enrich", py.None())?,
    }
    Ok(d.into_any().unbind())
}

/// Fetch Last.fm enrichment for a library and cache it under
/// `<library_root>/.sonagram/lastfm/`.
///
/// Resolves the API key from `api_key=` (if given), else the `LASTFM_API_KEY`
/// env var, else a `.env` file in the current dir or the library root; a missing
/// key raises `RuntimeError`. Fetches popularity, folksonomy tags, MBIDs,
/// similar artists/tracks (with match weights), and original-album mapping for
/// every artist/track/album not already cached (incremental). Per-entity
/// failures are soft (recorded in the cache), never fatal.
///
/// Returns a dict of per-entity `*_fetched` / `*_skipped` / `*_failed` counts
/// plus `elapsed_sec`. After this, `build()` / `scan_and_build()` pick the cache
/// up automatically.
#[pyfunction]
#[pyo3(name = "enrich", signature = (library_root, *, api_key=None))]
fn enrich_(py: Python<'_>, library_root: PathBuf, api_key: Option<String>) -> PyResult<Py<PyAny>> {
    let opts = EnrichOptions {
        api_key,
        progress: None,
    };
    // The fetch is blocking IO/network pure Rust — release the GIL.
    let report = py
        .detach(|| enrich::enrich_library(&library_root, &opts))
        .map_err(to_pyerr)?;
    Ok(enrich_report_to_dict(py, &report)?.into_any().unbind())
}

/// Build the knowledge graph from a library's cached analysis records and
/// return a live `kglite.KnowledgeGraph`.
///
/// Run [`scan`] first (this reads the cache under `<library_root>/.sonagram/`).
/// If `out_path` is given the `.kgl` is written there and kept — that is the
/// file `kglite-mcp-server --graph` serves. Otherwise the graph is carried
/// through a temporary `.kgl` (deleted once loaded); see the module docs for
/// the round-trip cost.
#[pyfunction]
#[pyo3(signature = (library_root, out_path=None))]
fn build(
    py: Python<'_>,
    library_root: PathBuf,
    out_path: Option<PathBuf>,
) -> PyResult<Py<PyAny>> {
    let graph = build_graph_from_lib(py, &library_root)?;
    handoff_via_kgl(py, graph, out_path)
}

/// Convenience composition of [`scan`] then [`build`] over one library.
///
/// Scans `library_root` (forwarding `progress`), then builds and returns the
/// `kglite.KnowledgeGraph`, persisting the `.kgl` to `out_path` when given.
#[pyfunction]
#[pyo3(signature = (library_root, out_path=None, *, progress=None))]
fn scan_and_build(
    py: Python<'_>,
    library_root: PathBuf,
    out_path: Option<PathBuf>,
    progress: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    let _ = run_scan(py, &library_root, progress)?;
    let graph = build_graph_from_lib(py, &library_root)?;
    handoff_via_kgl(py, graph, out_path)
}

/// Export a playlist from a saved graph.
///
/// Loads the graph from `kgl_path`, resolves a track set — pass **exactly one**
/// of `cypher=` (a read-only query returning a Track-node or content-hash
/// column) or `track_ids=` (content hashes, order preserved) — and joins each
/// track's relative path onto `library_root`.
///
/// Always writes a UTF-8 extended-M3U playlist (absolute paths) to `out_path`.
/// When `copy_to=` is given, ALSO exports a self-contained **portable folder**
/// there: the tracks copied as `NN - Artist - Title.<ext>` next to a
/// relative-path `.m3u8` (named after `out_path`'s stem). Copies only — source
/// files are never moved, retagged, or modified.
///
/// Returns `str(copy_to's playlist path)` when `copy_to=` is set, else
/// `str(out_path)`.
#[pyfunction]
#[pyo3(signature = (kgl_path, library_root, out_path, *, cypher=None, track_ids=None, copy_to=None))]
fn export_m3u(
    py: Python<'_>,
    kgl_path: PathBuf,
    library_root: PathBuf,
    out_path: PathBuf,
    cypher: Option<String>,
    track_ids: Option<Vec<String>>,
    copy_to: Option<PathBuf>,
) -> PyResult<String> {
    let selection = validate_export_selection(cypher.as_deref(), track_ids.as_deref())
        .map_err(PyValueError::new_err)?;

    py.detach(move || {
        let g = load_saved_graph(&kgl_path)?;
        let entries = match &selection {
            Selection::Cypher(q) => playlist::entries_from_cypher(g.as_ref(), &library_root, q)?,
            Selection::Ids(ids) => playlist::entries_from_graph(g.as_ref(), &library_root, ids)?,
        };
        playlist::write_m3u8(&entries, &out_path)?;

        // Optional portable copy-folder: copied audio + a relative-path .m3u8,
        // whose path is returned in place of out_path when requested.
        if let Some(dir) = &copy_to {
            let name = out_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "playlist".to_string());
            let report = playlist::export_folder(&entries, dir, &name)?;
            return Ok::<String, SonagramError>(report.playlist_path.to_string_lossy().into_owned());
        }
        Ok::<String, SonagramError>(out_path.to_string_lossy().into_owned())
    })
    .map_err(to_pyerr)
}

/// Profile curation-relevant statistics from a saved `.kgl` graph.
#[pyfunction]
fn profile_library(py: Python<'_>, kgl_path: PathBuf) -> PyResult<Py<PyAny>> {
    let profile = py
        .detach(move || {
            let graph = load_saved_graph(&kgl_path)?;
            core_curation::profile_library(&graph)
        })
        .map_err(to_pyerr)?;
    to_python_json(py, &profile)
}

/// Return the complete versioned policy for a named preset.
#[pyfunction]
fn curation_policy(py: Python<'_>, preset: &str) -> PyResult<Py<PyAny>> {
    let preset = serde_json::from_value::<PlaylistPreset>(serde_json::Value::String(
        preset.to_ascii_lowercase(),
    ))
    .map_err(|error| PyValueError::new_err(format!("invalid preset: {error}")))?;
    to_python_json(py, &PlaylistPolicy::for_preset(preset))
}

/// Curate from a JSON-compatible brief and optional resolved policy.
#[pyfunction]
#[pyo3(signature = (kgl_path, brief, policy=None))]
fn curate_playlist(
    py: Python<'_>,
    kgl_path: PathBuf,
    brief: Py<PyAny>,
    policy: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    let brief: PlaylistBrief = from_python_json(py, brief.bind(py), "brief")?;
    let policy = match policy {
        Some(value) => Some(from_python_json(py, value.bind(py), "policy")?),
        None => None,
    };
    let policy = resolve_curation_policy(&brief, policy).map_err(PyValueError::new_err)?;
    let result = py
        .detach(move || {
            let graph = load_saved_graph(&kgl_path)?;
            core_curation::curate_playlist(&graph, &brief, &policy)
        })
        .map_err(to_pyerr)?;
    to_python_json(py, &result)
}

/// Audit an ordered id list against an optional policy and original brief.
#[pyfunction]
#[pyo3(signature = (kgl_path, track_ids, policy=None, brief=None))]
fn audit_playlist(
    py: Python<'_>,
    kgl_path: PathBuf,
    track_ids: Vec<String>,
    policy: Option<Py<PyAny>>,
    brief: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    validate_ordered_track_ids(&track_ids).map_err(PyValueError::new_err)?;
    let brief = match brief {
        Some(value) => Some(from_python_json(py, value.bind(py), "brief")?),
        None => None,
    };
    let policy = match (policy, brief.as_ref()) {
        (Some(value), Some(brief)) => resolve_curation_policy(
            brief,
            Some(from_python_json(py, value.bind(py), "policy")?),
        )
        .map_err(PyValueError::new_err)?,
        (Some(value), None) => from_python_json(py, value.bind(py), "policy")?,
        (None, Some(brief)) => PlaylistPolicy::for_preset(brief.preset),
        (None, None) => PlaylistPolicy::default(),
    };
    let audit = py
        .detach(move || {
            let graph = load_saved_graph(&kgl_path)?;
            match brief {
                Some(brief) => {
                    core_curation::audit_playlist_for_brief(&graph, &track_ids, &brief, &policy)
                }
                None => core_curation::audit_playlist(&graph, &track_ids, &policy),
            }
        })
        .map_err(to_pyerr)?;
    to_python_json(py, &audit)
}

/// Explain an ordered id list against an optional policy and original brief.
#[pyfunction]
#[pyo3(signature = (kgl_path, track_ids, policy=None, brief=None))]
fn explain_playlist(
    py: Python<'_>,
    kgl_path: PathBuf,
    track_ids: Vec<String>,
    policy: Option<Py<PyAny>>,
    brief: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    validate_ordered_track_ids(&track_ids).map_err(PyValueError::new_err)?;
    let brief = match brief {
        Some(value) => Some(from_python_json(py, value.bind(py), "brief")?),
        None => None,
    };
    let policy = match (policy, brief.as_ref()) {
        (Some(value), Some(brief)) => resolve_curation_policy(
            brief,
            Some(from_python_json(py, value.bind(py), "policy")?),
        )
        .map_err(PyValueError::new_err)?,
        (Some(value), None) => from_python_json(py, value.bind(py), "policy")?,
        (None, Some(brief)) => PlaylistPolicy::for_preset(brief.preset),
        (None, None) => PlaylistPolicy::default(),
    };
    let explanation = py
        .detach(move || {
            let graph = load_saved_graph(&kgl_path)?;
            match brief {
                Some(brief) => core_curation::explain_playlist_for_brief(
                    &graph, &track_ids, &brief, &policy,
                ),
                None => core_curation::explain_playlist(&graph, &track_ids, &policy),
            }
        })
        .map_err(to_pyerr)?;
    to_python_json(py, &explanation)
}

/// Run the shared `sonagram` CLI in-process, returning its process exit code.
///
/// The standalone `sonagram` binary and the `sonagram` console script bundled in
/// this wheel both call the same pure-Rust [`sonagram::cli::run`], so command
/// parsing, output strings, and exit codes cannot drift. `argv` excludes the
/// program name (the `sonagram.cli` shim forwards `sys.argv[1:]`). The GIL is
/// released for the duration — the CLI is blocking Rust IO/CPU that writes its
/// own stdout/stderr and returns only an exit code.
#[pyfunction]
fn _run_cli(py: Python<'_>, argv: Vec<String>) -> i32 {
    py.detach(|| sonagram::cli::run(&argv))
}

/// The compiled `_sonagram` module. Imported by `python/sonagram/__init__.py`.
#[pymodule]
fn _sonagram(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Version — sourced from this crate's Cargo.toml via env! at compile time,
    // kept in lockstep with the core crate and pyproject.toml.
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(scan, m)?)?;
    m.add_function(wrap_pyfunction!(scan_and_enrich, m)?)?;
    m.add_function(wrap_pyfunction!(enrich_, m)?)?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    m.add_function(wrap_pyfunction!(scan_and_build, m)?)?;
    m.add_function(wrap_pyfunction!(export_m3u, m)?)?;
    m.add_function(wrap_pyfunction!(profile_library, m)?)?;
    m.add_function(wrap_pyfunction!(curation_policy, m)?)?;
    m.add_function(wrap_pyfunction!(curate_playlist, m)?)?;
    m.add_function(wrap_pyfunction!(audit_playlist, m)?)?;
    m.add_function(wrap_pyfunction!(explain_playlist, m)?)?;
    m.add_function(wrap_pyfunction!(_run_cli, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_names_are_stable() {
        assert_eq!(stage_name(ScanStage::Walk), "walk");
        assert_eq!(stage_name(ScanStage::Hash), "hash");
        assert_eq!(stage_name(ScanStage::Analyze), "analyze");
        assert_eq!(stage_name(ScanStage::Done), "done");
    }

    #[test]
    fn library_label_is_last_component() {
        assert_eq!(library_label(Path::new("/a/b/Music")), "Music");
        assert_eq!(library_label(Path::new("Music")), "Music");
        // Trailing slash still yields the final component.
        assert_eq!(library_label(Path::new("/a/b/Music/")), "Music");
    }

    #[test]
    fn export_selection_requires_exactly_one() {
        // Both → error.
        let ids = vec!["h1".to_string()];
        assert!(validate_export_selection(Some("MATCH (t) RETURN t"), Some(&ids)).is_err());
        // Neither → error.
        assert!(validate_export_selection(None, None).is_err());
    }

    #[test]
    fn export_selection_cypher_ok_and_trimmed() {
        let sel = validate_export_selection(Some("  MATCH (t) RETURN t  "), None).unwrap();
        assert_eq!(sel, Selection::Cypher("MATCH (t) RETURN t".to_string()));
        // Whitespace-only cypher is rejected.
        assert!(validate_export_selection(Some("   "), None).is_err());
    }

    #[test]
    fn export_selection_ids_ok_and_nonempty() {
        let ids = vec!["h1".to_string(), "h2".to_string()];
        let sel = validate_export_selection(None, Some(&ids)).unwrap();
        assert_eq!(sel, Selection::Ids(ids));
        // Empty id list is rejected.
        assert!(validate_export_selection(None, Some(&[])).is_err());
    }

    #[test]
    fn curation_json_inputs_validate_and_resolve_defaults() {
        let brief: PlaylistBrief = decode_json(
            r#"{"preset":"focus","target_tracks":12,"target_duration_sec":null,"seed_ids":[]}"#,
            "brief",
        )
        .unwrap();
        assert_eq!(brief.target_tracks, 12);
        let policy = resolve_curation_policy(&brief, None).unwrap();
        assert_eq!(policy.preset, brief.preset);
        assert!(resolve_curation_policy(&brief, Some(PlaylistPolicy::default())).is_err());
        assert!(validate_ordered_track_ids(&[]).is_err());
        assert!(validate_ordered_track_ids(&[" ".into()]).is_err());
        assert!(validate_ordered_track_ids(&["hash".into()]).is_ok());
        assert!(decode_json::<PlaylistBrief>(r#"{"preset":"unknown"}"#, "brief").is_err());
        assert!(decode_json::<PlaylistPolicy>(r#"{"preset":"focus"}"#, "policy").is_err());

        let brief_json = serde_json::to_value(&brief).unwrap();
        assert_eq!(brief_json["preset"], "focus");
        assert_eq!(brief_json["target_tracks"], 12);
        let policy_json = serde_json::to_value(&policy).unwrap();
        assert_eq!(policy_json["version"], 1);
        assert!(policy_json.get("eligibility").is_some());
        assert!(policy_json.get("transition").is_some());
    }

    #[test]
    fn curation_python_json_bridge_returns_plain_python_values() {
        Python::initialize();
        Python::attach(|py| {
            let source = py
                .import("json")
                .unwrap()
                .call_method1(
                    "loads",
                    (r#"{"preset":"chill","target_tracks":7,"seed_ids":[]}"#,),
                )
                .unwrap();
            let brief: PlaylistBrief = from_python_json(py, &source, "brief").unwrap();
            assert_eq!(brief.target_tracks, 7);

            let output = to_python_json(py, &brief).unwrap();
            let output = output.bind(py).cast::<PyDict>().unwrap();
            assert_eq!(output.get_item("preset").unwrap().unwrap().extract::<String>().unwrap(), "chill");
            assert_eq!(output.get_item("target_tracks").unwrap().unwrap().extract::<usize>().unwrap(), 7);
            assert!(output.get_item("target_duration_sec").unwrap().unwrap().is_none());
        });
    }
}
