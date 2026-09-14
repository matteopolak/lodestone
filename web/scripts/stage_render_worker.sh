#!/usr/bin/env bash
# Stage a stable worker entrypoint alongside Trunk's hashed Wasm output.
set -euo pipefail

out_dir="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
entry="$(find "$out_dir" -maxdepth 1 -type f -name 'lodestone-web-*.js' ! -name 'lodestone-web-entry.js' -print -quit)"
wasm="$(find "$out_dir" -maxdepth 1 -type f -name 'lodestone-web-*_bg.wasm' ! -name 'lodestone-web-entry_bg.wasm' -print -quit)"

if [ -z "$entry" ] || [ -z "$wasm" ]; then
  echo "render worker staging requires Trunk's lodestone-web JS and Wasm output" >&2
  exit 1
fi

cp "$entry" "$out_dir/lodestone-web-entry.js"
cp "$wasm" "$out_dir/lodestone-web-entry_bg.wasm"
cp "$root/web/render_worker.js" "$out_dir/lodestone-render-worker.js"
