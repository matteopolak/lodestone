#!/usr/bin/env bash
# Build and stage the dedicated server Worker beside Trunk's page bundle.
set -euo pipefail

out_dir="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
worker_manifest="$root/web/worker/Cargo.toml"
worker_target="$(cargo metadata --manifest-path "$root/web/Cargo.toml" --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

wasm_target="wasm32-unknown-unknown"
wasm_binary="$worker_target/$wasm_target/release/lodestone_server_worker.wasm"

export CARGO_PROFILE_RELEASE_CODEGEN_BACKEND=llvm

# Build serial and shared-memory artifacts; atomics are module-level.
cargo build --manifest-path "$worker_manifest" --target "$wasm_target" --release
wasm-bindgen --target web --out-dir "$out_dir" --out-name lodestone-server-worker-wasm-serial \
  "$wasm_binary"

# The threaded build requires nightly and rust-src. Imported shared memory is
# passed to nested workers by wasm-bindgen-rayon.
RUSTFLAGS="-C target-feature=+atomics,+bulk-memory \
  -C link-arg=--shared-memory \
  -C link-arg=--max-memory=1073741824 \
  -C link-arg=--import-memory \
  -C link-arg=--export=__heap_base \
  -C link-arg=--export=__wasm_init_tls \
  -C link-arg=--export=__tls_size \
  -C link-arg=--export=__tls_align \
  -C link-arg=--export=__tls_base" \
  cargo build --manifest-path "$worker_manifest" --target "$wasm_target" --release \
  --features wasm-threads -Z build-std=panic_abort,std
wasm-bindgen --target web --out-dir "$out_dir" --out-name lodestone-server-worker-wasm-threaded \
  "$wasm_binary"

threaded_helper="$(find "$out_dir/snippets" -type f -name 'workerHelpers.no-bundler.js' -print -quit 2>/dev/null || true)"
if [ -z "$threaded_helper" ]; then
  echo "threaded worker glue did not stage its no-bundler helper" >&2
  exit 1
fi
if ! grep -Fq 'workerHelpers.no-bundler.js' "$out_dir"/lodestone-server-worker-wasm-threaded.js; then
  echo "threaded worker glue does not import its staged no-bundler helper" >&2
  exit 1
fi
node "$root/web/scripts/verify_threaded_worker.mjs" \
  "$out_dir/lodestone-server-worker-wasm-threaded.js" \
  "$out_dir/lodestone-server-worker-wasm-threaded_bg.wasm"

cp "$root/web/worker/worker.js" "$out_dir/lodestone-server-worker.js"
cp "$root/web/worker/worker_bootstrap.js" "$out_dir/lodestone-server-worker-bootstrap.js"
cp "$root/web/worker/worldgen_long_task_harness.js" "$out_dir/lodestone-worldgen-long-task-harness.js"
