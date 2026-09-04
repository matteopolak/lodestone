#!/usr/bin/env bash
#
# Build and execute the JNI invocation spikes in a reproducible Rust/JDK image.
# Each scenario starts the Rust executable as a fresh process because JNI
# permits one live JVM per process. `timeout` is part of the assertion: a
# dropped or silent PortServicer must report an exception, never hang.
# The `intercepted` binary additionally loads the existing real/shim pair around
# the already-compiled user-style Caller before registering the shim native.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "${HERE}" rev-parse --show-toplevel)"
IMAGE="lodestone-jni-invocation-spike:nightly-2026-08-07-temurin-25.0.3"

container system start >/dev/null 2>&1 || true
container build --tag "${IMAGE}" --file "${HERE}/Containerfile" "${HERE}"

container run --rm \
  --memory 4g \
  -v "${REPO_ROOT}":/repo:ro \
  -w /work \
  "${IMAGE}" \
  bash -c '
    set -euo pipefail
    export CARGO_HOME=/work/cargo-home
    export CARGO_TARGET_DIR=/work/target
    export CARGO_UNSTABLE_CODEGEN_BACKEND=true

    mkdir -p /work/classes /work/real /work/shim /work/app /work/harness
    javac -d /work/classes \
      /repo/crates/plugins/lodestone-jvm-bridge/spike/invocation/java/org/example/InvocationPlugin.java
    javac -d /work/real \
      /repo/crates/plugins/lodestone-jvm-bridge/spike/real/net/minecraft/world/level/Level.java
    javac -d /work/shim \
      /repo/crates/plugins/lodestone-jvm-bridge/spike/shim/net/minecraft/world/level/Level.java
    javac -cp /work/real -d /work/app \
      /repo/crates/plugins/lodestone-jvm-bridge/spike/app/org/example/Caller.java
    javac -d /work/harness \
      /repo/crates/plugins/lodestone-jvm-bridge/spike/invocation/java/org/example/BridgeLoader.java

    cargo test --locked --manifest-path \
      /repo/crates/plugins/lodestone-jvm-bridge/spike/invocation/Cargo.toml
    cargo build --locked --manifest-path \
      /repo/crates/plugins/lodestone-jvm-bridge/spike/invocation/Cargo.toml

    for scenario in success unregistered dropped timeout panic; do
      timeout 15s /work/target/debug/lodestone-jni-invocation-spike \
        "${scenario}" /work/classes
    done

    for scenario in success unregistered dropped timeout panic; do
      timeout 15s /work/target/debug/intercepted \
        "${scenario}" /work/harness /work/real /work/shim /work/app
    done

    echo "INVOCATION SPIKE PASSED"
  '
