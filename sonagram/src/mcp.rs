//! Installable kglite-native manifest and revealed music-domain skills.
//!
//! The assets are embedded so both Cargo and PyPI installs can materialize a
//! deployment next to the configured `.kgl` graph without a repository checkout.
//! Kglite owns the server and generic graph tools; this module only installs
//! Sonagram's declarative manifest and domain methodology.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::{Result, SonagramError};

pub const MANIFEST_YAML: &str = include_str!("../assets/music_mcp.yaml");
pub const CURATION_CONTRACT_MARKER: &str = "sonagram-curation-contract:v1";

pub fn launch_label() -> &'static str {
    if cfg!(windows) {
        "RUN (PowerShell)"
    } else {
        "RUN"
    }
}

pub const SKILL_ASSETS: &[(&str, &str)] = &[
    (
        "music_library_profile.md",
        include_str!("../assets/music_mcp.skills/music_library_profile.md"),
    ),
    (
        "music_curation_policy.md",
        include_str!("../assets/music_mcp.skills/music_curation_policy.md"),
    ),
    (
        "music_playlist_audit.md",
        include_str!("../assets/music_mcp.skills/music_playlist_audit.md"),
    ),
    (
        "music_playlist_store.md",
        include_str!("../assets/music_mcp.skills/music_playlist_store.md"),
    ),
    (
        "music_song_versions.md",
        include_str!("../assets/music_mcp.skills/music_song_versions.md"),
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub graph_path: PathBuf,
    pub manifest_path: PathBuf,
    pub skills_dir: PathBuf,
    pub public_source_dir: PathBuf,
    pub server_binary: Option<PathBuf>,
    pub written: usize,
    pub unchanged: usize,
}

impl InstallReport {
    pub fn launch_command(&self) -> Option<String> {
        self.server_binary.as_ref().map(|binary| launch_command(binary, &self.graph_path))
    }
}

/// Install beside the configured graph. Identical existing assets are a clean
/// no-op; differing files require `force`, so local operator edits are never
/// overwritten silently.
pub fn install(force: bool) -> Result<InstallReport> {
    let graph_path = Config::load()?.resolved_graph()?;
    install_for_graph(&graph_path, force)
}

fn install_for_graph(graph_path: &Path, force: bool) -> Result<InstallReport> {
    if !graph_path.is_file() {
        return Err(SonagramError::Config(format!(
            "configured graph {} does not exist — run `sonagram build` first",
            graph_path.display()
        )));
    }
    let graph_path = std::fs::canonicalize(graph_path).map_err(|error| {
        SonagramError::Config(format!(
            "canonicalize configured graph {}: {error}",
            graph_path.display()
        ))
    })?;
    let parent = graph_path.parent().ok_or_else(|| {
        SonagramError::Config(format!("configured graph {} has no parent", graph_path.display()))
    })?;
    let stem = graph_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            SonagramError::Config(format!(
                "configured graph {} has no UTF-8 file stem",
                graph_path.display()
            ))
        })?;
    let manifest_path = parent.join(format!("{stem}_mcp.yaml"));
    let skills_dir = parent.join(format!("{stem}_mcp.skills"));
    let public_source_dir = parent.join(".sonagram-mcp-public");
    validate_public_source_dir(&public_source_dir)?;
    reject_symlink(&manifest_path)?;
    reject_symlink(&skills_dir)?;
    let mut assets = vec![(manifest_path.clone(), MANIFEST_YAML.as_bytes())];
    assets.extend(
        SKILL_ASSETS
            .iter()
            .map(|(name, body)| (skills_dir.join(name), body.as_bytes())),
    );

    for (path, expected) in &assets {
        reject_symlink(path)?;
        if path.exists() {
            let actual = std::fs::read(path).map_err(|error| {
                SonagramError::Config(format!("read {}: {error}", path.display()))
            })?;
            if actual != *expected && !force {
                return Err(SonagramError::Config(format!(
                    "{} differs from Sonagram's bundled MCP asset — pass --force to replace it",
                    path.display()
                )));
            }
        }
    }

    std::fs::create_dir_all(&skills_dir).map_err(|error| {
        SonagramError::Config(format!("create {}: {error}", skills_dir.display()))
    })?;
    std::fs::create_dir_all(&public_source_dir).map_err(|error| {
        SonagramError::Config(format!("create {}: {error}", public_source_dir.display()))
    })?;
    let mut unchanged = 0;
    let mut changed = Vec::new();
    for (path, expected) in &assets {
        if path.exists() && std::fs::read(path).ok().as_deref() == Some(*expected) {
            unchanged += 1;
            continue;
        }
        changed.push((path.clone(), *expected));
    }
    let written = changed.len();
    write_transaction(&changed)?;
    Ok(InstallReport {
        graph_path,
        manifest_path,
        skills_dir,
        public_source_dir,
        server_binary: resolve_server_binary(),
        written,
        unchanged,
    })
}

fn reject_symlink(path: &Path) -> Result<()> {
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(SonagramError::Config(format!(
            "refusing symlinked MCP asset path {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_public_source_dir(path: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SonagramError::Config(format!(
            "MCP source sandbox {} must be a real directory, not a file or symlink",
            path.display()
        )));
    }
    let mut entries = std::fs::read_dir(path).map_err(|error| {
        SonagramError::Config(format!("read MCP source sandbox {}: {error}", path.display()))
    })?;
    if entries.next().is_some() {
        return Err(SonagramError::Config(format!(
            "MCP source sandbox {} is not empty; refusing to expose operator files",
            path.display()
        )));
    }
    Ok(())
}

fn resolve_server_binary() -> Option<PathBuf> {
    let executable = format!("kglite-mcp-server{}", std::env::consts::EXE_SUFFIX);
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(executable)));
    sibling
        .filter(|path| is_executable_file(path))
        .and_then(|path| std::fs::canonicalize(path).ok())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn write_transaction(assets: &[(PathBuf, &[u8])]) -> Result<()> {
    if assets.is_empty() {
        return Ok(());
    }
    let suffix = format!(
        "{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut staged = Vec::with_capacity(assets.len());
    for (path, bytes) in assets {
        let parent = path.parent().ok_or_else(|| {
            SonagramError::Config(format!("MCP asset {} has no parent", path.display()))
        })?;
        let file_name = path.file_name().and_then(|value| value.to_str()).ok_or_else(|| {
            SonagramError::Config(format!("bad MCP asset path {}", path.display()))
        })?;
        let temp = parent.join(format!(".{file_name}.tmp.{suffix}"));
        if let Err(error) = std::fs::write(&temp, bytes) {
            for (_, staged_temp, _) in &staged {
                let _ = std::fs::remove_file(staged_temp);
            }
            return Err(SonagramError::Config(format!(
                "stage MCP asset {}: {error}",
                path.display()
            )));
        }
        let backup = parent.join(format!(".{file_name}.backup.{suffix}"));
        staged.push((path.clone(), temp, backup));
    }

    let mut committed: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for (index, (path, temp, backup)) in staged.iter().enumerate() {
        let prior = if path.exists() {
            if let Err(error) = std::fs::rename(path, backup) {
                rollback_assets(&committed);
                cleanup_temps(&staged[index..]);
                return Err(SonagramError::Config(format!(
                    "stage existing {}: {error}",
                    path.display()
                )));
            }
            Some(backup.clone())
        } else {
            None
        };
        if let Err(error) = std::fs::rename(temp, path) {
            if let Some(backup) = &prior {
                let _ = std::fs::rename(backup, path);
            }
            rollback_assets(&committed);
            cleanup_temps(&staged[index + 1..]);
            return Err(SonagramError::Config(format!(
                "commit MCP asset {}: {error}; prior assets restored",
                path.display()
            )));
        }
        committed.push((path.clone(), prior));
    }
    for (_, backup) in committed {
        if let Some(backup) = backup {
            std::fs::remove_file(&backup).map_err(|error| {
                SonagramError::Config(format!("remove staged {}: {error}", backup.display()))
            })?;
        }
    }
    Ok(())
}

fn rollback_assets(committed: &[(PathBuf, Option<PathBuf>)]) {
    for (path, backup) in committed.iter().rev() {
        let _ = std::fs::remove_file(path);
        if let Some(backup) = backup {
            let _ = std::fs::rename(backup, path);
        }
    }
}

fn cleanup_temps(staged: &[(PathBuf, PathBuf, PathBuf)]) {
    for (_, temp, _) in staged {
        let _ = std::fs::remove_file(temp);
    }
}

#[cfg(unix)]
fn command_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn command_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('`', "``").replace('"', "`\""))
}

#[cfg(unix)]
fn launch_command(binary: &Path, graph: &Path) -> String {
    format!(
        "{} --graph {}",
        command_quote(&binary.to_string_lossy()),
        command_quote(&graph.to_string_lossy())
    )
}

#[cfg(windows)]
fn launch_command(binary: &Path, graph: &Path) -> String {
    format!(
        "& {} --graph {}",
        command_quote(&binary.to_string_lossy()),
        command_quote(&graph.to_string_lossy())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempGraph(PathBuf);

    impl std::ops::Deref for TempGraph {
        type Target = Path;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl Drop for TempGraph {
        fn drop(&mut self) {
            if let Some(parent) = self.0.parent() {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }

    fn temp_graph(tag: &str) -> TempGraph {
        let root = std::env::temp_dir().join(format!(
            "sonagram-mcp-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let graph = root.join("work music.kgl");
        std::fs::write(&graph, b"fixture").unwrap();
        TempGraph(graph)
    }

    #[test]
    fn bundled_assets_carry_native_skill_gates_and_contract() {
        assert!(MANIFEST_YAML.contains("skills: true"));
        assert!(MANIFEST_YAML.contains("name: music_library_profile"));
        for (name, body) in SKILL_ASSETS {
            assert!(body.contains("references_tools:"), "{name}");
            assert!(body.contains("applies_when:"), "{name}");
            assert!(body.len() < 4096, "{name} exceeds the 4 KB soft target");
        }
        assert!(SKILL_ASSETS
            .iter()
            .all(|(_, body)| body.contains(CURATION_CONTRACT_MARKER)));
        assert!(crate::skill::SKILL_MD.contains(CURATION_CONTRACT_MARKER));
    }

    #[test]
    fn install_is_idempotent_and_preserves_operator_edits() {
        let graph = temp_graph("idempotent");
        let first = install_for_graph(&graph, false).unwrap();
        assert_eq!(first.written, 1 + SKILL_ASSETS.len());
        assert_eq!(first.unchanged, 0);
        assert!(first.manifest_path.exists());
        assert!(first.skills_dir.join("music_curation_policy.md").exists());
        assert!(first.public_source_dir.is_dir());
        assert_eq!(std::fs::read_dir(&first.public_source_dir).unwrap().count(), 0);

        let second = install_for_graph(&graph, false).unwrap();
        assert_eq!(second.written, 0);
        assert_eq!(second.unchanged, 1 + SKILL_ASSETS.len());

        std::fs::write(&first.manifest_path, b"operator edit").unwrap();
        assert!(install_for_graph(&graph, false).is_err());
        assert_eq!(std::fs::read(&first.manifest_path).unwrap(), b"operator edit");
        let forced = install_for_graph(&graph, true).unwrap();
        assert_eq!(forced.written, 1);
        assert_eq!(std::fs::read_to_string(&first.manifest_path).unwrap(), MANIFEST_YAML);
    }

    #[cfg(unix)]
    #[test]
    fn install_rejects_symlinked_public_source_sandbox() {
        use std::os::unix::fs::symlink;

        let graph = temp_graph("sandbox-symlink");
        let parent = graph.parent().unwrap();
        let exposed = parent.join("private");
        std::fs::create_dir_all(&exposed).unwrap();
        std::fs::write(exposed.join(".env"), b"SECRET=value").unwrap();
        symlink(&exposed, parent.join(".sonagram-mcp-public")).unwrap();
        assert!(install_for_graph(&graph, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn install_rejects_symlinked_skill_directory() {
        use std::os::unix::fs::symlink;

        let graph = temp_graph("skill-symlink");
        let parent = graph.parent().unwrap();
        let exposed = parent.join("private-skills");
        std::fs::create_dir_all(&exposed).unwrap();
        symlink(&exposed, parent.join("work music_mcp.skills")).unwrap();
        assert!(install_for_graph(&graph, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_file_is_not_a_server_binary() {
        use std::os::unix::fs::PermissionsExt;

        let graph = temp_graph("non-executable");
        let fake = graph.parent().unwrap().join("kglite-mcp-server");
        std::fs::write(&fake, b"not executable").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable_file(&fake));
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable_file(&fake));
    }
}
