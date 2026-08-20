//! Mechanical gate: every statement of the embedded KGLite version agrees with
//! the pin.
//!
//! Sonagram names the kglite version it embeds in eleven places — two
//! manifests, a rationale comment, a compiled-in error string, five prose
//! files, and the two CI workflows that check the engine out at a release tag.
//! Nothing but a human eye had ever checked them against each other, and two of
//! them had silently drifted a whole minor version behind the pin. The workflow
//! pins were added to that list after the 0.16.3 -> 0.16.5 bump, where all four
//! of them drifted while every site this test already covered moved correctly —
//! a gate that covers most of the sites still lets the bump ship broken. This test
//! derives the expected version from ONE source (the `kglite` entry in the
//! workspace `Cargo.toml`) and asserts every other site agrees, so a future bump
//! edits one pin and a red test enumerates the rest.
//!
//! Two properties this test must have, both of which are easy to get wrong:
//!
//! 1. **No substring subsumption.** `contents.contains("KGLite 0.15.1")` is also
//!    satisfied by `KGLite 0.15.10`. Every check here extracts the version
//!    *token* that follows a marker and compares tokens for equality.
//! 2. **No vacuous pass.** A file that cannot be read, or that contains zero
//!    version mentions where one is required, fails loudly with its path. A gate
//!    that skips is worse than no gate.

use std::path::{Path, PathBuf};

/// Repo root — `CARGO_MANIFEST_DIR` is `<repo>/sonagram`, so the root is its
/// parent. Never derived from the process CWD.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sonagram/ must have a parent (the repo root)")
        .to_path_buf()
}

/// Read a repo-relative file, panicking with the full path when it is missing.
/// This is the "no vacuous pass" guarantee: an asserted site that vanished (or
/// was renamed) turns the gate red instead of quietly dropping an assertion.
fn read_repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "version_consistency: cannot read asserted file {} ({e}). \
             If it moved, update this test's site list — do not delete the check.",
            path.display()
        )
    })
}

/// Is `c` part of a semver-ish token (`0.15.1`)?
fn is_version_char(c: char) -> bool {
    c.is_ascii_digit() || c == '.'
}

/// Every version token that immediately follows `marker` in `hay`.
///
/// The token runs to the first character that is neither a digit nor a dot, and
/// a trailing `.` (sentence punctuation, as in "we embed KGLite 0.15.1.") is
/// trimmed. Because the token ends at a real boundary, `0.15.10` and `0.15.1`
/// are *different* tokens — the subsumption trap in requirement 1 above.
fn versions_after(hay: &str, marker: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = hay;
    while let Some(idx) = rest.find(marker) {
        let tail = &rest[idx + marker.len()..];
        let end = tail
            .find(|c: char| !is_version_char(c))
            .unwrap_or(tail.len());
        let token = tail[..end].trim_end_matches('.');
        if !token.is_empty() && token.contains('.') {
            out.push(token.to_string());
        }
        rest = &rest[idx + marker.len()..];
    }
    out
}

/// The single source of truth: the `kglite` dependency's `version` in the
/// workspace `Cargo.toml`.
fn pinned_kglite_version(manifest: &str) -> String {
    dep_version(manifest, "kglite").unwrap_or_else(|| {
        panic!(
            "version_consistency: no `kglite = {{ version = \"…\" }}` entry found in \
             Cargo.toml [workspace.dependencies]. This test derives everything from \
             that pin; without it there is nothing to check."
        )
    })
}

/// Extract `version = "X"` from the `<name> = { … }` dependency line of a
/// manifest. Deliberately small: these entries are single-line inline tables,
/// and the crate must not grow a toml parser dependency for a test.
fn dep_version(manifest: &str, name: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some(rhs) = line.strip_prefix(name) else {
            continue;
        };
        let rhs = rhs.trim_start();
        let Some(rhs) = rhs.strip_prefix('=') else {
            continue; // `kglite-mcp-server` when we asked for `kglite`, etc.
        };
        let rhs = rhs.trim_start();
        if !rhs.starts_with('{') {
            continue;
        }
        let vi = rhs.find("version")?;
        let after = &rhs[vi..];
        let q = after.find('"')?;
        let after = &after[q + 1..];
        let end = after.find('"')?;
        return Some(after[..end].to_string());
    }
    None
}

/// Prose files that must name the embedded engine as `KGLite <version>`.
const PROSE_SITES: &[&str] = &[
    "README.md",
    "AGENT-GUIDE.md",
    "docs/agent-guide.md",
    "docs/cli.md",
    "docs/index.md",
];


/// Workflow files that check out the kglite sibling at a release tag.
///
/// These pin the engine CI actually builds against. They drifted silently
/// through the 0.16.3 -> 0.16.5 bump — the manifest said one thing and four
/// workflow steps said another — which is exactly the failure this test exists
/// to prevent, so they are asserted like every other site.
const WORKFLOW_SITES: &[&str] = &[".github/workflows/ci.yml", ".github/workflows/release.yml"];

/// Every `ref: v<version>` that belongs to a checkout step for `repository`.
///
/// Scoped deliberately: these workflows check out BOTH upstreams, each with its
/// own `ref:`, so a blanket scan for `ref: v` would compare one engine's
/// version against the other's pin and fail for the wrong reason. The `ref:` is
/// matched only while inside a step whose `repository:` named the wanted repo.
fn sibling_refs_in_workflow(yaml: &str, repository: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_wanted_step = false;
    for (i, line) in yaml.lines().enumerate() {
        let t = line.trim();
        if let Some(repo) = t.strip_prefix("repository:") {
            // A new `repository:` key always ends the previous step's scope.
            in_wanted_step = repo.trim() == repository;
            continue;
        }
        if !in_wanted_step {
            continue;
        }
        if let Some(r) = t.strip_prefix("ref:") {
            let r = r.trim().trim_start_matches('v');
            let end = r.find(|c: char| !is_version_char(c)).unwrap_or(r.len());
            let token = r[..end].trim_end_matches('.');
            if !token.is_empty() && token.contains('.') {
                out.push((i + 1, token.to_string()));
            }
        }
    }
    out
}

#[test]
fn kglite_version_is_stated_consistently_everywhere() {
    let manifest = read_repo_file("Cargo.toml");
    let expected = pinned_kglite_version(&manifest);

    // Collect every failure before reporting, so a bump gets one actionable red
    // list rather than a whack-a-mole of single assertion failures.
    let mut failures: Vec<String> = Vec::new();

    // --- Cargo.toml: the sibling server crate must move in lockstep. ---
    match dep_version(&manifest, "kglite-mcp-server") {
        Some(v) if v == expected => {}
        Some(v) => failures.push(format!(
            "Cargo.toml: `kglite-mcp-server` is pinned to {v}, but `kglite` is pinned \
             to {expected}. The two crates ship from one KGLite release and must match."
        )),
        None => failures.push(
            "Cargo.toml: no `kglite-mcp-server = { version = \"…\" }` entry found.".to_string(),
        ),
    }

    // --- Cargo.toml: the rationale comment above the pin. ---
    // Any comment line that names a kglite version is making a claim about what
    // we embed; all of them must state the pinned version.
    let mut comment_versions = Vec::new();
    for (i, line) in manifest.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        for v in versions_after(trimmed, "kglite ") {
            comment_versions.push((i + 1, v));
        }
    }
    if comment_versions.is_empty() {
        failures.push(
            "Cargo.toml: the rationale comment above the upstream pins no longer names a \
             kglite version. It documents what the pin is satisfiable by — restore it."
                .to_string(),
        );
    }
    for (line_no, v) in &comment_versions {
        if v != &expected {
            failures.push(format!(
                "Cargo.toml:{line_no}: rationale comment says kglite {v}, pin says {expected}."
            ));
        }
    }

    // --- pyproject.toml: the Python runtime dependency floor. ---
    let pyproject = read_repo_file("pyproject.toml");
    let floors = versions_after(&pyproject, "kglite>=");
    if floors.is_empty() {
        failures.push(
            "pyproject.toml: no `kglite>=<version>` runtime dependency found. The wheel \
             needs the kglite wheel at run time; that floor must exist."
                .to_string(),
        );
    }
    for v in &floors {
        if v != &expected {
            failures.push(format!(
                "pyproject.toml: runtime dependency floor is kglite>={v}, cargo pin is \
                 {expected}. The floor moves in lockstep with the pin."
            ));
        }
    }

    // --- sonagram-python/src/lib.rs: the compiled-in "install kglite" hint. ---
    // A user who follows a stale hint installs a version our own metadata
    // forbids, so this string is as load-bearing as the manifest floor.
    let lib_rs = read_repo_file("sonagram-python/src/lib.rs");
    let hints = versions_after(&lib_rs, "kglite>=");
    if hints.is_empty() {
        failures.push(
            "sonagram-python/src/lib.rs: no `pip install kglite>=<version>` hint found. \
             The kglite-import failure path must tell the user which version to install."
                .to_string(),
        );
    }
    for v in &hints {
        if v != &expected {
            failures.push(format!(
                "sonagram-python/src/lib.rs: error string tells users `pip install \
                 kglite>={v}`, but the pin (and pyproject floor) is {expected}. \
                 Following that hint installs a version our own metadata forbids."
            ));
        }
    }

    // --- Prose: every doc that names the embedded engine version. ---
    for site in PROSE_SITES {
        let contents = read_repo_file(site);
        let mentions = versions_after(&contents, "KGLite ");
        if mentions.is_empty() {
            failures.push(format!(
                "{site}: expected at least one `KGLite {expected}` mention, found no \
                 versioned KGLite mention at all. If the sentence moved, point this \
                 test at its new home — do not drop the site."
            ));
        }
        for v in &mentions {
            if v != &expected {
                failures.push(format!(
                    "{site}: says `KGLite {v}`, pin says `KGLite {expected}`."
                ));
            }
        }
    }

    // --- CI/release workflows: the tags CI builds BOTH engines from. ---
    // sonara is asserted here too. It is not otherwise covered by this test, and
    // its pins sit in the very same four steps that drifted for kglite, so
    // leaving it out would reproduce the identical hole one bump later.
    let sonara_expected = dep_version(&manifest, "sonara").unwrap_or_else(|| {
        panic!(
            "version_consistency: no `sonara = {{ version = \"…\" }}` entry found in \
             Cargo.toml [workspace.dependencies]. The workflow tag pins are checked \
             against it; without it there is nothing to check."
        )
    });
    for site in WORKFLOW_SITES {
        let contents = read_repo_file(site);
        for (engine, repository, want) in [
            ("kglite", "kkollsga/kglite", &expected),
            ("sonara", "kkollsga/sonara", &sonara_expected),
        ] {
            let refs = sibling_refs_in_workflow(&contents, repository);
            if refs.is_empty() {
                failures.push(format!(
                    "{site}: no `{repository}` checkout with a `ref: v<version>` found. CI \
                     builds {engine} from a pinned tag; if that step moved, point this test \
                     at its new home — do not drop the site."
                ));
            }
            for (line_no, v) in &refs {
                if &v != &want {
                    failures.push(format!(
                        "{site}:{line_no}: checks {engine} out at tag v{v}, pin says {want}. \
                         CI would build against a different engine than the manifest declares."
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "embedded KGLite version is stated inconsistently \
         (pin = {expected}, from Cargo.toml [workspace.dependencies].kglite):\n  - {}\n",
        failures.join("\n  - ")
    );
}

/// The subsumption guard itself, proven rather than asserted: `0.15.10` must not
/// satisfy an expectation of `0.15.1`. A naive `contains()` check passes this
/// input, which is exactly why the real test compares extracted tokens.
#[test]
fn version_extraction_does_not_subsume_longer_versions() {
    assert_eq!(
        versions_after("embeds KGLite 0.15.1's server", "KGLite "),
        vec!["0.15.1"]
    );
    assert_eq!(
        versions_after("embeds KGLite 0.15.10 today", "KGLite "),
        vec!["0.15.10"]
    );
    assert_eq!(versions_after("(KGLite 0.15.1)", "KGLite "), vec!["0.15.1"]);
    assert_eq!(
        versions_after("we embed KGLite 0.15.1.", "KGLite "),
        vec!["0.15.1"]
    );
    assert_eq!(
        versions_after("`pip install kglite>=0.15.1`.", "kglite>="),
        vec!["0.15.1"]
    );
    assert_eq!(
        versions_after("\"kglite>=0.15.1\",", "kglite>="),
        vec!["0.15.1"]
    );
    assert_eq!(
        versions_after("KGLite 0.15.1 then KGLite 0.14.5", "KGLite "),
        vec!["0.15.1", "0.14.5"]
    );
    // Bare "KGLite" with no version is not a version claim.
    assert!(versions_after("against KGLite's live graph", "KGLite ").is_empty());
    // The trap: a `0.15.10` mention must NOT be accepted where 0.15.1 is wanted.
    let found = versions_after("KGLite 0.15.10", "KGLite ");
    assert_ne!(found, vec!["0.15.1".to_string()]);
    assert!("KGLite 0.15.10".contains("KGLite 0.15.1")); // ...which contains() would have.
}

#[test]
fn dep_version_reads_inline_tables() {
    let manifest = "\
[workspace.dependencies]\n\
# kglite 9.9.9 (a comment, not a pin)\n\
kglite = { version = \"0.15.1\", default-features = false }\n\
kglite-mcp-server = { version = \"0.15.2\", default-features = false }\n";
    assert_eq!(dep_version(manifest, "kglite").as_deref(), Some("0.15.1"));
    assert_eq!(
        dep_version(manifest, "kglite-mcp-server").as_deref(),
        Some("0.15.2")
    );
    assert_eq!(dep_version(manifest, "nonexistent"), None);
}

/// The workflow extractor must read only the named repository's `ref:`. These
/// workflows check out both upstreams, and each engine's `ref:` sitting a few
/// lines from the other's is exactly the value a blanket `ref:` scan would
/// wrongly compare against the other engine's pin.
#[test]
fn workflow_refs_are_scoped_to_the_named_repository() {
    let yaml = "\
      - name: Check out sonara\n\
        with:\n\
          repository: kkollsga/sonara\n\
          ref: v0.3.6\n\
      - name: Check out kglite\n\
        with:\n\
          repository: kkollsga/kglite\n\
          ref: v0.16.5\n\
          path: _siblings/KGLite\n";
    let refs = |repo| {
        sibling_refs_in_workflow(yaml, repo)
            .into_iter()
            .map(|(_, v)| v)
            .collect::<Vec<_>>()
    };
    assert_eq!(refs("kkollsga/kglite"), vec!["0.16.5"]);
    assert_eq!(refs("kkollsga/sonara"), vec!["0.3.6"]);
    // A repository that is not checked out at all is a missing site, not a
    // silent pass — the caller turns an empty result into a failure.
    assert!(refs("kkollsga/absent").is_empty());
}
