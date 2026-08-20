//! Best-effort on-disk progress files (P20).
//!
//! Long operations (scan, enrich) write a small JSON snapshot of their live
//! state under `<library>/.sonagram/`, atomically and throttled, so progress is
//! observable from **any** entry point — the CLI, the Python API, or an outside
//! observer (`sonagram progress` / `sonagram status`) — without depending on
//! how stdout is wired.
//!
//! Progress files are telemetry, not state: a write failure is swallowed (the
//! operation's own results are never gated on them), and readers must treat a
//! stale `updated_unix` as "the writer is gone", not as truth.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Whole seconds since the Unix epoch, now.
pub fn unix_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// A throttled, atomic, **best-effort** JSON snapshot writer.
///
/// `write` serializes the snapshot and atomically replaces `path`, at most once
/// per `min_interval` (unless `force`d — stage transitions and final states
/// should always land). All IO errors are swallowed by design: progress
/// telemetry must never fail the operation it observes.
pub struct ProgressWriter {
    path: PathBuf,
    min_interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl ProgressWriter {
    /// A writer targeting `path`, writing at most once per `min_interval`.
    pub fn new(path: PathBuf, min_interval: Duration) -> Self {
        ProgressWriter {
            path,
            min_interval,
            last: Mutex::new(None),
        }
    }

    /// Write `snapshot` if `force` or the throttle window has passed.
    pub fn write<T: Serialize>(&self, snapshot: &T, force: bool) {
        {
            let mut last = match self.last.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if !force {
                if let Some(t) = *last {
                    if t.elapsed() < self.min_interval {
                        return;
                    }
                }
            }
            *last = Some(Instant::now());
        }
        let Ok(json) = serde_json::to_string_pretty(snapshot) else {
            return;
        };
        let _ = atomic_write_best_effort(&self.path, json.as_bytes());
    }
}

/// Atomic write (unique temp sibling + rename), mirroring the scan cache's
/// discipline, but with the parent dir created on demand and errors returned
/// for the caller to swallow.
fn atomic_write_best_effort(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent dir"))?;
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("bad file name"))?;
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Load and parse a progress file, `None` when absent or unparseable (a
/// half-migrated or foreign file must never break a probe).
pub fn load_progress<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Snap {
        stage: String,
        done: usize,
    }

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sonagram-progress-{}-{}-{}.json",
            std::process::id(),
            name,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn write_and_load_round_trip() {
        let path = tmp_path("roundtrip");
        let w = ProgressWriter::new(path.clone(), Duration::from_secs(0));
        let snap = Snap {
            stage: "analyze".to_string(),
            done: 7,
        };
        w.write(&snap, false);
        assert_eq!(load_progress::<Snap>(&path), Some(snap));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn throttle_skips_but_force_writes() {
        let path = tmp_path("throttle");
        let w = ProgressWriter::new(path.clone(), Duration::from_secs(3600));
        w.write(
            &Snap {
                stage: "a".into(),
                done: 1,
            },
            false,
        );
        // Throttled: the second unforced write is dropped.
        w.write(
            &Snap {
                stage: "a".into(),
                done: 2,
            },
            false,
        );
        assert_eq!(load_progress::<Snap>(&path).unwrap().done, 1);
        // Forced: lands regardless of the throttle window.
        w.write(
            &Snap {
                stage: "a".into(),
                done: 3,
            },
            true,
        );
        assert_eq!(load_progress::<Snap>(&path).unwrap().done, 3);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_missing_or_garbage_is_none() {
        let path = tmp_path("missing");
        assert_eq!(load_progress::<Snap>(&path), None);
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(load_progress::<Snap>(&path), None);
        std::fs::remove_file(&path).ok();
    }
}
