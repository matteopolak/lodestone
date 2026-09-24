#!/usr/bin/env bash
# Generate one disjoint share of the full parity grid as compact shards.
# Compact 32-wide shards keep each scheduler batch spatially local while
# reducing the number of JVM restarts needed for the 1001 x 1001 raw grid.
# End exports may opt into two-row horizontal shards so each JVM handles at most
# 2002 centres before its dirty-chunk state is discarded.
set -euo pipefail

worker="${1:?usage: full-parity-worker.sh WORKER_INDEX WORKER_COUNT}"
workers="${2:?usage: full-parity-worker.sh WORKER_INDEX WORKER_COUNT}"
if ! [[ "$worker" =~ ^[0-9]+$ && "$workers" =~ ^[1-9][0-9]*$ ]] || (( worker >= workers )); then
  echo "worker index must be in 0..WORKER_COUNT-1" >&2
  exit 2
fi
if [ -z "${LODESTONE_ORACLE_FROZEN_WORLD_ROOT:-}" ]; then
  echo "LODESTONE_ORACLE_FROZEN_WORLD_ROOT must name the sealed shared world" >&2
  exit 2
fi
shard_dir="${LODESTONE_ORACLE_SHARD_DIR:-baseline-tiles}"
if [[ "$shard_dir" == /* || "$shard_dir" == *".."* ]]; then
  echo "LODESTONE_ORACLE_SHARD_DIR must be a relative directory below /oracle" >&2
  exit 2
fi
dimension="${LODESTONE_ORACLE_DIMENSION:-overworld}"
case "$dimension" in
  overworld|nether|end) ;;
  *) echo "LODESTONE_ORACLE_DIMENSION must be overworld, nether, or end" >&2; exit 2 ;;
esac
output_prefix=/oracle
if [ -n "${LODESTONE_ORACLE_OUTPUT_ROOT:-}" ]; then
  output_prefix=/oracle-out
fi

here="$(cd "$(dirname "$0")" && pwd)"
grid_min=-500
grid_max=500
light_free=0
case "${LODESTONE_ORACLE_FORMAT:-}" in
  v7|light-free) light_free=1 ;;
  "") if [ "${LODESTONE_ORACLE_LIGHT_FREE:-0}" = 1 ]; then light_free=1; fi ;;
  v6|raw-packet) ;;
  *) echo "LODESTONE_ORACLE_FORMAT must be v6 or v7" >&2; exit 2 ;;
esac

if [[ "$dimension" == end && "${LODESTONE_ORACLE_END_BOUNDED:-0}" == 1 ]]; then
  slot=0
  for (( z_lo = grid_min; z_lo <= grid_max; z_lo += 2, slot += 1 )); do
    if (( slot % workers != worker )); then
      continue
    fi
    z_hi=$((z_lo + 1))
    if (( z_hi > grid_max )); then
      z_hi="$grid_max"
    fi
    output_rel="${shard_dir}/end/shard-z${z_lo}-${z_hi}.lwp"
    export_command=( "$here/large-parity.sh" --mode export --dimension end )
    if [ "$light_free" -eq 1 ]; then export_command+=( --light-free ); fi
    "${export_command[@]}" \
      --out "${output_prefix}/${output_rel}" \
      --cx "$grid_min" "$grid_max" --cz "$z_lo" "$z_hi" --resume
  done
  exit 0
fi

slot=0
for (( x_lo = grid_min; x_lo <= grid_max; x_lo += 32, slot += 1 )); do
  if (( slot % workers != worker )); then
    continue
  fi
  x_hi=$((x_lo + 31))
  if (( x_hi > grid_max )); then
    x_hi="$grid_max"
  fi
  export_command=( "$here/large-parity.sh" --mode export )
  if [ "$dimension" != overworld ]; then
    export_command+=( --dimension "$dimension" )
  fi
  if [ "$light_free" -eq 1 ]; then
    export_command+=( --light-free )
  fi
  output_rel="${shard_dir}/shard-x${x_lo}-${x_hi}.lwp"
  if [ "$dimension" != overworld ]; then
    output_rel="${shard_dir}/${dimension}/shard-x${x_lo}-${x_hi}.lwp"
  fi
  "${export_command[@]}" \
    --out "${output_prefix}/${output_rel}" \
    --cx "$x_lo" "$x_hi" --cz "$grid_min" "$grid_max" --resume
done
