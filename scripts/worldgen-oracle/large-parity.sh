#!/usr/bin/env bash
# Materialize in ordered, cleanly restarted epochs or run one read-only export
# shard. The full target is --cx -500 500 --cz -500 500.
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

while :; do
  LODESTONE_ORACLE_EPOCH_TILES="$EPOCH_TILES" "$HERE/run.sh" LargeParityOracle "${ARGS[@]}"
  if [ -f "$LODESTONE_ORACLE_WORLD_ROOT/lodestone-large-parity-v3.freeze.sha256" ]; then
    break
  fi
done
