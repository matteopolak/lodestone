#!/usr/bin/env bash
# Materialize in ordered, cleanly restarted epochs or run one read-only export
# shard. The full raw-packet target is --cx -500 500 --cz -500 500.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
MODE=""
ARGS=( "$@" )
for (( i = 0; i < ${#ARGS[@]}; i++ )); do
  if [ "${ARGS[$i]}" = "--mode" ]; then
    if [ $((i + 1)) -ge ${#ARGS[@]} ]; then
      echo "--mode requires a value" >&2
      exit 2
    fi
    MODE="${ARGS[$((i + 1))]}"
  fi
done

if [ "$MODE" != "materialize" ]; then
  exec "$HERE/run.sh" LargeParityOracle "${ARGS[@]}"
fi

if [ -z "${LODESTONE_ORACLE_WORLD_ROOT:-}" ] || [ ! -d "$LODESTONE_ORACLE_WORLD_ROOT" ]; then
  echo "LODESTONE_ORACLE_WORLD_ROOT must name an existing writable directory" >&2
  exit 2
fi
EPOCH_TILES="${LODESTONE_ORACLE_EPOCH_TILES:-32}"
case "$EPOCH_TILES" in
  *[!0-9]*|0) echo "LODESTONE_ORACLE_EPOCH_TILES must be a positive integer" >&2; exit 2 ;;
esac
dimension="${LODESTONE_ORACLE_DIMENSION:-overworld}"
dimension_arg_explicit=0
raw_packet=0
light_free=0
for (( i = 0; i < ${#ARGS[@]}; i++ )); do
  case "${ARGS[$i]}" in
    --raw-packet|--format-v6) raw_packet=1 ;;
    --light-free|--format-v7) light_free=1 ;;
    --format)
      if [ $((i + 1)) -ge ${#ARGS[@]} ] || { [ "${ARGS[$((i + 1))]}" != v6 ] && [ "${ARGS[$((i + 1))]}" != v7 ]; }; then
        echo "--format accepts v6 (raw packet) or v7 (light-free) for materialization" >&2
        exit 2
      fi
      if [ "${ARGS[$((i + 1))]}" = v6 ]; then raw_packet=1; else light_free=1; fi
      i=$((i + 1))
      ;;
  esac
  if [ "${ARGS[$i]}" = "--dimension" ]; then
    if [ $((i + 1)) -ge ${#ARGS[@]} ]; then echo "--dimension requires a value" >&2; exit 2; fi
    dimension="${ARGS[$((i + 1))]}"
    dimension_arg_explicit=1
  fi
done
case "$dimension" in
  overworld|nether|end) ;;
  *) echo "LODESTONE_ORACLE_DIMENSION must be overworld, nether, or end" >&2; exit 2 ;;
esac
if [ "$raw_packet" -eq 1 ] && [ "$light_free" -eq 1 ]; then
  echo "--raw-packet and --light-free select different explicit formats" >&2
  exit 2
fi
# Keep the Java invocation self-describing. This matters when callers select a
# non-overworld only through the environment: the mode script must not silently
# run an overworld materialization while waiting for a Nether/End seal.
if [ "$dimension_arg_explicit" -eq 0 ] && [ "$dimension" != overworld ]; then
  ARGS+=( --dimension "$dimension" )
fi
if [ "$light_free" -eq 1 ]; then
  freeze_stamp="lodestone-large-parity-materialization-v7-${dimension}.freeze.sha256"
elif [ "$raw_packet" -eq 1 ]; then
  freeze_stamp="lodestone-large-parity-materialization-v6-${dimension}.freeze.sha256"
else
  freeze_stamp="lodestone-large-parity-materialization-v2-${dimension}.freeze.sha256"
fi

while :; do
  LODESTONE_ORACLE_EPOCH_TILES="$EPOCH_TILES" "$HERE/run.sh" LargeParityOracle "${ARGS[@]}"
  if [ -f "$LODESTONE_ORACLE_WORLD_ROOT/$freeze_stamp" ]; then
    break
  fi
done
