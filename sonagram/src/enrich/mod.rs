//! Last.fm enrichment (P12): fetch popularity, folksonomy tags, MBIDs,
//! original-album back-mapping, and human co-listening similarity for the
//! library's artists/tracks/albums, cache them under `<lib>/.sonagram/lastfm/`,
//! and expose them for the graph builder to ingest.
//!
//! sonagram stays a **mapper**: this module owns the mapping from Last.fm's JSON
//! into our schema and the deterministic cache — nothing more. It is pure Rust
//! (PyO3-free) like the rest of the core crate.
//!
//! ## Shape
//! - [`store`] — the versioned JSON cache (`artists.json` / `tracks.json` /
//!   `albums.json`), each a sorted `BTreeMap` keyed by the same normalized id the
//!   graph uses (artist id / content hash / `"artist|album"`).
//! - [`LastfmApi`] — the HTTP seam (like scan's `Analyzer`): the real
//!   [`UreqClient`] hits `ws.audioscrobbler.com`; offline unit tests inject
//!   canned JSON so the fetch/parse logic is tested without a network.
//! - [`enrich_library`] — the driver: load analysis records, derive the distinct
//!   artist/track/album sets, fetch only what is missing (`fetched` = negative
//!   cache → incremental), and save every [`SAVE_EVERY`] entities (resumable).
//! - [`EnrichmentData`] — the read side the graph builder consumes.
//!
//! ## Soft-fail discipline
//! A per-entity fetch failure never aborts the run: it is recorded as a cache
//! entry with `failed: true` + a `reason` (and `fetched: true`, so it is not
//! re-queried on the next run). Only a missing API key or an unwritable cache
//! surfaces as [`SonagramError::Enrich`].

pub mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::graph::normalize::{album_id, album_name, artist_id};
use crate::progress::{load_progress, unix_now, ProgressWriter};
use crate::record::AnalysisRecord;
use crate::scan::load_records;
use crate::{Result, SonagramError};

pub use store::{
    AlbumEnrich, ArtistEnrich, EnrichStore, SimilarTrack, TrackEnrich, ENRICH_VERSION,
};

/// Last.fm API base. `format=json` + `autocorrect=1` are appended to every call.
pub const API_BASE: &str = "https://ws.audioscrobbler.com/2.0/";
/// Throttle between live API calls — 5 req/s, matching the prototype.
pub const RATE_LIMIT: Duration = Duration::from_millis(200);
/// Per-request timeout for the live client.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Minimum top-tag count kept as folksonomy (drops long-tail noise), matching
/// the prototype's `count >= 10` filter.
pub const MIN_TAG_COUNT: i64 = 10;
/// Max wiki-summary length after HTML stripping.
pub const WIKI_MAX_CHARS: usize = 500;
/// Persist the cache every this many fetched entities, so a long run is
/// resumable after an interruption.
pub const SAVE_EVERY: usize = 25;

// ───────────────────────────── API-key resolution ───────────────────────────

/// Resolve the Last.fm API key, in order: explicit `override_key` → the
/// `LASTFM_API_KEY` env var → a `.env` file in the current dir → a `.env` file in
/// `library_root` → `$SONAGRAM_HOME/.env` (P17: the session- and
/// library-independent home the `sonagram-playlist` skill writes the key to). A
/// missing key is [`SonagramError::Enrich`].
///
/// `.env` parsing is minimal and dependency-free (see [`parse_env`]): `KEY=VALUE`
/// lines, `#` comments and blanks ignored, surrounding quotes stripped.
pub fn api_key(library_root: &Path, override_key: Option<&str>) -> Result<String> {
    if let Some(k) = override_key.map(str::trim).filter(|k| !k.is_empty()) {
        return Ok(k.to_string());
    }
    if let Ok(k) = std::env::var("LASTFM_API_KEY") {
        let k = k.trim();
        if !k.is_empty() {
            return Ok(k.to_string());
        }
    }
    let mut env_paths = vec![
        std::path::PathBuf::from(".env"),
        library_root.join(".env"),
    ];
    // The home `.env` is the last, session-independent fallback (where the skill
    // stores a user-provided key).
    if let Ok(home) = crate::config::sonagram_home() {
        env_paths.push(home.join(".env"));
    }
    for env_path in env_paths {
        if let Some(k) = key_in_env_file(&env_path) {
            return Ok(k);
        }
    }
    Err(SonagramError::Enrich(
        "no LASTFM_API_KEY — set the env var, or add `LASTFM_API_KEY=...` to a \
         .env file in the current dir, the library root, or ~/.sonagram/.env"
            .to_string(),
    ))
}

/// The non-empty `LASTFM_API_KEY` value from a `.env` file, or `None` when the
/// file is missing or carries no usable key.
fn key_in_env_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let k = parse_env(&text).get("LASTFM_API_KEY")?.trim().to_string();
    if k.is_empty() {
        None
    } else {
        Some(k)
    }
}

/// Report **where** a Last.fm key is configured (for `sonagram config`), as a
/// short source label — **never the key itself**. Mirrors [`api_key`]'s order,
/// minus the library-root tier (`config` has no library context): the
/// `LASTFM_API_KEY` env var → a cwd `.env` → `$SONAGRAM_HOME/.env`. `None` when no
/// key is configured anywhere those tiers look.
pub fn api_key_source() -> Option<&'static str> {
    if std::env::var("LASTFM_API_KEY")
        .ok()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
    {
        return Some("env: LASTFM_API_KEY");
    }
    if key_in_env_file(Path::new(".env")).is_some() {
        return Some(".env (current dir)");
    }
    if let Ok(home) = crate::config::sonagram_home() {
        if key_in_env_file(&home.join(".env")).is_some() {
            return Some("~/.sonagram/.env");
        }
    }
    None
}

/// Minimal `.env` parser: `KEY=VALUE` per line. Blank lines and `#` comments are
/// ignored, a leading `export ` is dropped, and one layer of surrounding matched
/// single/double quotes is stripped from the value. No escapes, no interpolation
/// — just enough to read an API key without a dotenv dependency.
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let val = strip_quotes(val.trim());
        out.insert(key.to_string(), val.to_string());
    }
    out
}

/// Strip one layer of matched surrounding quotes.
fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ─────────────────────────────── HTTP seam ──────────────────────────────────

/// The Last.fm HTTP surface, factored out so tests can inject canned JSON
/// (mirrors scan's `Analyzer` seam). `call` returns `None` on any transport /
/// decode failure **and** on a Last.fm `{"error": ...}` payload — i.e. "no usable
/// data", exactly like the prototype's `api_call`.
pub trait LastfmApi {
    /// Call `method` with the extra `params` (`method`, `api_key`, `format`,
    /// `autocorrect` are supplied by the implementation). `None` = no data.
    fn call(&self, method: &str, params: &[(&str, &str)]) -> Option<Json>;
}

/// The production client: one blocking `ureq` agent, throttled to
/// [`RATE_LIMIT`], hitting [`API_BASE`].
pub struct UreqClient {
    agent: ureq::Agent,
    api_key: String,
    base: String,
}

impl UreqClient {
    /// Build a client for `api_key` against [`API_BASE`].
    pub fn new(api_key: String) -> Self {
        Self::with_base(api_key, API_BASE.to_string())
    }

    /// Build a client against a custom base URL (tests / mirrors).
    pub fn with_base(api_key: String, base: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(HTTP_TIMEOUT)
            .build();
        UreqClient {
            agent,
            api_key,
            base,
        }
    }
}

impl LastfmApi for UreqClient {
    fn call(&self, method: &str, params: &[(&str, &str)]) -> Option<Json> {
        let mut req = self
            .agent
            .get(&self.base)
            .query("method", method)
            .query("api_key", &self.api_key)
            .query("format", "json")
            .query("autocorrect", "1");
        for (k, v) in params {
            req = req.query(k, v);
        }
        let result = req.call();
        // Throttle after every live call (whether or not it succeeded).
        std::thread::sleep(RATE_LIMIT);
        let body = result.ok()?.into_string().ok()?;
        let json: Json = serde_json::from_str(&body).ok()?;
        if json.get("error").is_some() {
            return None;
        }
        Some(json)
    }
}

// ──────────────────────────── JSON parse helpers ────────────────────────────

/// A non-empty trimmed string from a JSON value, else `None`.
fn opt_str(v: Option<&Json>) -> Option<String> {
    v.and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// An i64 from a JSON value that may be a number or a numeric string (Last.fm
/// returns counts as strings). `None` for missing / non-numeric / empty.
fn opt_int(v: Option<&Json>) -> Option<i64> {
    match v {
        Some(Json::Number(n)) => n.as_i64(),
        Some(Json::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                s.parse::<i64>().ok()
            }
        }
        _ => None,
    }
}

/// Normalize a Last.fm `{"tag": ...}` container into `(lowercased_name, count)`
/// pairs. The `tag` node is an array of objects, a single object, or absent.
fn tag_pairs(container: Option<&Json>) -> Vec<(String, Option<i64>)> {
    let Some(tag) = container.and_then(|c| c.get("tag")) else {
        return Vec::new();
    };
    let items: Vec<&Json> = match tag {
        Json::Array(a) => a.iter().collect(),
        obj @ Json::Object(_) => vec![obj],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|t| {
            let name = opt_str(t.get("name"))?.to_lowercase();
            Some((name, opt_int(t.get("count"))))
        })
        .collect()
}

/// The names from a tag container with **no** count filter, deduped in first-seen
/// order (used for track/album `toptags`, which carry no meaningful count).
fn tag_names_unfiltered(container: Option<&Json>) -> Vec<String> {
    dedup_keep_order(tag_pairs(container).into_iter().map(|(n, _)| n))
}

/// Dedup an iterator of strings, keeping first-seen order.
fn dedup_keep_order(it: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for s in it {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

/// Strip everything from the first `<` (Last.fm wiki summaries end in an
/// `<a href...>` read-more link) and truncate to `max` chars. `None` when the
/// result is empty.
fn strip_html_truncate(s: &str, max: usize) -> Option<String> {
    let head = match s.find('<') {
        Some(i) => &s[..i],
        None => s,
    };
    let head = head.trim();
    if head.is_empty() {
        return None;
    }
    Some(head.chars().take(max).collect())
}

/// The join key for resolving a `(artist, title)` against owned tracks:
/// `"<artist-lower>::<title-lower>"`. Trimmed + lowercased on both sides so
/// Last.fm's returned names match our tag strings case-insensitively.
pub fn similar_key(artist: &str, title: &str) -> String {
    format!("{}::{}", artist.trim().to_lowercase(), title.trim().to_lowercase())
}

// ────────────────────────────── fetch surface ───────────────────────────────

/// Fetch one artist: `artist.getInfo` (+ `artist.getTopTags`). Mirrors the
/// prototype (`fetch_lastfm.py:59`). Always returns a record with `fetched:
/// true`; `failed: true` when `getInfo` yielded nothing.
pub fn fetch_artist(client: &dyn LastfmApi, name: &str) -> ArtistEnrich {
    let mut rec = ArtistEnrich {
        queried_name: name.to_string(),
        fetched: true,
        ..Default::default()
    };

    match client.call("artist.getInfo", &[("artist", name)]) {
        Some(data) => {
            if let Some(info) = data.get("artist") {
                rec.url = opt_str(info.get("url"));
                rec.mbid = opt_str(info.get("mbid"));
                let stats = info.get("stats");
                rec.listeners = opt_int(stats.and_then(|s| s.get("listeners")));
                rec.playcount = opt_int(stats.and_then(|s| s.get("playcount")));

                if let Some(returned) = opt_str(info.get("name")) {
                    if returned.to_lowercase() != name.trim().to_lowercase() {
                        rec.correction = Some(returned);
                    }
                }
                rec.tags = tag_names_unfiltered(info.get("tags"));
                rec.similar = info
                    .get("similar")
                    .and_then(|s| s.get("artist"))
                    .map(collect_names)
                    .unwrap_or_default();
            } else {
                rec.failed = true;
                rec.reason = Some("artist.getInfo returned no `artist`".to_string());
            }
        }
        None => {
            rec.failed = true;
            rec.reason = Some("artist.getInfo returned no data".to_string());
        }
    }

    // Top tags: count-filtered, appended to any getInfo tags (deduped).
    if let Some(data) = client.call("artist.getTopTags", &[("artist", name)]) {
        for (tname, count) in tag_pairs(data.get("toptags")) {
            if count.unwrap_or(0) >= MIN_TAG_COUNT && !rec.tags.contains(&tname) {
                rec.tags.push(tname);
            }
        }
    }

    rec
}

/// Collect `name` fields from a Last.fm array-or-single-object of entities.
fn collect_names(node: &Json) -> Vec<String> {
    let items: Vec<&Json> = match node {
        Json::Array(a) => a.iter().collect(),
        obj @ Json::Object(_) => vec![obj],
        _ => Vec::new(),
    };
    items.iter().filter_map(|e| opt_str(e.get("name"))).collect()
}

/// Fetch one track: `track.getInfo` (+ `track.getSimilar`, limit 10). Mirrors
/// `fetch_lastfm.py:177`. **Keeps the similar `match` weight** (the bug the
/// prototype had — see [`SimilarTrack`]).
pub fn fetch_track(client: &dyn LastfmApi, artist: &str, title: &str) -> TrackEnrich {
    let mut rec = TrackEnrich {
        fetched: true,
        ..Default::default()
    };

    match client.call("track.getInfo", &[("artist", artist), ("track", title)]) {
        Some(data) => {
            if let Some(track) = data.get("track") {
                rec.mbid = opt_str(track.get("mbid"));
                rec.url = opt_str(track.get("url"));
                rec.lastfm_title = opt_str(track.get("name"));
                rec.duration_ms = opt_int(track.get("duration")).filter(|&d| d > 0);
                rec.listeners = opt_int(track.get("listeners"));
                rec.playcount = opt_int(track.get("playcount"));
                rec.lastfm_artist = opt_str(track.get("artist").and_then(|a| a.get("name")));

                if let Some(album) = track.get("album") {
                    if let Some(atitle) = opt_str(album.get("title")) {
                        rec.album_title = Some(atitle);
                        rec.album_mbid = opt_str(album.get("mbid"));
                        rec.album_url = opt_str(album.get("url"));
                        rec.album_position =
                            opt_int(album.get("@attr").and_then(|a| a.get("position")));
                    }
                }
                rec.tags = tag_names_unfiltered(track.get("toptags"));
            } else {
                rec.failed = true;
                rec.reason = Some("track.getInfo returned no `track`".to_string());
            }
        }
        None => {
            rec.failed = true;
            rec.reason = Some("track.getInfo returned no data".to_string());
        }
    }

    if let Some(data) = client.call(
        "track.getSimilar",
        &[("artist", artist), ("track", title), ("limit", "10")],
    ) {
        if let Some(node) = data.get("similartracks").and_then(|s| s.get("track")) {
            let items: Vec<&Json> = match node {
                Json::Array(a) => a.iter().collect(),
                obj @ Json::Object(_) => vec![obj],
                _ => Vec::new(),
            };
            for sim in items {
                let s_artist = opt_str(sim.get("artist").and_then(|a| a.get("name")));
                let s_title = opt_str(sim.get("name"));
                if let (Some(a), Some(t)) = (s_artist, s_title) {
                    let match_weight = sim
                        .get("match")
                        .and_then(|m| match m {
                            Json::Number(n) => n.as_f64(),
                            Json::String(s) => s.trim().parse::<f64>().ok(),
                            _ => None,
                        })
                        .unwrap_or(0.0) as f32;
                    rec.similar.push(SimilarTrack {
                        artist: a,
                        title: t,
                        match_weight,
                    });
                }
            }
        }
    }

    rec
}

/// Fetch one album: `album.getInfo`. Mirrors `fetch_lastfm.py:303`. No art
/// fetching in P12 (a separate backlog item).
pub fn fetch_album(client: &dyn LastfmApi, artist: &str, album: &str) -> AlbumEnrich {
    let mut rec = AlbumEnrich {
        fetched: true,
        ..Default::default()
    };

    match client.call("album.getInfo", &[("artist", artist), ("album", album)]) {
        Some(data) => {
            if let Some(al) = data.get("album") {
                rec.mbid = opt_str(al.get("mbid"));
                rec.url = opt_str(al.get("url"));
                rec.listeners = opt_int(al.get("listeners"));
                rec.playcount = opt_int(al.get("playcount"));
                rec.tags = tag_names_unfiltered(al.get("tags"));
                rec.wiki_summary = al
                    .get("wiki")
                    .and_then(|w| w.get("summary"))
                    .and_then(Json::as_str)
                    .and_then(|s| strip_html_truncate(s, WIKI_MAX_CHARS));
            } else {
                rec.failed = true;
                rec.reason = Some("album.getInfo returned no `album`".to_string());
            }
        }
        None => {
            rec.failed = true;
            rec.reason = Some("album.getInfo returned no data".to_string());
        }
    }

    rec
}

// ─────────────────────────── distinct entity sets ───────────────────────────

/// The distinct artist ids, `(content_hash, artist, title)` track tuples, and
/// `(album_id, artist, album)` album tuples the library's records imply — the
/// work list the driver fetches. All sorted / `BTreeMap`-derived for
/// determinism.
struct EntitySets {
    /// artist id → the (raw) artist name to query.
    artists: BTreeMap<String, String>,
    /// content_hash → (artist name, track title) to query.
    tracks: BTreeMap<String, (String, String)>,
    /// album id → (artist name, album title) to query.
    albums: BTreeMap<String, (String, String)>,
}

/// Derive the distinct artist / track / album work sets from analysis records.
fn distinct_entities(records: &[AnalysisRecord]) -> EntitySets {
    let mut artists = BTreeMap::new();
    let mut tracks = BTreeMap::new();
    let mut albums = BTreeMap::new();

    for r in records {
        let t = r.tags.as_ref();
        let art = artist_id(t.and_then(|t| t.artist.as_deref()));
        artists.entry(art.clone()).or_insert_with(|| art.clone());

        // A track is only fetchable with a title (else Last.fm can't resolve it).
        if let Some(title) = t
            .and_then(|t| t.title.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            tracks
                .entry(r.source.content_hash.clone())
                .or_insert_with(|| (art.clone(), title.to_string()));
        }

        if let Some(aid) = album_id(&art, t.and_then(|t| t.album.as_deref())) {
            if let Some(name) = album_name(t.and_then(|t| t.album.as_deref())) {
                albums.entry(aid).or_insert_with(|| (art.clone(), name));
            }
        }
    }

    EntitySets {
        artists,
        tracks,
        albums,
    }
}

// ───────────────────────────────── driver ───────────────────────────────────

/// Coarse enrichment progress, reported through [`EnrichOptions::progress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichProgress {
    /// Which entity kind is being fetched.
    pub kind: EnrichKind,
    /// Entities fetched in this kind so far (this run; excludes cache skips).
    pub done: usize,
    /// Total to fetch in this kind (this run).
    pub total: usize,
}

/// The entity kind an [`EnrichProgress`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichKind {
    /// Fetching artists.
    Artist,
    /// Fetching tracks.
    Track,
    /// Fetching albums.
    Album,
}

/// The stable string form of an [`EnrichKind`] used in the progress file.
fn kind_name(kind: EnrichKind) -> &'static str {
    match kind {
        EnrichKind::Artist => "artist",
        EnrichKind::Track => "track",
        EnrichKind::Album => "album",
    }
}

/// The on-disk enrich progress snapshot (P20):
/// `<lib>/.sonagram/enrich_progress.json`. Written atomically and throttled by
/// [`enrich_library_with`] itself — every entry point (CLI, Python, a
/// concurrent scan-and-enrich pipeline) produces the same observable progress.
/// `kind = "done"` marks a completed run; a stale `updated_unix` with any other
/// kind means the enriching process died.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnrichProgressSnapshot {
    /// PID of the enriching process.
    pub pid: u32,
    /// What is being fetched: `"artist"`, `"track"`, `"album"`, `"done"`.
    pub kind: String,
    /// Entities fetched in this kind so far (this run).
    pub done: usize,
    /// Total to fetch in this kind (this run).
    pub total: usize,
    /// Artists fetched / failed / skipped-as-cached this run.
    pub artists_fetched: usize,
    /// See `artists_fetched`.
    pub artists_failed: usize,
    /// See `artists_fetched`.
    pub artists_skipped: usize,
    /// Tracks fetched / failed / skipped-as-cached this run.
    pub tracks_fetched: usize,
    /// See `tracks_fetched`.
    pub tracks_failed: usize,
    /// See `tracks_fetched`.
    pub tracks_skipped: usize,
    /// Albums fetched / failed / skipped-as-cached this run.
    pub albums_fetched: usize,
    /// See `albums_fetched`.
    pub albums_failed: usize,
    /// See `albums_fetched`.
    pub albums_skipped: usize,
    /// When this run started (unix seconds).
    pub started_unix: i64,
    /// When this snapshot was written (unix seconds).
    pub updated_unix: i64,
}

/// Path of a library's enrich progress file.
pub fn enrich_progress_path(library_root: &Path) -> PathBuf {
    library_root.join(".sonagram").join("enrich_progress.json")
}

/// Load a library's enrich progress snapshot, `None` when absent or unreadable.
pub fn load_enrich_progress(library_root: &Path) -> Option<EnrichProgressSnapshot> {
    load_progress(&enrich_progress_path(library_root))
}

/// How often the enrich progress snapshot is refreshed (unforced writes).
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// Options controlling an enrichment run.
#[derive(Default)]
pub struct EnrichOptions {
    /// Explicit API key, overriding env / `.env` resolution. Usually `None`.
    pub api_key: Option<String>,
    /// Optional progress sink.
    pub progress: Option<Box<dyn Fn(EnrichProgress) + Send + Sync>>,
}

impl EnrichOptions {
    fn report(&self, kind: EnrichKind, done: usize, total: usize) {
        if let Some(p) = &self.progress {
            p(EnrichProgress { kind, done, total });
        }
    }
}

/// The outcome of an enrichment run — counts per entity kind + wall time.
#[derive(Debug, Clone, Default)]
pub struct EnrichReport {
    /// Artists newly fetched this run.
    pub artists_fetched: usize,
    /// Artists already in cache (skipped).
    pub artists_skipped: usize,
    /// Artists whose fetch soft-failed this run.
    pub artists_failed: usize,
    /// Tracks newly fetched this run.
    pub tracks_fetched: usize,
    /// Tracks already in cache (skipped).
    pub tracks_skipped: usize,
    /// Tracks whose fetch soft-failed this run.
    pub tracks_failed: usize,
    /// Albums newly fetched this run.
    pub albums_fetched: usize,
    /// Albums already in cache (skipped).
    pub albums_skipped: usize,
    /// Albums whose fetch soft-failed this run.
    pub albums_failed: usize,
    /// Wall-clock time for the run.
    pub elapsed: Duration,
}

/// Enrich a library: load its analysis records, derive the distinct
/// artist/track/album sets, fetch every entity not already cached, and save the
/// `.sonagram/lastfm/*.json` cache (incrementally, every [`SAVE_EVERY`]).
///
/// Resolves the API key via [`api_key`]; a missing key is the only hard error.
/// Per-entity failures are soft (recorded in the cache with `failed: true`).
pub fn enrich_library(library_root: &Path, opts: &EnrichOptions) -> Result<EnrichReport> {
    let key = api_key(library_root, opts.api_key.as_deref())?;
    let client = UreqClient::new(key);
    enrich_library_with(library_root, opts, &client)
}

/// [`enrich_library`] with an injected [`LastfmApi`] (the seam tests drive).
/// Does **not** resolve the API key (the client already carries it).
pub fn enrich_library_with(
    library_root: &Path,
    opts: &EnrichOptions,
    client: &dyn LastfmApi,
) -> Result<EnrichReport> {
    let start = Instant::now();
    let started_unix = unix_now();
    let records = load_records(library_root)?;
    let sets = distinct_entities(&records);
    let store = EnrichStore::new(library_root);

    let mut report = EnrichReport::default();
    let progress_file =
        ProgressWriter::new(enrich_progress_path(library_root), PROGRESS_INTERVAL);
    let write_progress =
        |report: &EnrichReport, kind: &str, done: usize, total: usize, force: bool| {
            progress_file.write(
                &EnrichProgressSnapshot {
                    pid: std::process::id(),
                    kind: kind.to_string(),
                    done,
                    total,
                    artists_fetched: report.artists_fetched,
                    artists_failed: report.artists_failed,
                    artists_skipped: report.artists_skipped,
                    tracks_fetched: report.tracks_fetched,
                    tracks_failed: report.tracks_failed,
                    tracks_skipped: report.tracks_skipped,
                    albums_fetched: report.albums_fetched,
                    albums_failed: report.albums_failed,
                    albums_skipped: report.albums_skipped,
                    started_unix,
                    updated_unix: unix_now(),
                },
                force,
            );
        };

    // ── Artists ──
    let mut artists = store.load_artists()?;
    let todo: Vec<(&String, &String)> = sets
        .artists
        .iter()
        .filter(|(id, _)| !artists.get(*id).map(|r| r.fetched).unwrap_or(false))
        .collect();
    report.artists_skipped = sets.artists.len() - todo.len();
    let total = todo.len();
    write_progress(&report, kind_name(EnrichKind::Artist), 0, total, true);
    let mut since_save = 0usize;
    for (i, (id, name)) in todo.iter().enumerate() {
        let rec = fetch_artist(client, name);
        if rec.failed {
            report.artists_failed += 1;
        }
        artists.insert((*id).clone(), rec);
        report.artists_fetched += 1;
        since_save += 1;
        opts.report(EnrichKind::Artist, i + 1, total);
        write_progress(&report, kind_name(EnrichKind::Artist), i + 1, total, false);
        if since_save >= SAVE_EVERY {
            store.save_artists(&artists)?;
            since_save = 0;
        }
    }
    store.save_artists(&artists)?;

    // ── Tracks ──
    let mut tracks = store.load_tracks()?;
    let todo: Vec<(&String, &(String, String))> = sets
        .tracks
        .iter()
        .filter(|(id, _)| !tracks.get(*id).map(|r| r.fetched).unwrap_or(false))
        .collect();
    report.tracks_skipped = sets.tracks.len() - todo.len();
    let total = todo.len();
    write_progress(&report, kind_name(EnrichKind::Track), 0, total, true);
    since_save = 0;
    for (i, (hash, (artist, title))) in todo.iter().enumerate() {
        let rec = fetch_track(client, artist, title);
        if rec.failed {
            report.tracks_failed += 1;
        }
        tracks.insert((*hash).clone(), rec);
        report.tracks_fetched += 1;
        since_save += 1;
        opts.report(EnrichKind::Track, i + 1, total);
        write_progress(&report, kind_name(EnrichKind::Track), i + 1, total, false);
        if since_save >= SAVE_EVERY {
            store.save_tracks(&tracks)?;
            since_save = 0;
        }
    }
    store.save_tracks(&tracks)?;

    // ── Albums ──
    let mut albums = store.load_albums()?;
    let todo: Vec<(&String, &(String, String))> = sets
        .albums
        .iter()
        .filter(|(id, _)| !albums.get(*id).map(|r| r.fetched).unwrap_or(false))
        .collect();
    report.albums_skipped = sets.albums.len() - todo.len();
    let total = todo.len();
    write_progress(&report, kind_name(EnrichKind::Album), 0, total, true);
    since_save = 0;
    for (i, (id, (artist, album))) in todo.iter().enumerate() {
        let rec = fetch_album(client, artist, album);
        if rec.failed {
            report.albums_failed += 1;
        }
        albums.insert((*id).clone(), rec);
        report.albums_fetched += 1;
        since_save += 1;
        opts.report(EnrichKind::Album, i + 1, total);
        write_progress(&report, kind_name(EnrichKind::Album), i + 1, total, false);
        if since_save >= SAVE_EVERY {
            store.save_albums(&albums)?;
            since_save = 0;
        }
    }
    store.save_albums(&albums)?;

    write_progress(&report, "done", 0, 0, true);
    report.elapsed = start.elapsed();
    Ok(report)
}

// ─────────────────────────── read side (for graph) ──────────────────────────

/// The enrichment the graph builder ingests: the three cached maps, keyed
/// exactly as the graph keys its nodes (artist id / content hash /
/// `"artist|album"`). Loaded from `<lib>/.sonagram/lastfm/`.
#[derive(Debug, Clone, Default)]
pub struct EnrichmentData {
    /// artist id → enrichment.
    pub artists: BTreeMap<String, ArtistEnrich>,
    /// content_hash → enrichment.
    pub tracks: BTreeMap<String, TrackEnrich>,
    /// album id (`"artist|album"`) → enrichment.
    pub albums: BTreeMap<String, AlbumEnrich>,
}

impl EnrichmentData {
    /// Load the enrichment cache for `library_root`, or `None` when no
    /// `.sonagram/lastfm/` cache exists (a plain, un-enriched build). A present
    /// cache with any of the three files loads what it has (missing files → empty
    /// maps).
    pub fn load(library_root: &Path) -> Result<Option<Self>> {
        let store = EnrichStore::new(library_root);
        if !store.dir().exists() {
            return Ok(None);
        }
        Ok(Some(EnrichmentData {
            artists: store.load_artists()?,
            tracks: store.load_tracks()?,
            albums: store.load_albums()?,
        }))
    }

    /// Load directly from a `lastfm/` directory (the three JSON files live
    /// there). Used by the golden-gate fixtures, which store them outside a
    /// library layout.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        // Reuse EnrichStore by pointing its `.sonagram/lastfm/` at `dir`'s
        // parent chain is awkward; read the three files directly instead.
        let read = |name: &str| -> Result<String> {
            let p = dir.join(name);
            if p.exists() {
                Ok(std::fs::read_to_string(&p)?)
            } else {
                Ok(String::new())
            }
        };
        fn parse<T: for<'de> serde::Deserialize<'de>>(
            text: &str,
        ) -> Result<BTreeMap<String, T>> {
            if text.trim().is_empty() {
                return Ok(BTreeMap::new());
            }
            let file: store::EnrichFile<T> = serde_json::from_str(text)
                .map_err(|e| SonagramError::Enrich(format!("parse enrichment fixture: {e}")))?;
            Ok(file.entries)
        }
        Ok(EnrichmentData {
            artists: parse(&read("artists.json")?)?,
            tracks: parse(&read("tracks.json")?)?,
            albums: parse(&read("albums.json")?)?,
        })
    }

    /// True when there is nothing to ingest.
    pub fn is_empty(&self) -> bool {
        self.artists.is_empty() && self.tracks.is_empty() && self.albums.is_empty()
    }

    /// Count of tracks that carry a non-failed record (for the build report).
    pub fn tracks_present(&self) -> usize {
        self.tracks.values().filter(|r| r.fetched && !r.failed).count()
    }

    /// Count of artists that carry a non-failed record (for the build report).
    pub fn artists_present(&self) -> usize {
        self.artists.values().filter(|r| r.fetched && !r.failed).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── .env parsing ──

    #[test]
    fn parse_env_handles_comments_blanks_quotes_export() {
        let text = "\
            # a comment\n\
            \n\
            LASTFM_API_KEY=abc123\n\
            QUOTED=\"hello world\"\n\
            SINGLE='sq'\n\
            export EXPORTED=xyz\n\
            SPACED =  spaced_val  \n\
            # trailing comment\n";
        let m = parse_env(text);
        assert_eq!(m.get("LASTFM_API_KEY").unwrap(), "abc123");
        assert_eq!(m.get("QUOTED").unwrap(), "hello world");
        assert_eq!(m.get("SINGLE").unwrap(), "sq");
        assert_eq!(m.get("EXPORTED").unwrap(), "xyz");
        assert_eq!(m.get("SPACED").unwrap(), "spaced_val");
        assert!(!m.contains_key("# a comment"));
    }

    #[test]
    fn strip_quotes_only_matched_pairs() {
        assert_eq!(strip_quotes("\"x\""), "x");
        assert_eq!(strip_quotes("'x'"), "x");
        assert_eq!(strip_quotes("\"mismatch'"), "\"mismatch'");
        assert_eq!(strip_quotes("plain"), "plain");
    }

    #[test]
    fn api_key_missing_is_clear_error() {
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A dir with no .env, env var cleared for this process is not safe to
        // assume; instead point at a temp dir and pass an explicit empty override
        // + rely on env being unset in CI. We only assert the error *shape* when
        // the var is absent.
        if std::env::var("LASTFM_API_KEY").is_ok() {
            return; // a real key is present in this environment; skip
        }
        let dir = std::env::temp_dir().join(format!("sonagram-nokey-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Pin SONAGRAM_HOME at this empty temp dir so the home `.env` fallback in
        // `api_key` can't reach a real `~/.sonagram/.env` (a sibling test may have
        // left SONAGRAM_HOME unset). Without this, a maintainer machine with a
        // configured key fails a test that only guards the env var.
        std::env::set_var("SONAGRAM_HOME", &dir);
        let err = api_key(&dir, None).unwrap_err();
        std::env::remove_var("SONAGRAM_HOME");
        assert!(matches!(err, SonagramError::Enrich(_)));
        assert!(err.to_string().contains("no LASTFM_API_KEY"));
    }

    #[test]
    fn api_key_override_wins() {
        let dir = std::env::temp_dir();
        assert_eq!(api_key(&dir, Some("  mykey  ")).unwrap(), "mykey");
    }

    #[test]
    fn api_key_falls_back_to_sonagram_home_env() {
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Only meaningful when no real key is present in this environment.
        if std::env::var("LASTFM_API_KEY").is_ok() {
            return;
        }
        let home = std::env::temp_dir().join(format!("sonagram-homeenv-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".env"), "LASTFM_API_KEY=homekey123\n").unwrap();
        std::env::set_var("SONAGRAM_HOME", &home);

        // A library root with no .env of its own falls through to the home .env.
        let lib = std::env::temp_dir().join(format!("sonagram-homeenv-lib-{}", std::process::id()));
        std::fs::create_dir_all(&lib).unwrap();
        // Run from a dir with no cwd .env by pointing library_root at `lib`.
        let got = api_key(&lib, None);

        std::env::remove_var("SONAGRAM_HOME");
        let _ = std::fs::remove_dir_all(&home);
        // The cwd may or may not carry a `.env`; accept either the home key or a
        // cwd-provided one, but the home tier must be reachable when cwd has none.
        if !Path::new(".env").exists() {
            assert_eq!(got.unwrap(), "homekey123");
        }
    }

    // ── html strip / truncate ──

    #[test]
    fn strip_html_truncate_cuts_at_first_tag_and_length() {
        let s = "A great album. <a href=\"http://last.fm\">Read more</a>";
        assert_eq!(strip_html_truncate(s, 500).unwrap(), "A great album.");
        // Truncation at max chars.
        let long = "x".repeat(600);
        assert_eq!(strip_html_truncate(&long, 500).unwrap().chars().count(), 500);
        // Empty after strip → None.
        assert!(strip_html_truncate("   <a>", 500).is_none());
    }

    // ── similar key ──

    #[test]
    fn similar_key_is_lowercased_and_trimmed() {
        assert_eq!(similar_key("  ABBA ", "On And On"), "abba::on and on");
    }

    // ── tag count filter (artist top tags) ──

    #[test]
    fn artist_top_tags_apply_count_filter() {
        struct Canned;
        impl LastfmApi for Canned {
            fn call(&self, method: &str, _p: &[(&str, &str)]) -> Option<Json> {
                match method {
                    "artist.getInfo" => Some(json!({
                        "artist": {
                            "name": "ABBA",
                            "url": "https://last.fm/abba",
                            "mbid": "mbid-abba",
                            "stats": {"listeners": "1000", "playcount": "5000"},
                            "tags": {"tag": [{"name": "Pop"}]},
                            "similar": {"artist": [{"name": "Bee Gees"}, {"name": "A-ha"}]}
                        }
                    })),
                    "artist.getTopTags" => Some(json!({
                        "toptags": {"tag": [
                            {"name": "swedish", "count": 100},
                            {"name": "disco", "count": 40},
                            {"name": "noise", "count": 3}
                        ]}
                    })),
                    _ => None,
                }
            }
        }
        let rec = fetch_artist(&Canned, "ABBA");
        assert!(rec.fetched && !rec.failed);
        assert_eq!(rec.listeners, Some(1000));
        assert_eq!(rec.playcount, Some(5000));
        assert_eq!(rec.mbid.as_deref(), Some("mbid-abba"));
        // "pop" from getInfo (lowercased), then swedish + disco (count >= 10);
        // "noise" (count 3) is filtered out.
        assert_eq!(rec.tags, vec!["pop", "swedish", "disco"]);
        assert_eq!(rec.similar, vec!["Bee Gees", "A-ha"]);
    }

    #[test]
    fn track_similar_keeps_match_weight() {
        struct Canned;
        impl LastfmApi for Canned {
            fn call(&self, method: &str, _p: &[(&str, &str)]) -> Option<Json> {
                match method {
                    "track.getInfo" => Some(json!({
                        "track": {
                            "name": "On and on and on",
                            "mbid": "t-mbid",
                            "url": "https://last.fm/t",
                            "duration": "270000",
                            "listeners": "800",
                            "playcount": "9000",
                            "artist": {"name": "ABBA"},
                            "album": {
                                "title": "Super Trouper",
                                "mbid": "al-mbid",
                                "url": "https://last.fm/al",
                                "@attr": {"position": "7"}
                            },
                            "toptags": {"tag": [{"name": "Pop"}, {"name": "Classic"}]}
                        }
                    })),
                    "track.getSimilar" => Some(json!({
                        "similartracks": {"track": [
                            {"name": "Jive Talkin'", "artist": {"name": "Bee Gees"}, "match": 0.87},
                            {"name": "Marry You", "artist": {"name": "Bruno Mars"}, "match": "0.42"}
                        ]}
                    })),
                    _ => None,
                }
            }
        }
        let rec = fetch_track(&Canned, "ABBA", "On and on and on");
        assert!(rec.fetched && !rec.failed);
        assert_eq!(rec.duration_ms, Some(270000));
        assert_eq!(rec.album_title.as_deref(), Some("Super Trouper"));
        assert_eq!(rec.album_position, Some(7));
        assert_eq!(rec.tags, vec!["pop", "classic"]);
        assert_eq!(rec.similar.len(), 2);
        assert_eq!(rec.similar[0].match_weight, 0.87);
        assert_eq!(rec.similar[1].match_weight, 0.42);
    }

    #[test]
    fn fetch_soft_fails_on_no_data() {
        struct Empty;
        impl LastfmApi for Empty {
            fn call(&self, _m: &str, _p: &[(&str, &str)]) -> Option<Json> {
                None
            }
        }
        let a = fetch_artist(&Empty, "Nobody");
        assert!(a.fetched && a.failed);
        assert!(a.reason.is_some());
        let t = fetch_track(&Empty, "Nobody", "Nothing");
        assert!(t.fetched && t.failed);
        let al = fetch_album(&Empty, "Nobody", "Nowhere");
        assert!(al.fetched && al.failed);
    }

    /// Live-API smoke test — hits the real Last.fm endpoint for a well-known
    /// artist. `#[ignore]` so it never runs in the normal offline suite; also
    /// env-gated on `LASTFM_API_KEY` (skips cleanly if run without a key). Run
    /// explicitly:
    ///
    ///   LASTFM_API_KEY=... cargo test -p sonagram --lib \
    ///       enrich::tests::live_artist_fetch_smoke -- --ignored
    #[test]
    #[ignore = "live network; run with --ignored and LASTFM_API_KEY set"]
    fn live_artist_fetch_smoke() {
        let Ok(key) = std::env::var("LASTFM_API_KEY") else {
            eprintln!("SKIP: LASTFM_API_KEY unset");
            return;
        };
        let client = UreqClient::new(key);
        let rec = fetch_artist(&client, "The Beatles");
        assert!(rec.fetched, "queried");
        assert!(!rec.failed, "The Beatles should resolve: {:?}", rec.reason);
        assert!(rec.listeners.unwrap_or(0) > 0, "listeners populated");
        assert!(!rec.tags.is_empty(), "folksonomy tags populated");
    }

    #[test]
    fn album_wiki_is_stripped_and_truncated() {
        struct Canned;
        impl LastfmApi for Canned {
            fn call(&self, method: &str, _p: &[(&str, &str)]) -> Option<Json> {
                match method {
                    "album.getInfo" => Some(json!({
                        "album": {
                            "mbid": "al",
                            "url": "u",
                            "listeners": "10",
                            "playcount": "20",
                            "tags": {"tag": [{"name": "Disco"}]},
                            "wiki": {"summary": "A classic. <a href=\"x\">more</a>"}
                        }
                    })),
                    _ => None,
                }
            }
        }
        let rec = fetch_album(&Canned, "ABBA", "Super Trouper");
        assert_eq!(rec.wiki_summary.as_deref(), Some("A classic."));
        assert_eq!(rec.tags, vec!["disco"]);
        assert_eq!(rec.listeners, Some(10));
    }
}
