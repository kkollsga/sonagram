//! User config + multi-source registry (P17).
//!
//! sonagram becomes a **config-driven** tool: instead of naming a library root
//! on every command, a user registers one or more source directories once
//! (`sonagram sources add <dir>`) and thereafter runs bare `sonagram scan` /
//! `build` / `playlist`, which fan out over the registered sources and read/write
//! a central graph + playlist store. The config is a small JSON file:
//!
//! ```json
//! {
//!   "config_version": 1,
//!   "sources": ["/abs/dir/a", "/abs/dir/b"],
//!   "graph": "/abs/music.kgl",
//!   "playlists_dir": "/abs/playlists"
//! }
//! ```
//!
//! It lives at `$SONAGRAM_HOME/config.json` (default `~/.sonagram/config.json`);
//! `graph`/`playlists_dir` are optional and fall back to
//! `$SONAGRAM_HOME/music.kgl` and `$SONAGRAM_HOME/playlists/` when unset. The
//! `$SONAGRAM_HOME` override keeps every test hermetic — a test points it at a
//! temp dir and never touches the real home.
//!
//! ## Determinism
//! `sources` is kept **sorted** (a `Vec<String>` sorted after every mutation) so
//! the registry serializes reproducibly and the multi-source graph build (which
//! iterates sources sorted, first-source-wins on a shared content hash) is
//! order-independent of the add order.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Result, SonagramError};

/// Current config-format version. Bump when the on-disk shape changes in a way
/// that needs migration.
pub const CONFIG_VERSION: u32 = 1;

/// The sonagram home directory: `$SONAGRAM_HOME` when set (tests point this at a
/// temp dir), else `~/.sonagram`. Errors only when neither `SONAGRAM_HOME` nor
/// `HOME` is resolvable.
pub fn sonagram_home() -> Result<PathBuf> {
    if let Ok(h) = std::env::var("SONAGRAM_HOME") {
        let h = h.trim();
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    let home = std::env::var("HOME").map_err(|_| {
        SonagramError::Config(
            "cannot resolve the sonagram home dir: neither SONAGRAM_HOME nor HOME is set".into(),
        )
    })?;
    Ok(PathBuf::from(home).join(".sonagram"))
}

/// Path of the config file (`$SONAGRAM_HOME/config.json`).
pub fn config_path() -> Result<PathBuf> {
    Ok(sonagram_home()?.join("config.json"))
}

/// The user config + source registry.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// On-disk format version ([`CONFIG_VERSION`]).
    pub config_version: u32,
    /// Registered source directories, absolute + canonicalized + sorted.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Explicit graph path. `None` → the default `$SONAGRAM_HOME/music.kgl`
    /// (see [`resolved_graph`](Self::resolved_graph)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    /// Explicit playlist-store dir. `None` → the default
    /// `$SONAGRAM_HOME/playlists/` (see
    /// [`resolved_playlists_dir`](Self::resolved_playlists_dir)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlists_dir: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            config_version: CONFIG_VERSION,
            sources: Vec::new(),
            graph: None,
            playlists_dir: None,
        }
    }
}

impl Config {
    /// A fresh, empty config at the current version.
    pub fn new() -> Self {
        Config::default()
    }

    /// Load the config from `$SONAGRAM_HOME/config.json`, or a fresh empty config
    /// when the file does not exist yet (first run).
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        Self::load_from(&path)
    }

    /// Load the config from an explicit path (or a fresh empty config when it is
    /// absent). Used by [`load`](Self::load) and directly by tests.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Config::new());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| SonagramError::Config(format!("read {}: {e}", path.display())))?;
        serde_json::from_str(&text)
            .map_err(|e| SonagramError::Config(format!("parse {}: {e}", path.display())))
    }

    /// Atomically save the config to `$SONAGRAM_HOME/config.json` (creating the
    /// home dir).
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        self.save_to(&path)
    }

    /// Atomically save to an explicit path (creating its parent). Write a temp
    /// sibling then rename, so a reader never sees a half-written config.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                SonagramError::Config(format!("create {}: {e}", parent.display()))
            })?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| SonagramError::Config(format!("serialize config: {e}")))?;
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| SonagramError::Config(format!("bad config path {}", path.display())))?;
        let dir = path
            .parent()
            .ok_or_else(|| SonagramError::Config(format!("no parent for {}", path.display())))?;
        let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
        std::fs::write(&tmp, json.as_bytes())
            .map_err(|e| SonagramError::Config(format!("write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| SonagramError::Config(format!("rename into {}: {e}", path.display())))?;
        Ok(())
    }

    /// The resolved graph path: the explicit `graph` when set, else the default
    /// `$SONAGRAM_HOME/music.kgl`.
    pub fn resolved_graph(&self) -> Result<PathBuf> {
        match &self.graph {
            Some(g) => Ok(PathBuf::from(g)),
            None => Ok(sonagram_home()?.join("music.kgl")),
        }
    }

    /// The resolved playlist-store dir: the explicit `playlists_dir` when set,
    /// else the default `$SONAGRAM_HOME/playlists/`.
    pub fn resolved_playlists_dir(&self) -> Result<PathBuf> {
        match &self.playlists_dir {
            Some(p) => Ok(PathBuf::from(p)),
            None => Ok(sonagram_home()?.join("playlists")),
        }
    }

    /// Register a source directory: canonicalize it (resolving symlinks so two
    /// spellings of the same dir dedupe), require that it exists and is a
    /// directory, and add it to the sorted set. Returns `(canonical, added)` —
    /// `added` is false when it was already registered.
    pub fn add_source(&mut self, dir: &Path) -> Result<(String, bool)> {
        let canon = std::fs::canonicalize(dir).map_err(|e| {
            SonagramError::Config(format!(
                "source directory {} does not exist or is unreadable: {e}",
                dir.display()
            ))
        })?;
        if !canon.is_dir() {
            return Err(SonagramError::Config(format!(
                "source path {} is not a directory",
                canon.display()
            )));
        }
        let s = canon.to_string_lossy().into_owned();
        if self.sources.iter().any(|x| x == &s) {
            return Ok((s, false));
        }
        self.sources.push(s.clone());
        self.sources.sort();
        Ok((s, true))
    }

    /// Unregister a source directory, matching either the canonicalized path (when
    /// the dir still exists) or the raw string as given. Returns whether an entry
    /// was removed.
    pub fn remove_source(&mut self, dir: &Path) -> bool {
        let raw = dir.to_string_lossy().into_owned();
        let canon = std::fs::canonicalize(dir)
            .ok()
            .map(|c| c.to_string_lossy().into_owned());
        let before = self.sources.len();
        self.sources
            .retain(|s| s != &raw && Some(s) != canon.as_ref());
        self.sources.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sonagram-cfg-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip_and_defaults() {
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tmp_home("roundtrip");
        let path = home.join("config.json");

        // A missing file loads as a fresh, empty, current-version config.
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.config_version, CONFIG_VERSION);
        assert!(cfg.sources.is_empty());
        assert!(cfg.graph.is_none());

        // Defaults resolve under the given home (via SONAGRAM_HOME).
        std::env::set_var("SONAGRAM_HOME", &home);
        assert_eq!(cfg.resolved_graph().unwrap(), home.join("music.kgl"));
        assert_eq!(
            cfg.resolved_playlists_dir().unwrap(),
            home.join("playlists")
        );

        // Save → reload is identical.
        let mut cfg = Config::new();
        cfg.graph = Some("/abs/g.kgl".to_string());
        cfg.playlists_dir = Some("/abs/pl".to_string());
        cfg.sources.push("/abs/a".to_string());
        cfg.save_to(&path).unwrap();
        let back = Config::load_from(&path).unwrap();
        assert_eq!(cfg, back);
        assert_eq!(back.resolved_graph().unwrap(), PathBuf::from("/abs/g.kgl"));

        std::env::remove_var("SONAGRAM_HOME");
    }

    #[test]
    fn add_dedup_sort_and_missing_dir_errors() {
        let home = tmp_home("sources");
        let a = home.join("a");
        let b = home.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let mut cfg = Config::new();
        // Add b then a → stored sorted (a before b).
        let (_, added_b) = cfg.add_source(&b).unwrap();
        let (_, added_a) = cfg.add_source(&a).unwrap();
        assert!(added_a && added_b);
        assert_eq!(cfg.sources.len(), 2);
        assert!(cfg.sources[0] < cfg.sources[1], "sorted");

        // Re-adding a is a no-op (dedup).
        let (_, added_again) = cfg.add_source(&a).unwrap();
        assert!(!added_again);
        assert_eq!(cfg.sources.len(), 2);

        // A non-existent dir is a Config error.
        let err = cfg.add_source(&home.join("nope")).unwrap_err();
        assert!(matches!(err, SonagramError::Config(_)), "got {err:?}");

        // Removing a canonicalized path works.
        assert!(cfg.remove_source(&a));
        assert_eq!(cfg.sources.len(), 1);
        assert!(!cfg.remove_source(&a), "second remove is a no-op");
    }
}
