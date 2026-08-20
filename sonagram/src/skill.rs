//! The embedded `sonagram-playlist` skill + the `sonagram skill show|install`
//! subcommands (P19 — cold-start bootstrap).
//!
//! The packaged asset (`sonagram/assets/sonagram-playlist.md`) is compiled INTO
//! the binary via `include_str!`; the repo-facing portable skill at
//! `skills/sonagram-playlist/SKILL.md` is byte-checked against it in tests,
//! so `pip install sonagram` ships the skill and `sonagram skill install` writes
//! it to `~/.claude/skills/` with no repo checkout. This is what carries a cold
//! prompt ("make me a work playlist") from a bare install to a delivered
//! playlist — the agent installs the package, installs the skill, then reads and
//! follows it in-session.
//!
//! Install personalizes the file: the two angle-bracket **personalization**
//! placeholders (`<YOUR_LIBRARY_ROOT>`, `<path to a built sonagram binary>`) are
//! substituted with real values from the user's config + the running executable.
//! The many command-syntax placeholders (`<graph.kgl>`, `<slug>`, `<dir>`, …) are
//! left untouched — only these two are personalization.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::{Result, SonagramError};

/// The portable `sonagram-playlist` skill, embedded at compile time. This is
/// exactly what `skill install` writes and `skill show` prints.
pub const SKILL_MD: &str = include_str!("../assets/sonagram-playlist.md");

/// The skill's install slug — the directory under a skills root, and the name an
/// agent recognizes it by.
pub const SKILL_SLUG: &str = "sonagram-playlist";

/// Personalization placeholder: the library root to register. Substituted at
/// install time with the first configured source, when one exists.
const PLACEHOLDER_LIBRARY_ROOT: &str = "<YOUR_LIBRARY_ROOT>";
/// Personalization placeholder: the built binary path. Substituted at install
/// time with the running executable's path.
const PLACEHOLDER_BINARY: &str = "<path to a built sonagram binary>";
/// Personalization placeholder: the Python interpreter for the skill's query
/// runner. A bare `python` is unsafe — shell aliases shadow it (observed in the
/// wild: `alias python='cd …'` broke the runner with a cryptic error). We
/// substitute the interpreter sitting next to the running console script when
/// one exists, else fall back to `python3`.
const PLACEHOLDER_PYTHON: &str = "<PYTHON>";

/// The result of an [`install`] — where the file landed and which personalization
/// substitutions were applied (for the CLI to report back).
#[derive(Debug, Clone)]
pub struct InstallReport {
    /// Absolute path of the written `SKILL.md`.
    pub path: PathBuf,
    /// The value `<YOUR_LIBRARY_ROOT>` was replaced with, if a source was configured.
    pub library_root: Option<String>,
    /// The value `<path to a built sonagram binary>` was replaced with, if the
    /// running executable path resolved.
    pub binary: Option<String>,
}

/// The default skills root, `~/.claude/skills`. Errors only when `HOME` is
/// unresolvable (pass `--dir <skills_root>` to override).
pub fn default_skills_root() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| {
        SonagramError::Config(
            "cannot resolve ~/.claude/skills: HOME is not set — pass --dir <skills_root>".into(),
        )
    })?;
    Ok(PathBuf::from(home).join(".claude").join("skills"))
}

/// Personalize the embedded skill for install: substitute the two personalization
/// placeholders with real values. Returns `(text, library_root, binary)` where
/// the latter two are the substituted values (or `None` when not applied).
fn personalize() -> (String, Option<String>, Option<String>) {
    let mut text = SKILL_MD.to_string();

    // Library root ← the first configured source, when the config exists and has one.
    let library_root = Config::load().ok().and_then(|c| c.sources.first().cloned());
    if let Some(root) = &library_root {
        text = text.replace(PLACEHOLDER_LIBRARY_ROOT, root);
    }

    // Binary path ← the running executable. Caveat: under the pip console
    // script, current_exe() is the *interpreter* (console scripts are Python
    // files) — in that case point at the sibling `sonagram` script instead.
    let binary = std::env::current_exe().ok().map(|exe| {
        let is_python = exe
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("python"));
        if is_python {
            if let Some(sib) = exe.parent().map(|d| d.join("sonagram")) {
                if sib.is_file() {
                    return sib.to_string_lossy().into_owned();
                }
            }
        }
        exe.to_string_lossy().into_owned()
    });
    if let Some(bin) = &binary {
        text = text.replace(PLACEHOLDER_BINARY, bin);
    }

    // Python interpreter ← the one next to the console script (a pip-installed
    // `sonagram` lives in <venv>/bin beside `python`), else `python3`. Never a
    // bare `python` — shell aliases shadow it.
    let python = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("python")))
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "python3".to_string());
    text = text.replace(PLACEHOLDER_PYTHON, &python);

    (text, library_root, binary)
}

/// Install the skill to `<skills_root>/sonagram-playlist/SKILL.md`, creating any
/// missing directories. Refuses to overwrite an existing file unless `force`.
///
/// `skills_root` defaults to [`default_skills_root`] (`~/.claude/skills`). The
/// written file is personalized from the user's config (see [`personalize`]).
pub fn install(skills_root: Option<&Path>, force: bool) -> Result<InstallReport> {
    let root = match skills_root {
        Some(p) => p.to_path_buf(),
        None => default_skills_root()?,
    };
    let dir = root.join(SKILL_SLUG);
    let file = dir.join("SKILL.md");

    if file.exists() && !force {
        return Err(SonagramError::Config(format!(
            "{} already exists — pass --force to overwrite",
            file.display()
        )));
    }

    std::fs::create_dir_all(&dir)
        .map_err(|e| SonagramError::Config(format!("create {}: {e}", dir.display())))?;
    let (text, library_root, binary) = personalize();
    std::fs::write(&file, text.as_bytes())
        .map_err(|e| SonagramError::Config(format!("write {}: {e}", file.display())))?;

    Ok(InstallReport {
        path: file,
        library_root,
        binary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sonagram-skill-{}-{name}-{}",
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
    fn embedded_skill_is_the_portable_copy() {
        // The embedded text is the real skill and names itself.
        assert!(!SKILL_MD.is_empty());
        assert!(SKILL_MD.contains("name: sonagram-playlist"));
        // The portable copy still carries the personalization placeholder (the
        // machine copy has a concrete path instead).
        assert!(SKILL_MD.contains(PLACEHOLDER_LIBRARY_ROOT));
        // The library-detection ladder (P19) is embedded.
        assert!(SKILL_MD.contains("Library detection"));
        let repo_copy =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../skills/sonagram-playlist/SKILL.md");
        if repo_copy.is_file() {
            assert_eq!(std::fs::read_to_string(repo_copy).unwrap(), SKILL_MD);
        }
    }

    #[test]
    fn install_writes_file_and_refuses_overwrite() {
        let _guard = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Hermetic home with NO config, so no library-root substitution happens.
        let home = tmp_dir("home-none");
        std::env::set_var("SONAGRAM_HOME", &home);
        let skills = tmp_dir("skills-none");

        let report = install(Some(&skills), false).unwrap();
        assert!(report.path.exists());
        assert_eq!(
            report.path,
            skills.join("sonagram-playlist").join("SKILL.md")
        );
        let body = std::fs::read_to_string(&report.path).unwrap();
        assert!(body.contains("name: sonagram-playlist"));
        // No config → the library placeholder is left intact.
        assert!(body.contains(PLACEHOLDER_LIBRARY_ROOT));
        assert!(report.library_root.is_none());

        // A second install without --force is refused.
        let err = install(Some(&skills), false).unwrap_err();
        assert!(matches!(err, SonagramError::Config(_)), "got {err:?}");

        // With --force it overwrites.
        assert!(install(Some(&skills), true).is_ok());

        std::env::remove_var("SONAGRAM_HOME");
    }

    #[test]
    fn install_substitutes_library_root_from_config() {
        let _guard = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tmp_dir("home-cfg");
        std::env::set_var("SONAGRAM_HOME", &home);
        // A real, existing source dir so add_source's canonicalize succeeds.
        let lib = tmp_dir("lib");
        let mut cfg = Config::new();
        cfg.add_source(&lib).unwrap();
        cfg.save().unwrap();

        let skills = tmp_dir("skills-cfg");
        let report = install(Some(&skills), false).unwrap();
        let body = std::fs::read_to_string(&report.path).unwrap();

        let expected = std::fs::canonicalize(&lib)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(report.library_root.as_deref(), Some(expected.as_str()));
        // The placeholder is gone, replaced by the real configured root.
        assert!(!body.contains(PLACEHOLDER_LIBRARY_ROOT));
        assert!(body.contains(&expected));

        std::env::remove_var("SONAGRAM_HOME");
    }
}
