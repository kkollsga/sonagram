#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_link="$repo_root/target"
link_target="$(readlink "$target_link" 2>/dev/null || true)"
target_dir="${CARGO_TARGET_DIR:-${link_target:-target}}"
case "$target_dir" in
  /*) ;;
  *) target_dir="$repo_root/$target_dir" ;;
esac

# A fresh checkout may not have created its target directory yet. Measure the
# nearest existing parent so the guard remains portable across Cargo setups.
probe="$target_dir"
while [ ! -e "$probe" ]; do
  parent="$(dirname "$probe")"
  [ "$parent" != "$probe" ] || break
  probe="$parent"
done

if [ -n "${FREE_GB:-}" ]; then
  free_kib=$((FREE_GB * 1024 * 1024))
else
  free_kib="$(df -Pk "$probe" 2>/dev/null | awk 'NR == 2 { print $4 }')"
fi
[ -n "$free_kib" ] || exit 0
fail_below_kib=$((15 * 1024 * 1024))
warn_below_kib=$((40 * 1024 * 1024))

if [ "$free_kib" -lt "$fail_below_kib" ]; then
  echo "build volume has less than 15 GiB free; run make prune-target before building" >&2
  exit 1
fi
if [ "$free_kib" -lt "$warn_below_kib" ]; then
  echo "warning: build volume has less than 40 GiB free; run make prune-target soon" >&2
fi
