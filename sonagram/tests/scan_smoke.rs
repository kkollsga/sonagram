//! Real-audio smoke test — **maintainer machine only**, never CI.
//!
//! Ignored by default; runs sonara over real MP3s. Point `SONAGRAM_SMOKE_LIB` at
//! a directory of MP3s and run:
//!
//! ```text
//! SONAGRAM_SMOKE_LIB=/path/to/lib cargo test -p sonagram --test scan_smoke \
//!     -- --ignored --nocapture smoke_real_library
//! ```
//!
//! It scans once (analyzing every unseen file), then rescans, asserting the
//! second pass runs **zero** analyses and completes near-instantly.

use std::path::PathBuf;

use sonagram::scan::{scan_library, ScanOptions};

#[test]
#[ignore = "requires real audio; set SONAGRAM_SMOKE_LIB"]
fn smoke_real_library() {
    let lib = match std::env::var("SONAGRAM_SMOKE_LIB") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!("SONAGRAM_SMOKE_LIB not set; skipping");
            return;
        }
    };

    let opts = ScanOptions::default();

    let r1 = scan_library(&lib, &opts).unwrap();
    println!(
        "FIRST SCAN: total={} analyzed={} reused_hash={} reused_stat={} failed={} elapsed={:.2?}",
        r1.total_files,
        r1.analyzed,
        r1.reused_hash_match,
        r1.reused_stat_match,
        r1.failed.len(),
        r1.elapsed,
    );
    for (p, e) in &r1.failed {
        println!("  FAIL {}: {e}", p.display());
    }

    let r2 = scan_library(&lib, &opts).unwrap();
    println!(
        "RESCAN:     total={} analyzed={} reused_hash={} reused_stat={} failed={} elapsed={:.2?}",
        r2.total_files,
        r2.analyzed,
        r2.reused_hash_match,
        r2.reused_stat_match,
        r2.failed.len(),
        r2.elapsed,
    );

    assert_eq!(r2.analyzed, 0, "no-op rescan must analyze nothing");
    assert_eq!(r2.reused_stat_match, r2.total_files - r2.failed.len());
}
