#!/usr/bin/env bash
# Run the small independent-root reproducibility control. This intentionally
# leaves every root and export behind for inspection; it never deletes data.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
side=21
work_root=""
skip_reverse=0

usage() {
  cat >&2 <<'EOF'
usage: reproducibility-control.sh [--side 21|51] [--work-root DIRECTORY] [--skip-reverse]

The control materializes two fresh forward roots, exports their light-free
records, and compares payloads without comparing per-root headers. Unless
--skip-reverse is supplied, it also materializes a reverse-order diagnostic
root and requires that payload to differ from the forward canonical payload.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --side)
      [ "$#" -ge 2 ] || { usage; exit 2; }
      side="$2"
      shift 2
      ;;
    --work-root)
      [ "$#" -ge 2 ] || { usage; exit 2; }
      work_root="$2"
      shift 2
      ;;
    --skip-reverse)
      skip_reverse=1
      shift
      ;;
    --help|-h)
      usage >&1
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

case "$side" in
  21) lo=-10; hi=10 ;;
  51) lo=-25; hi=25 ;;
  *) echo "--side must be 21 or 51" >&2; exit 2 ;;
esac

if [ -z "$work_root" ]; then
  work_root="$(mktemp -d "${TMPDIR:-/private/tmp}/lodestone-repro-control.XXXXXX")"
else
  if [ -e "$work_root" ]; then
    echo "--work-root must not already exist: $work_root" >&2
    exit 2
  fi
  mkdir -p "$work_root"
fi

forward_a="$work_root/forward-a"
forward_b="$work_root/forward-b"
reverse="$work_root/reverse"
export_a="$work_root/export-a"
export_b="$work_root/export-b"
export_reverse="$work_root/export-reverse"
mkdir "$forward_a" "$forward_b" "$export_a" "$export_b"
[ "$skip_reverse" -eq 1 ] || mkdir "$reverse" "$export_reverse"

epoch_tiles="${LODESTONE_ORACLE_EPOCH_TILES:-1}"
pause_seconds="${LODESTONE_ORACLE_PAUSE_WHEN_EMPTY_SECONDS:-1}"

materialize() {
  local root="$1" traversal="$2"
  if [ "$traversal" = forward ]; then
    env \
      LODESTONE_ORACLE_WORLD_ROOT="$root" \
      LODESTONE_ORACLE_EPOCH_TILES="$epoch_tiles" \
      LODESTONE_ORACLE_PAUSE_WHEN_EMPTY_SECONDS="$pause_seconds" \
      "$HERE/large-parity.sh" --mode materialize --format v7 \
        --cx "$lo" "$hi" --cz "$lo" "$hi"
  else
    env \
      LODESTONE_ORACLE_WORLD_ROOT="$root" \
      LODESTONE_ORACLE_EPOCH_TILES="$epoch_tiles" \
      LODESTONE_ORACLE_PAUSE_WHEN_EMPTY_SECONDS="$pause_seconds" \
      LODESTONE_ORACLE_TRAVERSAL="$traversal" \
      "$HERE/large-parity.sh" --mode materialize --format v7 \
        --cx "$lo" "$hi" --cz "$lo" "$hi"
  fi
}

export_records() {
  local root="$1" output_dir="$2" name="$3"
  env \
    LODESTONE_ORACLE_FROZEN_WORLD_ROOT="$root" \
    LODESTONE_ORACLE_OUTPUT_ROOT="$output_dir" \
    "$HERE/large-parity.sh" --mode export --format v7 \
      --cx "$lo" "$hi" --cz "$lo" "$hi" --out "/oracle-out/$name.lwp"
}

payload() {
  local source="$1" destination="$2"
  dd if="$source" of="$destination" bs=256 skip=1 status=none
}

echo "control work root: $work_root"
echo "control geometry: ${side}x${side} (${lo}..${hi})"
echo "materializing independent forward root A"
materialize "$forward_a" forward
echo "materializing independent forward root B"
materialize "$forward_b" forward
echo "exporting independent forward roots"
export_records "$forward_a" "$export_a" forward-a
export_records "$forward_b" "$export_b" forward-b
payload "$export_a/forward-a.lwp" "$work_root/forward-a.payload"
payload "$export_b/forward-b.lwp" "$work_root/forward-b.payload"
if ! cmp -s "$work_root/forward-a.payload" "$work_root/forward-b.payload"; then
  echo "FAIL: independent forward payloads differ" >&2
  echo "evidence: $work_root/forward-a.payload $work_root/forward-b.payload" >&2
  exit 1
fi
echo "PASS: independent forward payloads are identical (headers intentionally differ)"

if [ "$skip_reverse" -eq 0 ]; then
  echo "materializing reverse-order diagnostic root"
  materialize "$reverse" reverse
  echo "exporting reverse-order diagnostic root"
  export_records "$reverse" "$export_reverse" reverse
  payload "$export_reverse/reverse.lwp" "$work_root/reverse.payload"
  if cmp -s "$work_root/forward-a.payload" "$work_root/reverse.payload"; then
    echo "FAIL: reverse traversal unexpectedly matched the canonical payload" >&2
    exit 1
  fi
  echo "PASS: reverse traversal differs and is rejected as a canonical baseline"
fi
