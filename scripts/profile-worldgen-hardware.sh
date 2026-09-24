#!/usr/bin/env bash
set -euo pipefail

dry_run="${LODESTONE_WORLDGEN_PROFILE_DRY_RUN:-0}"

if [[ "$dry_run" != 1 && "$(uname -s)" != "Darwin" ]]; then
  echo "profile-worldgen-hardware: CPU Counters requires macOS Instruments" >&2
  exit 2
fi

if [[ "$dry_run" != 1 ]] && ! command -v xcrun >/dev/null; then
  echo "profile-worldgen-hardware: xcrun is unavailable" >&2
  exit 2
fi

seed="${1:-42}"
columns="${2:-8}"
batch_size="${3:-2}"
layout="${4:-line}"
template="${LODESTONE_WORLDGEN_XCTRACE_TEMPLATE:-CPU Counters}"
events="${LODESTONE_WORLDGEN_XCTRACE_EVENTS:-}"
run_id="${LODESTONE_WORLDGEN_PROFILE_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
output_dir="${LODESTONE_WORLDGEN_PROFILE_DIR:-bench-results/profiles/hardware}"

case "$columns" in
  ''|*[!0-9]*)
    echo "profile-worldgen-hardware: columns must be a positive integer" >&2
    exit 2
    ;;
esac
if (( columns == 0 )); then
  echo "profile-worldgen-hardware: columns must be a positive integer" >&2
  exit 2
fi
case "$batch_size" in
  ''|*[!0-9]*)
    echo "profile-worldgen-hardware: batch size must be a positive integer" >&2
    exit 2
    ;;
esac
if (( batch_size == 0 )); then
  echo "profile-worldgen-hardware: batch size must be a positive integer" >&2
  exit 2
fi
case "$layout" in
  line|square) ;;
  *)
    echo "profile-worldgen-hardware: layout must be line or square" >&2
    exit 2
    ;;
esac
if [[ "$layout" == square ]]; then
  side="$(python3 -c 'import math,sys; n=int(sys.argv[1]); print(math.isqrt(n))' "$columns")"
  if (( side * side != columns )); then
    echo "profile-worldgen-hardware: square layout needs a perfect-square column count" >&2
    exit 2
  fi
fi

if [[ "$batch_size" -gt "$columns" ]]; then
  batch_size="$columns"
fi

if [[ -z "$seed" ]]; then
  echo "profile-worldgen-hardware: seed must not be empty" >&2
  exit 2
fi

mkdir -p "$output_dir"
trace="$output_dir/worldgen-${run_id}.trace"
toc="$output_dir/worldgen-${run_id}-toc.xml"
counters="$output_dir/worldgen-${run_id}-counters.xml"
summary="$output_dir/worldgen-${run_id}-summary.txt"
stdout="$output_dir/worldgen-${run_id}.log"

if [[ -e "$trace" || -e "$toc" || -e "$counters" || -e "$summary" || -e "$stdout" ]]; then
  echo "profile-worldgen-hardware: run id already exists: $run_id" >&2
  exit 2
fi

if [[ "$dry_run" == 1 ]]; then
  echo "build=cargo test --release -p lodestone-server --test strict_single_thread_worldgen --no-run --message-format=json"
  echo "metric=production_request"
  echo "seed=$seed"
  echo "columns=$columns"
  echo "batch_size=$batch_size"
  echo "layout=$layout"
  echo "workers=1"
  echo "template=$template"
  [[ -n "$events" ]] && echo "events=$events"
  echo "command=<strict_single_thread_worldgen> --ignored --nocapture --exact strict_single_thread_production_worldgen --test-threads=1"
  exit 0
fi

cargo_output="$(cargo test --release -p lodestone-server --test strict_single_thread_worldgen --no-run --message-format=json)"
binary="$(printf '%s\n' "$cargo_output" | python3 -c '
import json, sys
for line in sys.stdin:
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        continue
    if event.get("reason") == "compiler-artifact" and event.get("target", {}).get("name") == "strict_single_thread_worldgen" and event.get("executable"):
        print(event["executable"])
        break
else:
    raise SystemExit("cargo did not report the strict_single_thread_worldgen executable")
')"
if [[ -z "$binary" || ! -x "$binary" ]]; then
  echo "profile-worldgen-hardware: test executable was not produced" >&2
  exit 1
fi

LODESTONE_WORLDGEN_WORKERS=1 \
LODESTONE_WORLDGEN_BENCH_SEED="$seed" \
LODESTONE_WORLDGEN_BENCH_COLUMNS="$columns" \
LODESTONE_WORLDGEN_BENCH_BATCH="$batch_size" \
LODESTONE_WORLDGEN_BENCH_LAYOUT="$layout" \
xcrun xctrace record \
  --no-prompt \
  --template "$template" \
  --output "$trace" \
  --time-limit 10m \
  --target-stdout "$stdout" \
  --launch -- "$binary" --ignored --nocapture --exact \
  strict_single_thread_production_worldgen --test-threads=1

xcrun xctrace export --input "$trace" --toc --output "$toc"
if grep -q 'schema="counters-profile"' "$toc"; then
  xcrun xctrace export \
    --input "$trace" \
    --xpath '/trace-toc/run[@number="1"]/data/table[@schema="counters-profile"]' \
    --output "$counters"
  summary_args=(
    scripts/summarize-xctrace-counters.py "$counters"
    --process strict_single_thread_worldgen
    --toc "$toc"
    --phase-log "$stdout"
  )
  [[ -n "$events" ]] && summary_args+=(--events "$events")
  python3 "${summary_args[@]}" \
    > "$summary"
fi

echo "metric=production_request"
echo "seed=$seed"
echo "columns=$columns"
echo "batch_size=$batch_size"
echo "layout=$layout"
echo "workers=1"
echo "trace=$trace"
echo "toc=$toc"
[[ -e "$counters" ]] && echo "counters=$counters"
[[ -e "$summary" ]] && echo "summary=$summary"
echo "stdout=$stdout"
