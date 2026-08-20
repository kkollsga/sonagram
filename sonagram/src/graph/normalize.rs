//! Deterministic normalization for dimension-node identity, plus the static
//! seed tables (musical keys, tempo bands) the graph always materializes.
//!
//! Every function here is pure and total: the same input always yields the same
//! output, with no `HashMap`, wall-clock, or filesystem influence. That is what
//! lets the graph builder derive byte-stable node ids from analysis records.
//!
//! ## Dimension identity rules (from `designs/graph-schema.md`)
//! - **Artist** id = trimmed artist tag; a missing/empty tag → `"Unknown Artist"`.
//! - **Album**  id = `"<artist-id>|<album>"` (artist-qualified to disambiguate
//!   same-named albums); only when an album tag is present.
//! - **Genre**  id = trimmed, lowercased tag (casing normalized so `"Pop"` and
//!   `"pop"` collapse to one node); only when a genre tag is present.
//! - **Decade** id = `"<floor-to-10>s"`, e.g. year 1974 → `"1970s"`.
//! - **TempoBand** — see [`tempo_band`]; **Key** — see [`KEYS`].

/// Fallback artist id when a track carries no usable artist tag.
pub const UNKNOWN_ARTIST: &str = "Unknown Artist";

/// The artist-node id for an (optional, raw) artist tag: trimmed, or
/// [`UNKNOWN_ARTIST`] when the tag is absent or blank.
///
/// This doubles as the artist *display name* — the graph stores no separate
/// raw/normalized artist forms in v1.
pub fn artist_id(artist_tag: Option<&str>) -> String {
    match artist_tag.map(str::trim) {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => UNKNOWN_ARTIST.to_string(),
    }
}

/// The album-node id: `"<artist_id>|<album>"`, or `None` when the album tag is
/// absent or blank. `artist_id` is the already-normalized artist id so the same
/// album title under two artists yields two nodes.
pub fn album_id(artist_id: &str, album_tag: Option<&str>) -> Option<String> {
    match album_tag.map(str::trim) {
        Some(a) if !a.is_empty() => Some(format!("{artist_id}|{a}")),
        _ => None,
    }
}

/// The trimmed album display name, or `None` when absent/blank.
pub fn album_name(album_tag: Option<&str>) -> Option<String> {
    match album_tag.map(str::trim) {
        Some(a) if !a.is_empty() => Some(a.to_string()),
        _ => None,
    }
}

/// The genre-node id: trimmed + lowercased, or `None` when absent/blank. Casing
/// is normalized so tag variants collapse to one node; the same string is used
/// as the genre display name.
pub fn genre_id(genre_tag: Option<&str>) -> Option<String> {
    match genre_tag.map(str::trim) {
        Some(g) if !g.is_empty() => Some(g.to_lowercase()),
        _ => None,
    }
}

/// The decade-node id for a release year: the year floored to a multiple of ten
/// with an `"s"` suffix (1974 → `"1970s"`, 2009 → `"2000s"`).
pub fn decade_id(year: u32) -> String {
    format!("{}s", (year / 10) * 10)
}

/// Canonical title form for **version keying** (P21 Stage C): the string that,
/// paired with an [`artist_id`], groups every recording of the same underlying
/// song regardless of edition. It folds away the edition/mastering decoration
/// that labels the *same* composition differently (`"- Remastered 2009"`,
/// `"(Live)"`, `"- Take 5"`, `"(Mono)"`), so studio master, session outtake and
/// live take share one key.
///
/// The transform, in order:
/// 1. lowercase, and fold the three common Unicode apostrophes
///    (`’ ‘ ʼ`) to ASCII `'` so `"I’d"` and `"I'd"` key identically;
/// 2. drop **every** parenthesized `(...)` and bracketed `[...]` group — these
///    almost always carry edition/feat./year decoration, and dropping the year-
///    only parenthetical is called out in the spec;
/// 3. drop a trailing `" - <marker>…"` segment when the segment (after removing
///    any standalone 4-digit year token) is empty or begins with a known edition
///    marker (`live`, `remaster`/`remastered`, `mono`, `stereo`, `take N`,
///    `demo`, `version`, `edit`, `single version`, `album version`) — repeated so
///    stacked markers all fall off;
/// 4. collapse internal whitespace and trim surrounding `-_.` and spaces.
///
/// It is pure and total: the same input always yields the same output, so it is
/// safe as a node-id component. This iteration keys on **title + artist only**;
/// the `song` grouping module deliberately structures grouping so a later
/// refinement can split an over-merged key by embedding/duration without changing
/// this function.
pub fn normalized_title(title: &str) -> String {
    // 1. Lowercase + apostrophe fold.
    let lowered = title
        .to_lowercase()
        .replace(['\u{2019}', '\u{2018}', '\u{02bc}'], "'");
    // 2. Strip bracketed/parenthesized groups.
    let debracketed = strip_bracket_groups(&lowered);
    let collapsed = collapse_ws(&debracketed);
    // 3. Strip trailing edition markers.
    let trimmed_markers = strip_trailing_markers(&collapsed);
    // 4. Final collapse + trim of separator punctuation.
    collapse_ws(&trimmed_markers)
        .trim_matches(|c: char| c == '-' || c == '_' || c == '.' || c.is_whitespace())
        .to_string()
}

/// Remove every `(...)` / `[...]` group (non-nested; unmatched openers drop the
/// remainder). Content is discarded wholesale — for version keying the decoration
/// inside brackets is exactly what must not distinguish two takes.
fn strip_bracket_groups(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Collapse any run of whitespace to a single space and trim the ends.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop a trailing `" - <marker>…"` segment while one is present. Uses the last
/// `" - "` so a title that itself contains a dash phrase is only trimmed when the
/// *final* segment reads as an edition marker.
fn strip_trailing_markers(s: &str) -> String {
    let mut cur = s.to_string();
    while let Some(pos) = cur.rfind(" - ") {
        if is_edition_marker(cur[pos + 3..].trim()) {
            cur.truncate(pos);
        } else {
            break;
        }
    }
    cur
}

/// Whether a post-separator tail reads as an edition/mastering marker rather than
/// part of the title. `take N` needs a numeric argument; a tail that is only
/// 4-digit year tokens qualifies (the `"- 2009"` reissue stamp); otherwise the
/// tail must begin with one of the marker words after its year tokens are removed
/// (so both `"2009 remaster"` and `"remastered 2009"` match).
fn is_edition_marker(tail: &str) -> bool {
    if tail.is_empty() {
        return false;
    }
    if let Some(rest) = tail.strip_prefix("take ") {
        return rest
            .split_whitespace()
            .next()
            .is_some_and(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()));
    }
    let is_year = |w: &str| w.len() == 4 && w.chars().all(|c| c.is_ascii_digit());
    let non_year: Vec<&str> = tail.split_whitespace().filter(|w| !is_year(w)).collect();
    if non_year.is_empty() {
        return true; // tail was only year token(s), e.g. "2009"
    }
    const MARKERS: [&str; 10] = [
        "live",
        "remaster",
        "remastered",
        "mono",
        "stereo",
        "demo",
        "version",
        "edit",
        "single version",
        "album version",
    ];
    let joined = non_year.join(" ");
    MARKERS
        .iter()
        .any(|m| joined == *m || joined.starts_with(&format!("{m} ")))
}

/// The file-name component of a (possibly `/`-joined, relative) source path.
/// Returns the whole string when it has no separator.
pub fn filename_from_path(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

// ─────────────────────────── Tempo bands (static) ───────────────────────────

/// The named tempo bands, in ascending order: `(name, lo_inclusive,
/// hi_exclusive)` in BPM. The graph always materializes all seven `TempoBand`
/// nodes so `graph_overview` reveals the full tempo axis even for a small
/// library.
///
/// **Boundary rule:** intervals are half-open `[lo, hi)` — a BPM exactly on a
/// boundary belongs to the **higher** band (120.0 is `mid` since `mid` is
/// `[110, 125)`... 125.0 is `upbeat`, not `house`). Encoded so the classifier
/// and the seed agree.
pub const TEMPO_BANDS: [(&str, f32, f32); 7] = [
    ("glacial", f32::NEG_INFINITY, 70.0),
    ("downtempo", 70.0, 90.0),
    ("mid", 90.0, 110.0),
    ("house", 110.0, 125.0),
    ("upbeat", 125.0, 140.0),
    ("fast", 140.0, 160.0),
    ("frantic", 160.0, f32::INFINITY),
];

/// Classify a BPM into one of [`TEMPO_BANDS`]. See the boundary rule there: a
/// value exactly on a boundary lands in the higher band. Non-finite input
/// (NaN) falls through to `"frantic"`.
pub fn tempo_band(bpm: f32) -> &'static str {
    for (name, lo, hi) in TEMPO_BANDS {
        if bpm >= lo && bpm < hi {
            return name;
        }
    }
    // Only +∞ or NaN reach here; both are pathological. Bucket as frantic.
    "frantic"
}

// ─────────────────────────── Musical keys (static) ──────────────────────────
//
// Mirrors sonara's key-string + Camelot tables (`sonara::perceptual`) so the
// 24 static `Key` nodes carry exactly the strings sonara emits into
// `TrackAnalysis::key` / `key_camelot` — otherwise `IN_KEY` edges would find no
// endpoint. The P5 contract test guards against upstream drift.

/// Note names in pitch-class order, sharp spellings — sonara's `NOTE_NAMES`.
const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
/// Camelot codes for minor keys, indexed by pitch class — sonara's `CAMELOT_MINOR`.
const CAMELOT_MINOR: [&str; 12] = [
    "5A", "12A", "7A", "2A", "9A", "4A", "11A", "6A", "1A", "8A", "3A", "10A",
];
/// Camelot codes for major keys, indexed by pitch class — sonara's `CAMELOT_MAJOR`.
const CAMELOT_MAJOR: [&str; 12] = [
    "8B", "3B", "10B", "5B", "12B", "7B", "2B", "9B", "4B", "11B", "6B", "1B",
];

/// One static musical key: `(name, camelot, mode)` where `name` matches
/// sonara's `format_key` output (`"A minor"`, `"C major"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySeed {
    /// Human key string, e.g. `"A minor"` — the `Key` node id and title.
    pub name: &'static str,
    /// Camelot-wheel code, e.g. `"8A"`.
    pub camelot: &'static str,
    /// `"minor"` or `"major"`.
    pub mode: &'static str,
}

/// The 24 musical keys (12 pitch classes × {minor, major}), always all
/// materialized as `Key` nodes. Order is pitch-class then minor-before-major,
/// which is deterministic; node identity does not depend on this order (it is
/// keyed by `name`).
pub const KEYS: [KeySeed; 24] = build_keys();

const fn build_keys() -> [KeySeed; 24] {
    // Const context: no iterators/format!. Names are compile-time literals
    // paired with each pitch class via a lookup table.
    const MINOR_NAMES: [&str; 12] = [
        "C minor", "C# minor", "D minor", "D# minor", "E minor", "F minor", "F# minor", "G minor",
        "G# minor", "A minor", "A# minor", "B minor",
    ];
    const MAJOR_NAMES: [&str; 12] = [
        "C major", "C# major", "D major", "D# major", "E major", "F major", "F# major", "G major",
        "G# major", "A major", "A# major", "B major",
    ];
    let mut out = [KeySeed {
        name: "C minor",
        camelot: "5A",
        mode: "minor",
    }; 24];
    let mut pc = 0;
    let mut i = 0;
    while pc < 12 {
        out[i] = KeySeed {
            name: MINOR_NAMES[pc],
            camelot: CAMELOT_MINOR[pc],
            mode: "minor",
        };
        out[i + 1] = KeySeed {
            name: MAJOR_NAMES[pc],
            camelot: CAMELOT_MAJOR[pc],
            mode: "major",
        };
        // Silence unused-const lint paths: NOTE_NAMES documents the ordering.
        let _ = NOTE_NAMES[pc];
        pc += 1;
        i += 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_missing_and_blank_fall_back() {
        assert_eq!(artist_id(None), "Unknown Artist");
        assert_eq!(artist_id(Some("   ")), "Unknown Artist");
        assert_eq!(artist_id(Some("  ABBA ")), "ABBA");
    }

    #[test]
    fn album_id_is_artist_qualified() {
        assert_eq!(
            album_id("Bruno Mars", Some("Doo-Wops & Hooligans")),
            Some("Bruno Mars|Doo-Wops & Hooligans".to_string())
        );
        assert_eq!(album_id("X", None), None);
        assert_eq!(album_id("X", Some("  ")), None);
    }

    #[test]
    fn genre_casing_is_normalized() {
        assert_eq!(genre_id(Some("Pop")), Some("pop".to_string()));
        assert_eq!(genre_id(Some(" R&B ")), Some("r&b".to_string()));
        assert_eq!(genre_id(None), None);
        assert_eq!(genre_id(Some("")), None);
    }

    #[test]
    fn decade_floors_to_ten() {
        assert_eq!(decade_id(1974), "1970s");
        assert_eq!(decade_id(1970), "1970s");
        assert_eq!(decade_id(1979), "1970s");
        assert_eq!(decade_id(2009), "2000s");
        assert_eq!(decade_id(1967), "1960s");
    }

    #[test]
    fn normalized_title_folds_editions_and_case() {
        // Bare title: lowercase only.
        assert_eq!(normalized_title("Yesterday"), "yesterday");
        assert_eq!(normalized_title("  Let It Be  "), "let it be");
        // Parenthesized + bracketed decoration is dropped wholesale.
        assert_eq!(normalized_title("Yesterday (Remastered)"), "yesterday");
        assert_eq!(normalized_title("Yesterday (2009)"), "yesterday");
        assert_eq!(normalized_title("Come Together [Mono]"), "come together");
        assert_eq!(normalized_title("Help! (feat. Someone)"), "help!");
        // Trailing dash markers, including year-decorated remaster spellings.
        assert_eq!(normalized_title("Yesterday - Live"), "yesterday");
        assert_eq!(normalized_title("Yesterday - Live at the BBC"), "yesterday");
        assert_eq!(normalized_title("Yesterday - Remastered 2009"), "yesterday");
        assert_eq!(normalized_title("Yesterday - 2009 Remaster"), "yesterday");
        assert_eq!(normalized_title("Yesterday - Mono"), "yesterday");
        assert_eq!(normalized_title("Yesterday - Stereo"), "yesterday");
        assert_eq!(normalized_title("Yesterday - Take 5"), "yesterday");
        assert_eq!(normalized_title("Yesterday - Demo"), "yesterday");
        assert_eq!(normalized_title("Yesterday - Single Version"), "yesterday");
        assert_eq!(normalized_title("Yesterday - Album Version"), "yesterday");
        assert_eq!(normalized_title("Yesterday - 2009"), "yesterday");
        // Stacked markers all fall off.
        assert_eq!(
            normalized_title("Yesterday (Remastered) - Live - Mono"),
            "yesterday"
        );
    }

    #[test]
    fn normalized_title_normalizes_unicode_apostrophes() {
        // Curly (U+2019), left (U+2018), modifier-letter (U+02BC) all fold to '.
        let curly = normalized_title("I\u{2019}d Much Rather Be With the Boys");
        let straight = normalized_title("I'd Much Rather Be With the Boys");
        assert_eq!(curly, "i'd much rather be with the boys");
        assert_eq!(curly, straight, "apostrophe variants must key identically");
        assert_eq!(
            normalized_title("Don\u{2018}t Stop"),
            normalized_title("Don't Stop")
        );
    }

    #[test]
    fn normalized_title_preserves_non_marker_dash_phrases() {
        // A genuine dash phrase that is NOT an edition marker stays intact.
        assert_eq!(
            normalized_title("Marie - A Little Cajun Waltz"),
            "marie - a little cajun waltz"
        );
        // "Take" without a number is not a take marker.
        assert_eq!(normalized_title("Give and Take"), "give and take");
        // A real title that happens to start with "Live" (no separator) survives.
        assert_eq!(normalized_title("Live and Let Die"), "live and let die");
    }

    #[test]
    fn normalized_title_trims_separator_punctuation() {
        // Punctuation left after stripping is trimmed off the ends.
        assert_eq!(normalized_title("Song."), "song");
        assert_eq!(normalized_title("_Intro_"), "intro");
        assert_eq!(
            normalized_title("(Live)"),
            "",
            "all-decoration title collapses"
        );
    }

    #[test]
    fn filename_strips_directories() {
        assert_eq!(
            filename_from_path("a/b/10 Jive Talkin'.mp3"),
            "10 Jive Talkin'.mp3"
        );
        assert_eq!(filename_from_path("track.mp3"), "track.mp3");
    }

    #[test]
    fn tempo_band_boundaries_favor_higher_band() {
        // Interiors.
        assert_eq!(tempo_band(60.0), "glacial");
        assert_eq!(tempo_band(80.0), "downtempo");
        assert_eq!(tempo_band(100.0), "mid");
        assert_eq!(tempo_band(118.0), "house");
        assert_eq!(tempo_band(130.0), "upbeat");
        assert_eq!(tempo_band(150.0), "fast");
        assert_eq!(tempo_band(180.0), "frantic");
        // Exact boundaries land in the HIGHER band (half-open [lo, hi)).
        assert_eq!(tempo_band(70.0), "downtempo");
        assert_eq!(tempo_band(90.0), "mid");
        assert_eq!(tempo_band(110.0), "house");
        assert_eq!(
            tempo_band(125.0),
            "upbeat",
            "125 BPM must be upbeat, not house"
        );
        assert_eq!(tempo_band(140.0), "fast");
        assert_eq!(tempo_band(160.0), "frantic");
    }

    #[test]
    fn keys_seed_is_24_and_matches_sonara_strings() {
        assert_eq!(KEYS.len(), 24);
        // Spot-check the three keys the fixtures actually use.
        let find = |name: &str| KEYS.iter().find(|k| k.name == name).copied();
        let a_minor = find("A minor").expect("A minor present");
        assert_eq!(a_minor.camelot, "8A");
        assert_eq!(a_minor.mode, "minor");
        let c_major = find("C major").expect("C major present");
        assert_eq!(c_major.camelot, "8B");
        let f_major = find("F major").expect("F major present");
        assert_eq!(f_major.camelot, "7B");
        assert_eq!(f_major.mode, "major");
        // All names unique.
        let mut names: Vec<&str> = KEYS.iter().map(|k| k.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 24);
    }
}
