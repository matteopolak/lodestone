#!/usr/bin/env bash
# Allocator measurement driver for Lodestone.
#
# Runs each allocator binary across thread counts and free-modes as SEPARATE
# processes so peak RSS is clean per configuration. Peak RSS is read from
# `/usr/bin/time -l` ("maximum resident set size", bytes on macOS); throughput
# comes from the binary's own RESULT line. Prints a CSV to stdout.
#
# Usage: bench.sh [total_columns] [repeats]
set -euo pipefail

cd "$(dirname "$0")"
BIN_DIR="./bin"
COLUMNS="${1:-300000}"
REPEATS="${2:-5}"
ALLOCS=(system mimalloc snmalloc jemalloc)
THREADS=(1 4 8 10)
MODES=(local cross)

# Build one binary per allocator (exactly one global allocator each).
mkdir -p "$BIN_DIR"
for alloc in "${ALLOCS[@]}"; do
  cargo build -p lodestone-allocbench --release \
    --no-default-features --features "alloc-$alloc" >&2
  cp ../../target/release/allocbench "$BIN_DIR/allocbench-$alloc"
done

echo "allocator,threads,mode,columns,run,throughput_ops_per_s,peak_rss_bytes"

for alloc in "${ALLOCS[@]}"; do
  bin="$BIN_DIR/allocbench-$alloc"
  for mode in "${MODES[@]}"; do
    for t in "${THREADS[@]}"; do
      for run in $(seq 1 "$REPEATS"); do
        # `/usr/bin/time -l` writes stats to stderr; the binary writes RESULT to stdout.
        out="$({ /usr/bin/time -l "$bin" "$t" "$mode" "$COLUMNS" ; } 2> .time.err)"
        tp="$(sed -n 's/.*throughput_ops_per_s=\([0-9]*\).*/\1/p' <<<"$out")"
        rss="$(sed -n 's/^[[:space:]]*\([0-9]*\)[[:space:]]*maximum resident set size.*/\1/p' .time.err)"
        echo "$alloc,$t,$mode,$COLUMNS,$run,${tp:-0},${rss:-0}"
      done
    done
  done
done
rm -f .time.err
