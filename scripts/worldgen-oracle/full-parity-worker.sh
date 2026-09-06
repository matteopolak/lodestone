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

here="$(cd "$(dirname "$0")" && pwd)"
slot=0
for (( x_lo = -500; x_lo <= 500; x_lo += 16, slot += 1 )); do
  if (( slot % workers != worker )); then
    continue
  fi
  x_hi=$((x_lo + 15))
  if (( x_hi > 500 )); then
    x_hi=500
  fi
  "$here/large-parity.sh" \
    --out "/oracle/baseline-tiles/shard-x${x_lo}-${x_hi}.lwp" \
    --cx "$x_lo" "$x_hi" --cz -500 500 --resume
done
