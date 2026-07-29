#!/usr/bin/env bash
#
# verify_sdist.sh — assert the PR #9 contract on a built sonagram sdist.
#
#   usage: scripts/verify_sdist.sh dist/sonagram-<version>.tar.gz
#
# WHAT THIS GUARDS (and why "it builds" is not enough)
#
# sonagram declares kglite/kglite-mcp-server as *registry* dependencies in
# Cargo.toml and redirects them to the sibling ../KGLite checkout from
# .cargo/config.toml. That split is load-bearing: maturin vendors every `path`
# dependency into the sdist, but .cargo/config.toml is build configuration, not
# package content, so it is NOT packaged. A consumer therefore gets no
# [patch.crates-io] and resolves kglite from crates.io.
#
# Regressions that silently undo this — re-adding `path = "../KGLite/..."`,
# moving [patch.crates-io] into Cargo.toml, pinning a kglite that was never
# published — are invisible to every other CI context and surface only on a
# user's machine. Hence these four assertions:
#
#   1. no vendored kglite source in the tarball  (the stale-snapshot defect)
#   2. sonara IS vendored                        (not on crates.io; without it
#                                                 the sdist cannot build at all)
#   3. no .cargo/config.toml inside the tarball  (the mechanism itself)
#   4. `cargo metadata` resolves in the unpacked tree, standalone, AND kglite
#      resolves from the crates.io registry rather than a local path
#
# ─────────────────────────────────────────────────────────────────────────────
# THE FALSE-GREEN TRAP — DO NOT "SIMPLIFY" THE mktemp DANCE BELOW.
#
# Cargo discovers .cargo/config.toml by walking the CWD *upward*. Unpack the
# tarball anywhere under this repo (or under $GITHUB_WORKSPACE in CI) and cargo
# finds THIS repo's [patch.crates-io], redirects kglite straight back to the
# sibling ../KGLite checkout, and assertion 4 passes — while testing precisely
# the thing it exists to disprove. The unpack directory must therefore live
# outside the repo and outside the workspace, which is what the TMPDIR guard
# and the post-mktemp containment check below enforce. Assertion 4b (kglite's
# package id must carry a `registry+…crates.io-index` source) is the second
# line of defence: it fails loudly even if the unpack location ever slips.
# ─────────────────────────────────────────────────────────────────────────────
#
# LAYOUT NOTE. maturin has used two sdist layouts for vendored path deps:
# `local_dependencies/<crate>/` (older) and a common-ancestor rebase where each
# checkout lands at the tarball root (`sonagram-X.Y.Z/sonara/`, and — before
# PR #9 — `sonagram-X.Y.Z/KGLite/`, which is exactly how the published 0.2.1
# sdist carried its stale snapshot). Path-only checks pinned to one layout go
# dead when maturin changes. So the assertions below are layout-agnostic: they
# ask which crates the tarball actually vendors, by reading the `[package]
# name` of every Cargo.toml it contains.

set -euo pipefail

die() {
  echo "" >&2
  echo "FAIL: $*" >&2
  echo "" >&2
  exit 1
}

note() { echo "  $*"; }
ok() { echo "PASS: $*"; }

# ── Arguments ────────────────────────────────────────────────────────────────

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <path-to-sdist.tar.gz>" >&2
  exit 64
fi

SDIST_ARG="$1"
[ -f "$SDIST_ARG" ] || die "sdist not found: $SDIST_ARG"
SDIST="$(cd "$(dirname "$SDIST_ARG")" && pwd)/$(basename "$SDIST_ARG")"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "verify_sdist: $SDIST"
echo ""

# ── Scratch directory, outside the repo and outside the workspace ────────────

# is_under <candidate> <ancestor> — true when candidate is ancestor or below it.
is_under() {
  local cand="$1" anc="${2:-}"
  [ -n "$anc" ] || return 1
  case "$cand/" in
    "$anc"/*) return 0 ;;
    *) return 1 ;;
  esac
}

TMP_BASE="${TMPDIR:-/tmp}"
TMP_BASE="${TMP_BASE%/}"
if is_under "$TMP_BASE" "$REPO_ROOT" || is_under "$TMP_BASE" "${GITHUB_WORKSPACE:-}"; then
  echo "note: TMPDIR ($TMP_BASE) is inside the repo/workspace — see the" >&2
  echo "      false-green trap above. Falling back to /tmp." >&2
  TMP_BASE=/tmp
fi

# Explicit template on purpose: BSD (macOS) `mktemp -d` with no template
# ignores TMPDIR entirely and uses the per-user Darwin temp dir, while GNU
# mktemp honours it — a template makes the location deterministic on both, and
# makes the containment check below actually testable.
WORK="$(mktemp -d "$TMP_BASE/verify-sdist.XXXXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# Belt and braces: whatever mktemp handed back must not sit under the repo or
# the CI workspace, or assertion 4 is worthless.
if is_under "$WORK" "$REPO_ROOT" || is_under "$WORK" "${GITHUB_WORKSPACE:-}"; then
  die "scratch dir $WORK is inside the repo/workspace.
     Cargo would walk upward, find this repo's .cargo/config.toml
     [patch.crates-io], and resolve kglite from ../KGLite — making
     assertion 4 pass for exactly the wrong reason."
fi
note "scratch dir: $WORK"
echo ""

# ── Unpack + inventory ───────────────────────────────────────────────────────

ENTRIES="$WORK/entries.txt"
tar -tzf "$SDIST" > "$ENTRIES" || die "cannot list $SDIST — is it a valid tarball?"

tar -xzf "$SDIST" -C "$WORK"
TREE="$(find "$WORK" -mindepth 1 -maxdepth 1 -type d -not -name 'entries*' | head -1)"
[ -n "$TREE" ] && [ -d "$TREE" ] || die "sdist did not unpack to a single top-level directory"
note "unpacked: $(basename "$TREE") ($(grep -c . "$ENTRIES") entries)"
echo ""

# vendored_crate_names — the `[package] name` of every Cargo.toml in the sdist.
# Layout-agnostic by construction (see LAYOUT NOTE above).
vendored_crate_names() {
  local f
  while IFS= read -r f; do
    sed -n 's/^name[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$f" | head -1
  done < <(find "$TREE" -name Cargo.toml)
}
CRATES="$WORK/crates.txt"
vendored_crate_names | sort -u > "$CRATES"

# ── Assertion 1: no vendored kglite ──────────────────────────────────────────

if grep -qxE 'kglite|kglite-mcp-server' "$CRATES"; then
  die "assertion 1 — the sdist VENDORS kglite source.

     Found these kglite crates packaged inside the tarball:
$(grep -xE 'kglite|kglite-mcp-server' "$CRATES" | sed 's/^/       - /')
$(grep -iE '(^|/)(local_dependencies/)?kglite' "$ENTRIES" | head -5 | sed 's/^/       @ /')

     This is the defect PR #9 fixed. A vendored kglite means users compile a
     working-tree snapshot of whatever was in ../KGLite when the release ran —
     not a release anyone can ask for — so every engine fix since is invisible
     to an sdist install and the build is not reproducible.

     Cause is almost always a \`path = \"../KGLite/...\"\` back in Cargo.toml's
     [workspace.dependencies]. The local redirect belongs in .cargo/config.toml."
fi
ok "assertion 1 — no vendored kglite source in the tarball"

# ── Assertion 2: sonara IS vendored ──────────────────────────────────────────

if ! grep -qx 'sonara' "$CRATES"; then
  die "assertion 2 — sonara is NOT vendored in the sdist.

     sonara is a path dependency and is NOT published to crates.io, so
     vendoring it is the only thing that makes this sdist buildable. Without
     it, \`pip install sonagram\` from source fails with
     'failed to load manifest for dependency sonara'.

     This assertion is the counterweight to assertion 1: dropping every path
     dep would satisfy assertion 1 and ship a tarball nobody can build."
fi
SONARA_MANIFEST="$(grep -E '(^|/)sonara/Cargo\.toml$' "$ENTRIES" | head -1 || true)"
ok "assertion 2 — sonara is vendored (${SONARA_MANIFEST:-Cargo.toml present})"

# ── Assertion 3: no .cargo/config.toml packaged ──────────────────────────────

if grep -qE '(^|/)\.cargo/config(\.toml)?$' "$ENTRIES"; then
  die "assertion 3 — a .cargo/config.toml LEAKED into the tarball:
$(grep -E '(^|/)\.cargo/config(\.toml)?$' "$ENTRIES" | sed 's/^/       @ /')

     That file carries [patch.crates-io] redirects to ../KGLite. Shipping it
     means a consumer's build tries to resolve a sibling checkout they do not
     have and fails with 'unable to update ../KGLite'. Its absence from the
     package is the entire mechanism by which sdist installs resolve kglite
     from crates.io."
fi
ok "assertion 3 — no .cargo/config.toml inside the tarball"

# ── Assertion 4: cargo metadata resolves standalone, from the registry ───────

MANIFEST_REL="$(sed -n 's/^manifest-path[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$TREE/pyproject.toml" | head -1)"
if [ -z "$MANIFEST_REL" ] || [ ! -f "$TREE/$MANIFEST_REL" ]; then
  die "assertion 4 — cannot locate the crate manifest.
     [tool.maturin] manifest-path in the sdist's pyproject.toml is
     '${MANIFEST_REL:-<missing>}', which does not exist in the tarball.
     pip would fail the same way."
fi
note "manifest: $MANIFEST_REL"

META="$WORK/metadata.json"
META_ERR="$WORK/metadata.err"
# CWD is the unpacked tree (under /tmp) — NOT the repo. See the trap note.
set +e
(cd "$TREE" && cargo metadata --format-version 1 --manifest-path "$MANIFEST_REL") \
  > "$META" 2> "$META_ERR"
META_RC=$?
set -e

if [ "$META_RC" -ne 0 ]; then
  die "assertion 4 — \`cargo metadata\` FAILED (exit $META_RC) in the unpacked sdist.

     A consumer running \`pip install sonagram\` from source hits this exact
     failure. cargo said:

$(sed 's/^/       /' "$META_ERR" | head -30)"
fi

# 4b. kglite must have resolved from the crates.io registry. If it resolved to
#     a local path, either a patch leaked into the package or we unpacked
#     somewhere cargo could walk upward into this repo's .cargo/config.toml.
if ! grep -q 'registry+https://github.com/rust-lang/crates.io-index#kglite@' "$META"; then
  RESOLVED="$(grep -o '"id":"[^"]*kglite[^"]*"' "$META" | sort -u | sed 's/^/       /')"
  die "assertion 4b — kglite did NOT resolve from the crates.io registry.

     Resolved instead as:
${RESOLVED:-       (kglite absent from the dependency graph entirely)}

     Either a [patch.crates-io] shipped inside the package, or this check ran
     somewhere cargo could walk upward into a config.toml that redirects
     kglite to a local checkout — the false-green trap this script is built
     to avoid. Scratch dir was: $WORK"
fi
KGLITE_VER="$(grep -o 'crates.io-index#kglite@[0-9][^"]*' "$META" | head -1 | sed 's/.*@//')"
ok "assertion 4 — cargo metadata resolves standalone; kglite $KGLITE_VER from crates.io"

echo ""
echo "verify_sdist: all assertions passed for $(basename "$SDIST")"
