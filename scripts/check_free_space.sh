#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_link="$repo_root/target"
expected_target="/Volumes/EksternalHome/coding-cache/cargo-targets/sonagram"

if [ ! -L "$target_link" ] || [ "$(readlink "$target_link")" != "$expected_target" ]; then
  echo "target must be a symlink to $expected_target" >&2
  exit 1
fi

free_kib="$(df -Pk "$expected_target" | awk 'NR == 2 { print $4 }')"
fail_below_kib=$((15 * 1024 * 1024))
warn_below_kib=$((40 * 1024 * 1024))

if [ "$free_kib" -lt "$fail_below_kib" ]; then
  echo "build volume has less than 15 GiB free; run make prune-target before building" >&2
  exit 1
fi
if [ "$free_kib" -lt "$warn_below_kib" ]; then
  echo "warning: build volume has less than 40 GiB free; run make prune-target soon" >&2
fi
