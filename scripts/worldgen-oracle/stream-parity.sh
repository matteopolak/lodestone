#!/usr/bin/env bash
# Compare the external JVM oracle and Lodestone through an ephemeral stream.
#
# Overworld and Nether frames are light-free content records. End frames carry
# raw packet bytes plus an authenticated resident-lifecycle sidecar so its
# packet heightmaps are compared at the same boundary as the external server.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
dimension="${LODESTONE_LARGE_PARITY_STREAM_DIMENSION:-overworld}"
cx0=0
cx1=0
cz0=0
cz1=0

usage() {
  cat >&2 <<'EOF'
usage: stream-parity.sh [options]

Options:
  --dimension <overworld|nether|end>
  --cx <low> <high>              target chunk x range (default 0..0)
  --cz <low> <high>              target chunk z range (default 0..0)
The target order is z-major with x fastest. All stream output is temporary and
removed on exit; pass explicit coordinate bounds to keep batches bounded.
EOF
  exit "${1:-2}"
}

while (($#)); do
  case "$1" in
    --dimension)
      (($# >= 2)) || usage
      dimension="$2"
      shift 2
      ;;
    --cx)
      (($# >= 3)) || usage
      cx0="$2"
      cx1="$3"
      shift 3
      ;;
    --cz)
      (($# >= 3)) || usage
      cz0="$2"
      cz1="$3"
      shift 3
      ;;
    -h|--help)
      usage 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage
      ;;
  esac
done

[[ "$dimension" = overworld || "$dimension" = nether || "$dimension" = end ]] || { echo "invalid dimension" >&2; exit 2; }
for value in "$cx0" "$cx1" "$cz0" "$cz1"; do
  [[ "$value" =~ ^-?[0-9]+$ ]] || { echo "coordinate bounds must be integers" >&2; exit 2; }
done
((cx0 <= cx1 && cz0 <= cz1)) || { echo "coordinate bounds must be ordered" >&2; exit 2; }

count=$(( (cx1 - cx0 + 1) * (cz1 - cz0 + 1) ))

work="$(mktemp -d "${TMPDIR:-/tmp}/lodestone-stream-parity.XXXXXX")"
mkdir -p "$work/oracle-out"
stream_file="$work/oracle-out/stream"
stream_window="$work/stream.window"
oracle_log="$work/oracle.log"
consumer_log="$work/consumer.log"
oracle_status=0
oracle_pid=""
consumer_pid=""
forwarder_pid=""
cleanup() {
  if [[ -n "$oracle_pid" ]] && kill -0 "$oracle_pid" 2>/dev/null; then
    kill "$oracle_pid" 2>/dev/null || true
  fi
  if [[ -n "$oracle_pid" ]]; then
    wait "$oracle_pid" 2>/dev/null || true
  fi
  if [[ -n "$consumer_pid" ]] && kill -0 "$consumer_pid" 2>/dev/null; then
    kill "$consumer_pid" 2>/dev/null || true
  fi
  if [[ -n "$consumer_pid" ]]; then
    wait "$consumer_pid" 2>/dev/null || true
  fi
  if [[ -n "$forwarder_pid" ]] && kill -0 "$forwarder_pid" 2>/dev/null; then
    kill "$forwarder_pid" 2>/dev/null || true
  fi
  if [[ -n "$forwarder_pid" ]]; then
    wait "$forwarder_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT INT TERM

# Keep the container-visible producer file bounded. The external generator
# appends records to its disposable file because VirtioFS cannot carry the
# stream directly over a FIFO. `tail -f` forwards those bytes into a local FIFO
# and unlinks the producer file immediately after both endpoints are attached.
# Unlinking prevents a named retained artifact; the open inode is bounded by
# producer/consumer back-pressure and disappears on exit.
mkfifo "$stream_window"

oracle_format=(--light-free)
if [[ "$dimension" = end ]]; then
  oracle_format=(--raw-packet)
fi

LODESTONE_ORACLE_OUTPUT_ROOT="$work/oracle-out" \
  bash "$HERE/run.sh" LargeParityOracle \
    --mode stream "${oracle_format[@]}" --dimension "$dimension" \
    --cx "$cx0" "$cx1" --cz "$cz0" "$cz1" \
    --stream-out /oracle-out/stream \
    >"$oracle_log" 2>&1 &
oracle_pid=$!

# Apple-container VirtioFS does not bridge FIFO open handshakes. The oracle
# writes a regular file incrementally and publishes this marker only after the
# first complete frame has been flushed, so consumer startup remains bounded.
frame_ready="$work/oracle-out/stream.frame-ready"
ready_deadline=$((SECONDS + 180))
while [[ ! -e "$frame_ready" ]]; do
  if ! kill -0 "$oracle_pid" 2>/dev/null; then
    oracle_status=1
    wait "$oracle_pid" || oracle_status=$?
    echo "stream oracle failed before its first frame; log: $oracle_log" >&2
    sed -n '1,160p' "$oracle_log" >&2 || true
    exit "$oracle_status"
  fi
  if ((SECONDS >= ready_deadline)); then
    echo "stream oracle did not publish a first frame within 180 seconds; log: $oracle_log" >&2
    sed -n '1,160p' "$oracle_log" >&2 || true
    exit 124
  fi
  sleep 1
done

# Start forwarding only after the first complete frame exists. This avoids a
# platform-dependent `tail -f` failure when the producer path has not yet been
# created, while still preventing any later frames from accumulating on disk.
tail -c +1 -f "$stream_file" >"$stream_window" &
forwarder_pid=$!

set +e
consumer_env=(
  "LODESTONE_LARGE_PARITY_STREAM_FIFO=$stream_window"
  "LODESTONE_LARGE_PARITY_STREAM_COMPLETE=$stream_file.complete"
  "LODESTONE_LARGE_PARITY_STREAM_DIMENSION=$dimension"
)
if [[ -n "${LODESTONE_LARGE_PARITY_STREAM_TIMINGS:-}" ]]; then
  consumer_env+=("LODESTONE_LARGE_PARITY_STREAM_TIMINGS=$LODESTONE_LARGE_PARITY_STREAM_TIMINGS")
fi
if [[ "${LODESTONE_LARGE_PARITY_STREAM_BATCH_SIZE+x}" = x ]]; then
  consumer_env+=("LODESTONE_LARGE_PARITY_STREAM_BATCH_SIZE=$LODESTONE_LARGE_PARITY_STREAM_BATCH_SIZE")
fi
if [[ -n "${LODESTONE_LARGE_PARITY_STREAM_DIAGNOSTICS:-}" ]]; then
  consumer_env+=("LODESTONE_LARGE_PARITY_STREAM_DIAGNOSTICS=$LODESTONE_LARGE_PARITY_STREAM_DIAGNOSTICS")
fi
if [[ -n "${LODESTONE_LARGE_PARITY_STREAM_DEFER_HEIGHTMAPS:-}" ]]; then
  consumer_env+=("LODESTONE_LARGE_PARITY_STREAM_DEFER_HEIGHTMAPS=$LODESTONE_LARGE_PARITY_STREAM_DEFER_HEIGHTMAPS")
fi
env "${consumer_env[@]}" cargo test -p lodestone-v26-2 --test streaming_worldgen_parity \
  stream_external_oracle_matches_lodestone -- --ignored --nocapture \
  >"$consumer_log" 2>&1 &
consumer_pid=$!

# Wait until the FIFO reader and file follower have both attached before
# unlinking the bridge pathname. A reader blocked in `File::open` is not enough
# to prove that `tail` has opened the producer file on every host.
bridge_deadline=$((SECONDS + 30))
while [[ -e "$stream_file" ]]; do
  if lsof -t "$stream_file" 2>/dev/null | grep -qx "$forwarder_pid"; then
    rm -f "$stream_file"
    break
  fi
  if ! kill -0 "$forwarder_pid" 2>/dev/null || ((SECONDS >= bridge_deadline)); then
    break
  fi
  sleep 0.01
done
wait "$consumer_pid"
consumer_status=$?
consumer_pid=""
cat "$consumer_log"
set -e

if ((consumer_status != 0)) && kill -0 "$oracle_pid" 2>/dev/null; then
  kill "$oracle_pid" 2>/dev/null || true
fi
wait "$oracle_pid" || oracle_status=$?
oracle_pid=""
kill "$forwarder_pid" 2>/dev/null || true
wait "$forwarder_pid" 2>/dev/null || true
forwarder_pid=""
if ((consumer_status != 0)); then
  echo "stream comparator failed; oracle log: $oracle_log" >&2
  sed -n '1,120p' "$oracle_log" >&2 || true
  exit "$consumer_status"
fi
if ((oracle_status != 0)); then
  echo "stream oracle failed; log: $oracle_log" >&2
  sed -n '1,120p' "$oracle_log" >&2 || true
  exit "$oracle_status"
fi
