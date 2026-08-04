#!/usr/bin/env bash
# Region-level worldgen throughput + peak-RSS sweep (issue #78 epic, sub-issue
# #87). `bench_worldgen` (crates/lodestone-server/examples/bench_worldgen.rs)
# already parameterises radius via argv and reports wall-clock, but never
# memory, and never automatically at more than one radius per invocation.
# This script runs it once per radius as a SEPARATE process (so peak RSS is
# clean per run, the same reasoning `lodestone-allocbench/bench.sh` already
# uses) and reads peak RSS from `/usr/bin/time -l`, which that script already
# validated on macOS — reused rather than a second RSS-measurement method.
#
# Usage: scripts/worldgen-region-sweep.sh [seed] [radii...]
#   Default radii: 8 16 (RD 32 is available but NOT run by default — see
#   CLAUDE.md's machine-courtesy section: RD 32 is 4225 chunks and this
#   machine is shared with several other concurrent agents. Pass it
#   explicitly, once, when the machine is otherwise idle.)
set -euo pipefail

cd "$(dirname "$0")/.."
SEED="${1:-3}"
shift || true
if [ "$#" -eq 0 ]; then
  RADII=(8 16)
else
  RADII=("$@")
fi

echo "Building bench_worldgen (release)..." >&2
cargo build --release -p lodestone-server --example bench_worldgen >&2

BIN="./target/release/examples/bench_worldgen"
# Full stdout/stderr goes to a log per radius so nothing here has to parse
# the program's own numbers through a pipeline for anything load-bearing;
# this script's only self-measured, trustworthy number is wall time via
# bash's own clock and peak RSS via `/usr/bin/time -l`, both read from a file
# with a program (`awk`/`sed` on a completed file), never from a live pipe.
echo "radius,chunks,seed,wall_seconds,peak_rss_bytes,peak_rss_mib,log_file"

prev_wall=""
prev_chunks=""
for radius in "${RADII[@]}"; do
  chunks=$(( (2 * radius + 1) * (2 * radius + 1) ))
  log=".worldgen-sweep-r${radius}.log"
  t0=$(date +%s.%N)
  /usr/bin/time -l "$BIN" "$SEED" "$radius" > "$log" 2> "${log}.time"
  t1=$(date +%s.%N)
  wall=$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.3f", b - a }')
  rss="$(sed -n 's/^[[:space:]]*\([0-9]*\)[[:space:]]*maximum resident set size.*/\1/p' "${log}.time")"
  rss="${rss:-0}"
  rss_mib="$(awk -v b="$rss" 'BEGIN { printf "%.1f", b / 1048576 }')"
  echo "$radius,$chunks,$SEED,$wall,$rss,$rss_mib,$log"

  if [ -n "$prev_wall" ] && [ -n "$prev_chunks" ]; then
    ratio="$(awk -v w="$wall" -v pw="$prev_wall" -v c="$chunks" -v pc="$prev_chunks" \
      'BEGIN { if (pw > 0 && pc > 0) printf "%.2f", (w / pw) / (c / pc); else print "NA" }')"
    echo "  -> vs previous radius: time ratio / chunk-count ratio = ${ratio}x (close to 1.0 is linear)" >&2
  fi
  prev_wall="$wall"
  prev_chunks="$chunks"
done
echo "Per-radius stdout (per-stage split, speedup, parity check) is in .worldgen-sweep-r*.log" >&2
