//! Dev-only fixture capture bin.
//!
//! Runs sonara analysis over one or more audio files and freezes each result as
//! an [`AnalysisRecord`] JSON under an output directory. These records are the
//! frozen fixtures the deterministic graph gate builds from (P5) — captured
//! sonara output, so the gate needs no audio and never re-runs analysis.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --bin capture_fixtures -- <out_dir> <audio_file>...
//! ```
//!
//! Privacy: only the file **name** is stored in the record (`source.path`),
//! never a user directory. No audio is ever copied — JSON only lands in
//! `out_dir`.

use std::path::Path;
use std::process::ExitCode;

use sonara::analyze::analyze_file;

use sonagram::record::{AnalysisRecord, SourceInfo};
use sonagram::scan::default_analysis_config;

/// Slugify a file stem for use as an output filename. Keeps alphanumerics
/// (including CJK, which `char::is_alphanumeric` accepts), lowercases ASCII, and
/// collapses every other run into a single `-`.
fn slugify(stem: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in stem.chars() {
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
        "track".to_string()
    } else {
        trimmed
    }
}

fn capture_one(out_dir: &Path, audio: &Path) -> Result<String, String> {
    let bytes = std::fs::read(audio).map_err(|e| format!("read {}: {e}", audio.display()))?;
    let content_hash = blake3::hash(&bytes).to_hex().to_string();
    let file_size = bytes.len() as u64;

    let filename = audio
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("no file name for {}", audio.display()))?
        .to_string();
    let format = audio
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Shared with the live scanner so fixtures and scans request identical
    // features (see `sonagram::scan::DEFAULT_FEATURES`).
    let config = default_analysis_config();

    // sr = 0 → analyze at the file's native rate.
    let analysis = analyze_file(audio, 0, &config)
        .map_err(|e| format!("analyze {}: {e}", audio.display()))?;

    let source = SourceInfo {
        content_hash,
        hash_kind: "whole-file-v0".to_string(),
        path: filename.clone(),
        file_size,
        format,
    };

    let record = AnalysisRecord::from_analysis(analysis, source);
    let json = record
        .to_json_pretty()
        .map_err(|e| format!("serialize {filename}: {e}"))?;

    let stem = audio
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("track");
    let out_path = out_dir.join(format!("{}.json", slugify(stem)));
    std::fs::write(&out_path, json).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    let artist = record
        .tags
        .as_ref()
        .and_then(|t| t.artist.clone())
        .unwrap_or_else(|| "?".to_string());
    let title = record
        .tags
        .as_ref()
        .and_then(|t| t.title.clone())
        .unwrap_or_else(|| filename.clone());
    Ok(format!(
        "{} -> {} ({} - {}, {:.0} bpm, {}, dur {:.0}s)",
        filename,
        out_path.display(),
        artist,
        title,
        record.analysis.bpm,
        record.analysis.key.as_deref().unwrap_or("?"),
        record.analysis.duration_sec,
    ))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: {} <out_dir> <audio_file>...", args[0]);
        return ExitCode::FAILURE;
    }

    let out_dir = Path::new(&args[1]);
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        eprintln!("create {}: {e}", out_dir.display());
        return ExitCode::FAILURE;
    }

    let mut failures = 0;
    for audio in &args[2..] {
        match capture_one(out_dir, Path::new(audio)) {
            Ok(msg) => println!("ok: {msg}"),
            Err(e) => {
                eprintln!("FAIL: {e}");
                failures += 1;
            }
        }
    }

    if failures > 0 {
        eprintln!("{failures} file(s) failed");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
