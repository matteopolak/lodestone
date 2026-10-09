#!/usr/bin/env bash
# Compile & run a single-file worldgen JVM oracle against the real 26.2 server
# classes, in an ephemeral temurin:25-jdk container; prints the oracle's stdout.
# The oracles here call pure functions (RNG, noise, math tables, parameter
# lists, a template roof); none boots a server or reads a world.
#   usage: run.sh <OracleClassName> [args...]
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
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CACHE="${LODESTONE_MC_CACHE:-$REPO_ROOT/.cache/mc/26.2}"
if [ ! -d "$CACHE" ]; then
  CACHE="/Users/matthew/projects/lodestone/.cache/mc/26.2"
fi
if [ ! -d "$CACHE" ]; then
  echo "26.2 cache not found; set LODESTONE_MC_CACHE to a cache containing the server jar" >&2
  exit 1
fi
HERE="$(cd "$(dirname "$0")" && pwd)"
container system start >/dev/null 2>&1 || true
container run --rm --memory 3g \
  -e "ORACLE_ARGS=$ARGS" \
  -v "$CACHE:/mc:ro" \
  -v "$HERE:/oracle:ro" \
  -w /work \
  eclipse-temurin:25-jdk bash -c '
    set -e
    LIB_CP="$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
    CP="/mc/versions/26.2/server-26.2.jar:$LIB_CP"
    mkdir -p /work
    cp /oracle/'"$CLASS"'.java /work/
    javac -cp "$CP" -d /work /work/'"$CLASS"'.java
    java -cp "/work:$CP" '"$CLASS"'
  '
