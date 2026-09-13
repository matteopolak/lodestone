#!/usr/bin/env bash
# CLI-level controls only: --dry-run must construct P06 commands without
# starting the Java oracle, while invalid roots and batches fail closed.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
DRIVER="$HERE/../full-grid-p06.py"
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/private/tmp}/lodestone-p06-cli.XXXXXX")"
trap 'rm -rf "$TEMP_ROOT"' EXIT

plan="$TEMP_ROOT/plan.txt"
python3 "$DRIVER" \
  --overworld-root "$TEMP_ROOT/overworld" \
  --nether-root "$TEMP_ROOT/nether" \
  --end-root "$TEMP_ROOT/end" \
  --output-root "$TEMP_ROOT/output" \
  --min-free-bytes 1 \
  --min-ram-bytes 1400000000 \
  --rust-command 'fake-rust --manifest' \
  --dry-run >"$plan"

grep -q -- '--raw-packet' "$plan"
grep -q -- '--dimension overworld' "$plan"
grep -q -- '--dimension nether' "$plan"
grep -q -- '--dimension end' "$plan"
grep -q -- 'fake-rust --manifest' "$plan"

if python3 "$DRIVER" \
  --overworld-root "$TEMP_ROOT/overworld" \
  --nether-root "$TEMP_ROOT/nether" \
  --end-root "$TEMP_ROOT/end" \
  --output-root "$HERE" \
  --min-free-bytes 1 \
  --min-ram-bytes 1107296256 \
  --dry-run >/dev/null 2>&1; then
  echo "output root inside repository was accepted" >&2
  exit 1
fi

if python3 "$DRIVER" \
  --overworld-root "$TEMP_ROOT/overworld" \
  --nether-root "$TEMP_ROOT/nether" \
  --end-root "$TEMP_ROOT/end" \
  --output-root "$TEMP_ROOT/output-oversized" \
  --batch-size 2049 \
  --min-free-bytes 1 \
  --min-ram-bytes 1107296256 \
  --dry-run >/dev/null 2>&1; then
  echo "oversized batch was accepted" >&2
  exit 1
fi

echo "full-grid-p06 CLI controls ok"
