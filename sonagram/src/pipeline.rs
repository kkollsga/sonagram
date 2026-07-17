//! Parallel scan + Last.fm enrichment (P20).
//!
//! Scanning is CPU-heavy (sonara DSP saturates the cores) while enrichment is
//! network-heavy (rate-limited Last.fm calls, ~5 req/s) — run together they
//! cost each other nothing. The enrich side loops [`enrich_library_with`]
//! passes while the scan streams records to disk (P20 incremental writes):
//! each pass picks up newly analyzed tracks — the Last.fm store is already
//! incremental and resumable — and one final pass after the scan completes
//! catches the tail. A pass that fails (network down, cache unwritable) is
//! retried on the next poll; enrichment trouble never aborts a scan.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::enrich::{self, EnrichOptions, EnrichReport, LastfmApi, UreqClient};
use crate::scan::{self, ScanOptions, ScanReport};
use crate::{Result, SonagramError};

/// How long the enrich loop idles between passes while the scan is running.
pub const ENRICH_POLL: Duration = Duration::from_secs(60);

/// The outcome of a combined scan + enrich run.
#[derive(Debug)]
pub struct ScanEnrichReport {
    /// The scan's report.
    pub scan: ScanReport,
    /// Accumulated enrichment counts across every pass, or `None` when no
    /// Last.fm key was configured (the run degraded to a plain scan).
    pub enrich: Option<EnrichReport>,
}

/// Scan `library_root` while concurrently enriching it from Last.fm.
///
/// The API key is resolved up front ([`enrich::api_key`]); without one the run
/// degrades gracefully to a plain scan (`enrich: None`) — a missing key must
/// never fail a scan.
pub fn scan_and_enrich_library(
    library_root: &Path,
    scan_opts: &ScanOptions,
    enrich_opts: &EnrichOptions,
) -> Result<ScanEnrichReport> {
    match enrich::api_key(library_root, enrich_opts.api_key.as_deref()) {
        Ok(key) => {
            let client = UreqClient::new(key);
            scan_and_enrich_library_with(library_root, scan_opts, enrich_opts, &client)
        }
        Err(_) => {
            let scan = scan::scan_library(library_root, scan_opts)?;
            Ok(ScanEnrichReport { scan, enrich: None })
        }
    }
}

/// [`scan_and_enrich_library`] with an injected [`LastfmApi`] (the seam tests
/// drive). Runs the scan on the calling thread and the enrich loop on a scoped
/// thread; both write only through their own atomic, incremental stores.
pub fn scan_and_enrich_library_with(
    library_root: &Path,
    scan_opts: &ScanOptions,
    enrich_opts: &EnrichOptions,
    client: &(dyn LastfmApi + Sync),
) -> Result<ScanEnrichReport> {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let enricher = scope.spawn(|| {
            let start = Instant::now();
            let mut acc = EnrichReport::default();
            loop {
                // A failed pass (network, unwritable cache) is soft: retry on
                // the next poll. Per-entity failures inside a pass are already
                // negative-cached by the store and never refetched.
                if let Ok(r) = enrich::enrich_library_with(library_root, enrich_opts, client) {
                    accumulate(&mut acc, &r);
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // Idle in small steps so a finished scan releases us promptly.
                let mut idled = Duration::ZERO;
                while idled < ENRICH_POLL && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    idled += Duration::from_millis(250);
                }
            }
            // Final pass: records analyzed after the last in-loop pass began.
            if let Ok(r) = enrich::enrich_library_with(library_root, enrich_opts, client) {
                accumulate(&mut acc, &r);
            }
            acc.elapsed = start.elapsed();
            acc
        });

        let scan_result = scan::scan_library(library_root, scan_opts);
        stop.store(true, Ordering::Relaxed);
        let enrich_report = enricher
            .join()
            .map_err(|_| SonagramError::Enrich("enrich loop panicked".to_string()));
        let scan = scan_result?;
        Ok(ScanEnrichReport {
            scan,
            enrich: Some(enrich_report?),
        })
    })
}

/// Fold one pass's report into the accumulated totals. Fetch/fail counts sum
/// (a pass only ever fetches entities no earlier pass covered — failures are
/// negative-cached); skip counts take the latest pass's view.
fn accumulate(acc: &mut EnrichReport, pass: &EnrichReport) {
    acc.artists_fetched += pass.artists_fetched;
    acc.artists_failed += pass.artists_failed;
    acc.artists_skipped = pass.artists_skipped;
    acc.tracks_fetched += pass.tracks_fetched;
    acc.tracks_failed += pass.tracks_failed;
    acc.tracks_skipped = pass.tracks_skipped;
    acc.albums_fetched += pass.albums_fetched;
    acc.albums_failed += pass.albums_failed;
    acc.albums_skipped = pass.albums_skipped;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulate_sums_fetches_and_keeps_latest_skips() {
        let mut acc = EnrichReport::default();
        let mut pass = EnrichReport {
            artists_fetched: 3,
            artists_skipped: 1,
            tracks_failed: 2,
            ..EnrichReport::default()
        };
        accumulate(&mut acc, &pass);
        pass.artists_fetched = 2;
        pass.artists_skipped = 4;
        accumulate(&mut acc, &pass);
        assert_eq!(acc.artists_fetched, 5, "fetches sum across passes");
        assert_eq!(acc.artists_skipped, 4, "skips reflect the latest pass");
        assert_eq!(acc.tracks_failed, 4, "failures sum across passes");
    }
}
