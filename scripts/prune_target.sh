#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_link="$repo_root/target"
expected_target="/Volumes/EksternalHome/coding-cache/cargo-targets/sonagram"

if [ ! -L "$target_link" ]; then
  echo "target must be a symlink to $expected_target" >&2
  exit 1
fi

actual_target="$(readlink "$target_link")"
if [ "$actual_target" != "$expected_target" ]; then
  echo "refusing to prune unexpected target: $actual_target" >&2
  exit 1
fi

free_kib="$(df -Pk "$actual_target" | awk 'NR == 2 { print $4 }')"
size_kib="$(du -sk "$actual_target" | awk '{ print $1 }')"
clean_below_kib=$((40 * 1024 * 1024))
clean_above_kib=$((40 * 1024 * 1024))

write_cache_tag() {
  mkdir -p "$actual_target"
  cat > "$actual_target/CACHEDIR.TAG" <<'EOF'
Signature: 8a477f597d28d172789f06886806bc55
# This file is a cache directory tag created by cargo.
# For information about cache directory tags see https://bford.info/cachedir/
EOF
}

write_cache_tag

if [ "$free_kib" -lt "$clean_below_kib" ] || [ "$size_kib" -ge "$clean_above_kib" ]; then
  echo "pruning sonagram cargo cache (free=${free_kib} KiB, size=${size_kib} KiB)"
  cargo clean --target-dir "$actual_target"
  write_cache_tag
else
  echo "sonagram cargo cache within bounds (free=${free_kib} KiB, size=${size_kib} KiB)"
fi
