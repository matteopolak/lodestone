#!/usr/bin/env bash
# Build the dedicated server Worker beside Trunk's page bundle.
set -euo pipefail

out_dir="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
worker_manifest="$root/web/worker/Cargo.toml"
worker_target="${LODESTONE_WORKER_TARGET_DIR:-$root/web/target/worker}"

export CARGO_PROFILE_RELEASE_CODEGEN_BACKEND=llvm
cargo build --manifest-path "$worker_manifest" --target wasm32-unknown-unknown --release \
  --target-dir "$worker_target"
wasm-bindgen --target web --out-dir "$out_dir" --out-name lodestone-server-worker-wasm \
  "$worker_target/wasm32-unknown-unknown/release/lodestone_server_worker.wasm"
cp "$root/web/worker/worker.js" "$out_dir/lodestone-server-worker.js"
