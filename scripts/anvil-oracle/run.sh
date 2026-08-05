#!/usr/bin/env bash
# Compile & run the Anvil readback oracle against the real 26.2 server classes,
# in an ephemeral temurin:25-jdk container.
#
#   usage: run.sh <regionDir> <rx> <rz> <x>,<y>,<z> [<x>,<y>,<z> ...]
#
# Unlike `scripts/worldgen-oracle/run.sh`, this one mounts a **third** directory:
# the region directory holding a `.mca` file *Lodestone wrote*, which is the
# whole point — this oracle exists to have Mojang's own code read our bytes,
# not to have us read theirs. It is mounted read-only; the oracle never writes.
#
# Runtime: Apple `container` — see docs/oracle-runtimes.md.
set -euo pipefail
REGION_DIR="${1:?usage: run.sh <regionDir> <rx> <rz> <x>,<y>,<z> ...}"
shift
REGION_DIR="$(cd "$REGION_DIR" && pwd)"
CACHE="$(cd "$(dirname "$0")/../../.cache/mc/26.2" && pwd)"
HERE="$(cd "$(dirname "$0")" && pwd)"
container system start >/dev/null 2>&1 || true
container run --rm \
  --memory 3g \
  -e ORACLE_ARGS="/work/region $*" \
  -v "$CACHE":/mc:ro \
  -v "$HERE":/oracle \
  -v "$REGION_DIR":/region:ro \
  -w /work \
  eclipse-temurin:25-jdk \
  bash -c '
    set -e
    CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
    mkdir -p /work/region
    # `RegionFile` opens its file **read-write** (it may rewrite the header), so
    # the read-only host mount cannot be handed to it directly. Copying into the
    # container keeps the fixture on the host pristine, which matters: the Rust
    # gate compares against the bytes it wrote, not against whatever vanilla
    # might leave behind.
    cp -r /region/. /work/region/
    cp /oracle/AnvilReadbackOracle.java /work/
    javac -cp "$CP" -d /work /work/AnvilReadbackOracle.java
    java -cp "/work:$CP" AnvilReadbackOracle
  '
