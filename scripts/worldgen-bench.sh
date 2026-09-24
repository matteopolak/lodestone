#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

target="$(rustc -vV | sed -n 's/^host: //p')"
rust_lld="$(rustc --print target-libdir)/../bin/rust-lld"
command=(cargo bench -p lodestone-worldgen --features gen-counters --bench generation)

if [[ -x "$rust_lld" ]]; then
  echo "worldgen benchmark linker: $rust_lld" >&2
  command+=(--config "target.${target}.linker=\"${rust_lld}\"")
else
  echo "worldgen benchmark linker: Apple/system linker; disabling LTO for diagnostics" >&2
  command+=(--config 'profile.bench.lto=false')
fi

"${command[@]}" "$@"
