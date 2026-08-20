//! Audio content hashing — the stable identity of a `Track`.
//!
//! A track's graph identity is the **hash of its audio bytes**, not its path or
//! tags. Re-tagging a file (which rewrites its ID3 container) or moving it must
//! not change that identity, so for MP3 we hash the audio payload with the ID3
//! metadata containers stripped off:
//!
//! - a leading **ID3v2** container (`"ID3"` magic; a syncsafe u28 size at bytes
//!   6..10, plus a 10-byte footer when the footer flag is set), and
//! - a trailing **ID3v1** block (last 128 bytes starting with `"TAG"`), including
//!   the enhanced **`"TAG+"`** 227-byte block that precedes it.
//!
//! Everything between those boundaries — the actual MPEG frames — is fed to
//! blake3. Two files with identical audio but different tags therefore hash
//! **equal**; a single changed audio byte changes the hash.
//!
//! **Scope (v1):** only ID3v1/ID3v2 are recognised. Other trailing metadata
//! (APEv2, Lyrics3) is *not* stripped and would ride along in the hash — see the
//! post-v1 todo. For a malformed/garbage ID3 header the algorithm falls back to
//! hashing the whole file rather than panicking. Non-MP3 extensions always hash
//! the whole file (`whole-file-v0`).

use std::path::Path;

use crate::Result;

/// `hash_kind` value for MP3 audio hashed with ID3 containers stripped.
pub const KIND_MP3_AUDIO: &str = "mp3-audio-v1";
/// `hash_kind` value for a whole-file hash (non-MP3, or MP3 fallback).
pub const KIND_WHOLE_FILE: &str = "whole-file-v0";

/// How the content hash for `path` is computed, based purely on its extension.
///
/// This is the value stored in [`SourceInfo::hash_kind`](crate::record::SourceInfo::hash_kind)
/// so a record self-describes which algorithm produced its hash. A tag-less MP3
/// still reports `mp3-audio-v1` even though its hash equals the raw-file hash —
/// "strip ID3, hash the rest" degenerates to "hash the whole file" when there is
/// nothing to strip, and that is a defined outcome of the v1 algorithm.
pub fn hash_kind(path: &Path) -> &'static str {
    if is_mp3(path) {
        KIND_MP3_AUDIO
    } else {
        KIND_WHOLE_FILE
    }
}

/// Content hash of the audio at `path`, hex-encoded.
///
/// For `*.mp3` this strips ID3 containers first (see module docs); for any other
/// extension it hashes the whole file. Reads the file fully into memory (music
/// tracks are a few MB); returns an IO error if the read fails.
pub fn audio_content_hash(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    let digest = if is_mp3(path) {
        let (start, end) = mp3_audio_range(&bytes);
        blake3::hash(&bytes[start..end])
    } else {
        blake3::hash(&bytes)
    };
    Ok(digest.to_hex().to_string())
}

fn is_mp3(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mp3"))
        .unwrap_or(false)
}

/// The `[start, end)` byte range of the MPEG audio inside `bytes`, i.e. the file
/// with any leading ID3v2 and trailing ID3v1/`TAG+` blocks removed.
///
/// On a malformed leading header (claimed size runs past EOF) it declines to
/// strip and returns the whole range — never panics, never slices out of bounds.
fn mp3_audio_range(bytes: &[u8]) -> (usize, usize) {
    let mut start = 0usize;
    let mut end = bytes.len();

    // -- Leading ID3v2 container --
    if bytes.len() >= 10 && &bytes[0..3] == b"ID3" {
        let flags = bytes[5];
        let size = syncsafe_u28(&bytes[6..10]);
        // Header is 10 bytes; add 10 more when the footer-present flag (0x10) is set.
        let footer = if flags & 0x10 != 0 { 10 } else { 0 };
        // `size` is the tag body only. Total container = 10 + body + optional footer.
        match 10usize
            .checked_add(size)
            .and_then(|n| n.checked_add(footer))
        {
            // Only strip when the container fits inside the file; a claimed size
            // past EOF is garbage → fall back to whole-file (start stays 0).
            Some(skip) if skip <= bytes.len() => start = skip,
            _ => {}
        }
    }

    // -- Trailing ID3v1 (128 bytes) + optional enhanced TAG+ (227 bytes) --
    if end - start >= 128 && &bytes[end - 128..end - 125] == b"TAG" {
        end -= 128;
        if end - start >= 227 && &bytes[end - 227..end - 223] == b"TAG+" {
            end -= 227;
        }
    }

    (start, end)
}

/// Decode a 4-byte ID3v2 syncsafe integer (7 significant bits per byte, MSB
/// always 0) into its `u28` value.
fn syncsafe_u28(b: &[u8]) -> usize {
    ((b[0] & 0x7f) as usize) << 21
        | ((b[1] & 0x7f) as usize) << 14
        | ((b[2] & 0x7f) as usize) << 7
        | ((b[3] & 0x7f) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a fake ID3v2 header whose body is `body_len` bytes, no footer.
    fn id3v2_header(body_len: usize) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"ID3"); // magic
        h.push(4); // version major
        h.push(0); // version minor
        h.push(0); // flags (no footer)
                   // syncsafe size (7 bits/byte)
        h.push(((body_len >> 21) & 0x7f) as u8);
        h.push(((body_len >> 14) & 0x7f) as u8);
        h.push(((body_len >> 7) & 0x7f) as u8);
        h.push((body_len & 0x7f) as u8);
        h
    }

    /// A synthetic "mp3": ID3v2(tag) + audio + optional ID3v1(128).
    fn synthetic_mp3(tag: &[u8], audio: &[u8], id3v1: bool) -> Vec<u8> {
        let mut v = id3v2_header(tag.len());
        v.extend_from_slice(tag);
        v.extend_from_slice(audio);
        if id3v1 {
            let mut tail = Vec::new();
            tail.extend_from_slice(b"TAG");
            tail.resize(128, 0u8); // pad the v1 block to 128 bytes
            v.extend_from_slice(&tail);
        }
        v
    }

    fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sonagram-hash-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    fn hash_of(path: &Path) -> String {
        audio_content_hash(path).unwrap()
    }

    #[test]
    fn same_audio_different_tags_hash_equal() {
        let audio = b"THE-SAME-AUDIO-FRAMES-0123456789";
        let a = write_tmp("a.mp3", &synthetic_mp3(b"tagsA", audio, false));
        let b = write_tmp(
            "b.mp3",
            &synthetic_mp3(b"a-much-longer-tag-payload", audio, true),
        );
        assert_eq!(
            hash_of(&a),
            hash_of(&b),
            "tag content must not affect the hash"
        );
    }

    #[test]
    fn different_audio_hashes_differ() {
        let a = write_tmp("c.mp3", &synthetic_mp3(b"tag", b"AUDIO-ONE", false));
        let b = write_tmp("d.mp3", &synthetic_mp3(b"tag", b"AUDIO-TWO", false));
        assert_ne!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn no_tags_equals_raw_hash() {
        // An mp3 with no ID3 markers hashes exactly the raw bytes.
        let audio = b"raw-audio-without-any-tags";
        let p = write_tmp("e.mp3", audio);
        let expect = blake3::hash(audio).to_hex().to_string();
        assert_eq!(hash_of(&p), expect);
    }

    #[test]
    fn id3v1_tail_stripped() {
        let audio = b"payload-payload-payload";
        let with_v1 = write_tmp("f.mp3", &synthetic_mp3(b"", audio, true));
        let without = write_tmp("g.mp3", &synthetic_mp3(b"", audio, false));
        assert_eq!(hash_of(&with_v1), hash_of(&without));
    }

    #[test]
    fn tag_plus_enhanced_tail_stripped() {
        let audio = b"enhanced-tail-audio-bytes";
        // audio + TAG+ (227 bytes) + TAG (128 bytes)
        let mut with_ext = synthetic_mp3(b"", audio, false);
        let mut tagplus = Vec::new();
        tagplus.extend_from_slice(b"TAG+");
        tagplus.resize(227, 7u8);
        with_ext.extend_from_slice(&tagplus);
        let mut v1 = Vec::new();
        v1.extend_from_slice(b"TAG");
        v1.resize(128, 0u8);
        with_ext.extend_from_slice(&v1);

        let plain = synthetic_mp3(b"", audio, false);
        let a = write_tmp("h.mp3", &with_ext);
        let b = write_tmp("i.mp3", &plain);
        assert_eq!(
            hash_of(&a),
            hash_of(&b),
            "TAG+ enhanced block must be stripped"
        );
    }

    #[test]
    fn footer_flag_stripped() {
        let audio = b"footered-audio";
        let body = b"tagbody";
        // Header with footer flag set (0x10), then body, then 10-byte footer.
        let mut v = Vec::new();
        v.extend_from_slice(b"ID3");
        v.push(4);
        v.push(0);
        v.push(0x10); // footer present
        v.push(0);
        v.push(0);
        v.push(0);
        v.push(body.len() as u8);
        v.extend_from_slice(body);
        v.extend_from_slice(b"3DIfooter1"); // 10-byte footer, between body and audio
        v.extend_from_slice(audio);
        let footered = write_tmp("j.mp3", &v);
        let plain = write_tmp("k.mp3", &synthetic_mp3(b"", audio, false));
        assert_eq!(hash_of(&footered), hash_of(&plain));
    }

    #[test]
    fn garbage_header_does_not_panic_and_falls_back() {
        // "ID3" magic but a syncsafe size claiming far past EOF → whole-file hash.
        let mut v = Vec::new();
        v.extend_from_slice(b"ID3");
        v.push(4);
        v.push(0);
        v.push(0);
        v.extend_from_slice(&[0x7f, 0x7f, 0x7f, 0x7f]); // huge claimed size
        v.extend_from_slice(b"short");
        let p = write_tmp("l.mp3", &v);
        // Must not panic; falls back to hashing the whole file.
        let expect = blake3::hash(&v).to_hex().to_string();
        assert_eq!(hash_of(&p), expect);
    }

    #[test]
    fn truncated_header_does_not_panic() {
        // Fewer than 10 bytes but starts with "ID3".
        let p = write_tmp("m.mp3", b"ID3xx");
        let _ = hash_of(&p); // just must not panic
    }

    #[test]
    fn non_mp3_uses_whole_file() {
        assert_eq!(hash_kind(Path::new("x.flac")), KIND_WHOLE_FILE);
        assert_eq!(hash_kind(Path::new("x.MP3")), KIND_MP3_AUDIO);
        let audio = synthetic_mp3(b"tag", b"audio", false);
        let p = write_tmp("n.flac", &audio);
        // Non-mp3 → whole file including the ID3 bytes.
        let expect = blake3::hash(&audio).to_hex().to_string();
        assert_eq!(hash_of(&p), expect);
    }
}
