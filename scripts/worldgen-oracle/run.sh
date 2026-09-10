#!/usr/bin/env bash
# Compile & run a worldgen JVM oracle against the real 26.2 server classes,
# in an ephemeral temurin:25-jdk container. Frozen exports get a fresh
# disposable clone of the sealed root; prints the oracle's stdout.
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
WORLD_MOUNTS=()
WORLD_ENV=()
FROZEN_CLONE=""
cleanup_frozen_clone() {
  if [ -n "$FROZEN_CLONE" ] && [ -d "$FROZEN_CLONE" ]; then
    chmod -R u+w "$FROZEN_CLONE" 2>/dev/null || true
    rm -rf "$FROZEN_CLONE"
  fi
}
trap cleanup_frozen_clone EXIT
if [ -n "${LODESTONE_ORACLE_WORLD_ROOT:-}" ]; then
  if [ ! -d "$LODESTONE_ORACLE_WORLD_ROOT" ]; then
    echo "LODESTONE_ORACLE_WORLD_ROOT must name an existing writable directory" >&2
    exit 2
  fi
  WORLD_MOUNTS+=( -v "$LODESTONE_ORACLE_WORLD_ROOT:/world" )
  WORLD_ENV+=( -e ORACLE_WORLD_ROOT=/world )
fi
if [ -n "${LODESTONE_ORACLE_EPOCH_TILES:-}" ]; then
  WORLD_ENV+=( -e "ORACLE_MATERIALIZE_EPOCH_TILES=$LODESTONE_ORACLE_EPOCH_TILES" )
fi
if [ -n "${LODESTONE_ORACLE_DIMENSION:-}" ]; then
  WORLD_ENV+=( -e "ORACLE_DIMENSION=$LODESTONE_ORACLE_DIMENSION" )
fi
if [ -n "${LODESTONE_ORACLE_PAUSE_WHEN_EMPTY_SECONDS:-}" ]; then
  WORLD_ENV+=( -e "ORACLE_PAUSE_WHEN_EMPTY_SECONDS=$LODESTONE_ORACLE_PAUSE_WHEN_EMPTY_SECONDS" )
fi
if [ -n "${LODESTONE_ORACLE_TRAVERSAL:-}" ]; then
  WORLD_ENV+=( -e "ORACLE_TRAVERSAL=$LODESTONE_ORACLE_TRAVERSAL" )
fi
if [ "${LODESTONE_ORACLE_ADMISSION:-}" = 1 ]; then
  WORLD_ENV+=( -e ORACLE_ADMISSION=1 )
fi
for placement_var in SOURCE_X SOURCE_Z PLACED_FEATURE FEATURE_INDEX FEATURE_STEP TARGET_CHUNK TARGET_X TARGET_Z FOCUS FOCUS_X FOCUS_Y FOCUS_Z BLOCK; do
  host_var="LODESTONE_ORACLE_${placement_var}"
  if [ -n "${!host_var:-}" ]; then
    WORLD_ENV+=( -e "ORACLE_${placement_var}=${!host_var}" )
  fi
done
if [ "${LODESTONE_ORACLE_STOP_STAGE_PROBE:-}" = 1 ]; then
  WORLD_ENV+=( -e STOP_STAGE_PROBE=1 )
fi
if [ "${LODESTONE_ORACLE_STREAM_FULL:-}" = 1 ]; then
  WORLD_ENV+=( -e ORACLE_STREAM_FULL=1 )
fi
if [ "${LODESTONE_ORACLE_BEARD_TRACE:-}" = 1 ]; then
  WORLD_ENV+=( -e ORACLE_BEARD_TRACE=1 )
fi
if [ "${LODESTONE_ORACLE_JIGSAW_REPLAY:-}" = 1 ]; then
  WORLD_ENV+=( -e ORACLE_JIGSAW_REPLAY=1 )
fi
for replay_var in REPLAY_FULL_ONLY REPLAY_FIRST_TILE REPLAY_SINGLE_SOURCE REPLAY_BASE_ONLY REPLAY_BIOMES_ONLY; do
  host_var="LODESTONE_ORACLE_${replay_var}"
  if [ "${!host_var:-}" = 1 ]; then
    WORLD_ENV+=( -e "${replay_var}=1" )
  fi
done
if [ -n "${LODESTONE_ORACLE_FROZEN_WORLD_ROOT:-}" ]; then
  if [ ! -d "$LODESTONE_ORACLE_FROZEN_WORLD_ROOT" ]; then
    echo "LODESTONE_ORACLE_FROZEN_WORLD_ROOT must name an existing frozen-world directory" >&2
    exit 2
  fi
  WORLD_MOUNTS+=( -v "$LODESTONE_ORACLE_FROZEN_WORLD_ROOT:/frozen:ro" )
  WORLD_ENV+=( -e ORACLE_FROZEN_WORLD_ROOT=/frozen )
  oracle_dimension="${LODESTONE_ORACLE_DIMENSION:-overworld}"
  raw_packet=0
  light_free=0
  for (( i = 1; i <= $#; i++ )); do
    arg="${!i}"
    case "$arg" in
      --raw-packet|--format-v6) raw_packet=1 ;;
      --light-free|--format-v7) light_free=1 ;;
      --format)
        next=$((i + 1))
        if [ "$next" -le "$#" ] && [ "${!next}" = v7 ]; then light_free=1; fi
        if [ "$next" -le "$#" ] && [ "${!next}" = v6 ]; then raw_packet=1; fi
        ;;
    esac
  done
  if [ "$raw_packet" -eq 1 ] && [ "$light_free" -eq 1 ]; then
    echo "--raw-packet and --light-free select different explicit formats" >&2
    exit 2
  fi
  if [ "$light_free" -eq 1 ]; then
    v7_stamp="lodestone-large-parity-materialization-v7-${oracle_dimension}.freeze.sha256"
    v6_stamp="lodestone-large-parity-materialization-v6-${oracle_dimension}.freeze.sha256"
    if [ -f "$LODESTONE_ORACLE_FROZEN_WORLD_ROOT/$v7_stamp" ]; then
      freeze_stamp="$v7_stamp"
    else
      # P07 is content-only, so an authenticated v6 frozen root is a valid
      # source and avoids a second 1001x1001 materialization pass.
      freeze_stamp="$v6_stamp"
    fi
  elif [ "$raw_packet" -eq 1 ]; then
    freeze_stamp="lodestone-large-parity-materialization-v6-${oracle_dimension}.freeze.sha256"
  else
    freeze_stamp="lodestone-large-parity-materialization-v2-${oracle_dimension}.freeze.sha256"
  fi
  frozen_cache="${LODESTONE_ORACLE_FROZEN_CACHE_ROOT:-${LODESTONE_ORACLE_FROZEN_WORLD_ROOT}.oracle-cache}"
  FROZEN_CLONE="$("$HERE/frozen-world-clone.sh" clone "$LODESTONE_ORACLE_FROZEN_WORLD_ROOT" "$frozen_cache" "$freeze_stamp")"
  WORLD_MOUNTS+=( -v "$FROZEN_CLONE:/frozen-work" )
  WORLD_ENV+=( -e ORACLE_FROZEN_WORK_ROOT=/frozen-work )
fi
if [ -n "${LODESTONE_ORACLE_OUTPUT_ROOT:-}" ]; then
  if [ ! -d "$LODESTONE_ORACLE_OUTPUT_ROOT" ]; then
    echo "LODESTONE_ORACLE_OUTPUT_ROOT must name an existing writable directory" >&2
    exit 2
  fi
  WORLD_MOUNTS+=( -v "$LODESTONE_ORACLE_OUTPUT_ROOT:/oracle-out" )
  WORLD_ENV+=( -e ORACLE_OUTPUT_ROOT=/oracle-out )
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
    LIB_CP="$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
    CP="/mc/versions/26.2/server-26.2.jar:$LIB_CP"
    mkdir -p /work
    if [ ! -f /work/server.properties ]; then
      printf "%s\\n" \
        "level-name=world" \
        "level-seed=42" \
        "level-type=minecraft\\:normal" \
        "online-mode=false" \
        "enable-status=false" \
        "pause-when-empty-seconds=${ORACLE_PAUSE_WHEN_EMPTY_SECONDS:-0}" \
        "view-distance=2" \
        "simulation-distance=2" \
        "server-port=25565" > /work/server.properties
    fi
    printf 'eula=true\n' > /work/eula.txt
    cp /oracle/'"$CLASS"'.java /work/
    if [ '"$CLASS"' = EndHeightmapStatusOracle ] || [ '"$CLASS"' = LargeParityOracle ]; then
      cp /oracle/LargeParityOracle.java /work/
      cp /oracle/EndHeightmapStatusOracle.java /work/
      javac -cp "$CP" -d /work /work/LargeParityOracle.java /work/EndHeightmapStatusOracle.java
    elif [ '"$CLASS"' = NetherPlacementOracle ] || [ '"$CLASS"' = NetherColumnOracle ] || [ '"$CLASS"' = NetherStageOracle ] || [ '"$CLASS"' = NetherFeatureAdmissionOracle ] || [ '"$CLASS"' = NetherFeatureCellsOracle ] || [ '"$CLASS"' = NetherBlobPlacementOracle ] || [ '"$CLASS"' = OverworldFeatureTraceOracle ] || [ '"$CLASS"' = OverworldReplayFeaturesOracle ]; then
      cp /oracle/LargeParityOracle.java /work/
      javac -cp "$CP" -d /work /work/LargeParityOracle.java /work/'"$CLASS"'.java
    else
      javac -cp "$CP" -d /work /work/'"$CLASS"'.java
    fi
    java -Dmax.bg.threads=1 -cp "/work:$CP" '"$CLASS"'
  '
