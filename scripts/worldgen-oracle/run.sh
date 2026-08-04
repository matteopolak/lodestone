#!/usr/bin/env bash
# Compile & run a worldgen JVM oracle against the real 26.2 server classes,
# in an ephemeral temurin:25-jdk container. Prints the oracle's stdout.
#   usage: run.sh <OracleClassName>
#
# Runtime: Apple `container` — see docs/oracle-runtimes.md. The `:ro`
# mount-suffix syntax this script depends on was unverified under `container`
# until this port; verified directly: `container run --rm -v <dir>:/mc:ro …
# touch /mc/x` reports "Read-only file system", same as Docker.
set -euo pipefail
CLASS="${1:?usage: run.sh <OracleClass> [args...]}"
shift || true
ARGS="$*"
export ORACLE_ARGS="$ARGS"
CACHE="$(cd "$(dirname "$0")/../../.cache/mc/26.2" && pwd)"
HERE="$(cd "$(dirname "$0")" && pwd)"
container system start >/dev/null 2>&1 || true
container run --rm \
  --memory 3g \
  -e ORACLE_ARGS="$ARGS" \
  -v "$CACHE":/mc:ro \
  -v "$HERE":/oracle \
  -w /work \
  eclipse-temurin:25-jdk \
  bash -c '
    set -e
    CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
    mkdir -p /work && cp /oracle/'"$CLASS"'.java /work/
    javac -cp "$CP" -d /work /work/'"$CLASS"'.java
    java -cp "/work:$CP" '"$CLASS"'
  '
