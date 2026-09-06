#!/usr/bin/env bash
# Generate one disjoint share of the full parity grid as compact vertical shards.
# Compact 16-wide shards keep each 256-record scheduler batch spatially local.
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

here="$(cd "$(dirname "$0")" && pwd)"
grid_min=-250
grid_max=250
slot=0
for (( x_lo = grid_min; x_lo <= grid_max; x_lo += 16, slot += 1 )); do
  if (( slot % workers != worker )); then
    continue
  fi
  x_hi=$((x_lo + 15))
  if (( x_hi > grid_max )); then
    x_hi=grid_max
  fi
  "$here/large-parity.sh" \
    --mode export \
    --out "/oracle/${shard_dir}/shard-x${x_lo}-${x_hi}.lwp" \
    --cx "$x_lo" "$x_hi" --cz "$grid_min" "$grid_max" --resume
done
