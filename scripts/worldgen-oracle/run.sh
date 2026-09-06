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
WORLD_MOUNTS=()
WORLD_ENV=()
if [ -n "${LODESTONE_ORACLE_WORLD_ROOT:-}" ]; then
  if [ ! -d "$LODESTONE_ORACLE_WORLD_ROOT" ]; then
    echo "LODESTONE_ORACLE_WORLD_ROOT must name an existing writable directory" >&2
    exit 2
  fi
  WORLD_MOUNTS+=( -v "$LODESTONE_ORACLE_WORLD_ROOT:/world" )
  WORLD_ENV+=( -e ORACLE_WORLD_ROOT=/world )
fi
if [ -n "${LODESTONE_ORACLE_FROZEN_WORLD_ROOT:-}" ]; then
  if [ ! -d "$LODESTONE_ORACLE_FROZEN_WORLD_ROOT" ]; then
    echo "LODESTONE_ORACLE_FROZEN_WORLD_ROOT must name an existing frozen-world directory" >&2
    exit 2
  fi
  WORLD_MOUNTS+=( -v "$LODESTONE_ORACLE_FROZEN_WORLD_ROOT:/frozen:ro" )
  WORLD_ENV+=( -e ORACLE_FROZEN_WORLD_ROOT=/frozen )
fi
CONTAINER_ARGS=(
  --rm
  --memory 3g
  -e "ORACLE_ARGS=$ARGS"
  -v "$CACHE:/mc:ro"
  -v "$HERE:/oracle"
  -w /work
)
set +u
CONTAINER_ARGS+=( "${WORLD_ENV[@]}" "${WORLD_MOUNTS[@]}" )
set -u
container run "${CONTAINER_ARGS[@]}" eclipse-temurin:25-jdk bash -c '
    set -e
    CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
    mkdir -p /work
    if [ ! -f /work/server.properties ]; then
      printf "%s\\n" \
        "level-name=world" \
        "level-seed=42" \
        "level-type=minecraft\\:normal" \
        "online-mode=false" \
        "enable-status=false" \
        "pause-when-empty-seconds=0" \
        "view-distance=2" \
        "simulation-distance=2" \
        "server-port=25565" > /work/server.properties
    fi
    printf 'eula=true\n' > /work/eula.txt
    cp /oracle/'"$CLASS"'.java /work/
    javac -cp "$CP" -d /work /work/'"$CLASS"'.java
    java -cp "/work:$CP" '"$CLASS"'
  '
