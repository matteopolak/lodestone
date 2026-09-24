#!/usr/bin/env bash
# Run one bounded raw-packet parity scan, then export reference packet bodies
# only for authenticated mismatches and cluster exact component signatures.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
usage() {
  cat >&2 <<'EOF'
usage: diagnose-parity.sh

Required environment:
  LODESTONE_LARGE_PARITY_MANIFEST       authenticated v6 manifest
  LODESTONE_ORACLE_FROZEN_WORLD_ROOT   sealed root used by the manifest
  LODESTONE_ORACLE_OUTPUT_ROOT         writable directory for diagnostic files
  LODESTONE_ORACLE_DIMENSION           overworld, nether, or end

Optional environment:
  LODESTONE_LARGE_PARITY_BATCH_SIZE     scan size (default: 2000; at least 2000)
  LODESTONE_LARGE_PARITY_DIAGNOSTIC_ROOT  subdirectory name (default: parity-diagnostics)

The scan retains at most 4,096 packet bodies and 64 MiB of generated packet
bytes.  The JVM reference phase receives only the coordinates whose full
packet digest differs from the authenticated packet-audit sidecar.
EOF
  exit "${1:-2}"
}

manifest="${LODESTONE_LARGE_PARITY_MANIFEST:?LODESTONE_LARGE_PARITY_MANIFEST is required}"
frozen="${LODESTONE_ORACLE_FROZEN_WORLD_ROOT:?LODESTONE_ORACLE_FROZEN_WORLD_ROOT is required}"
output_root="${LODESTONE_ORACLE_OUTPUT_ROOT:?LODESTONE_ORACLE_OUTPUT_ROOT is required}"
dimension="${LODESTONE_ORACLE_DIMENSION:?LODESTONE_ORACLE_DIMENSION is required}"
batch_size="${LODESTONE_LARGE_PARITY_BATCH_SIZE:-2000}"
diagnostic_root_name="${LODESTONE_LARGE_PARITY_DIAGNOSTIC_ROOT:-parity-diagnostics}"

[[ -f "$manifest" ]] || { echo "manifest is not a regular file: $manifest" >&2; exit 2; }
[[ -d "$frozen" ]] || { echo "frozen root is not a directory: $frozen" >&2; exit 2; }
[[ -d "$output_root" ]] || { echo "oracle output root is not a directory: $output_root" >&2; exit 2; }
[[ "$dimension" = overworld || "$dimension" = nether || "$dimension" = end ]] || usage
[[ "$batch_size" =~ ^[0-9]+$ && "$batch_size" -ge 2000 ]] || { echo "LODESTONE_LARGE_PARITY_BATCH_SIZE must be at least 2000" >&2; exit 2; }
[[ "$diagnostic_root_name" != /* && "$diagnostic_root_name" != *..* && "$diagnostic_root_name" != */* ]] || { echo "diagnostic root name must be a single relative directory name" >&2; exit 2; }

diagnostic_root="$output_root/$diagnostic_root_name"
if [[ -e "$diagnostic_root" ]]; then
  echo "diagnostic root already exists; use a new output root or remove the prior diagnostic run: $diagnostic_root" >&2
  exit 2
fi
mkdir -p "$diagnostic_root/all-packets" "$diagnostic_root/mismatch-packets" "$diagnostic_root/reference-packets"
inventory="$diagnostic_root/mismatches.tsv"
coordinates="$diagnostic_root/coordinates.tsv"
report="$diagnostic_root/component-report.txt"

set +e
LODESTONE_LARGE_PARITY_MANIFEST="$manifest" \
LODESTONE_LARGE_PARITY_SCAN_ALL=1 \
LODESTONE_LARGE_PARITY_BATCH_SIZE="$batch_size" \
LODESTONE_LARGE_PARITY_MISMATCH_OUT="$inventory" \
LODESTONE_LARGE_PARITY_PACKET_OUT_DIR="$diagnostic_root/all-packets" \
cargo test -p lodestone-v26-2 --test large_worldgen_parity \
  parity_manifest_streams_before_rust_comparison -- --ignored --nocapture
scan_status=$?
set -e

if [[ ! -f "$inventory" ]]; then
  echo "parity scan did not produce its authenticated mismatch inventory (exit $scan_status)" >&2
  exit "$scan_status"
fi
if [[ "$scan_status" -ne 0 && "$scan_status" -ne 101 ]]; then
  echo "parity scan failed before producing a comparable result (exit $scan_status)" >&2
  exit "$scan_status"
fi

python3 "$HERE/parity-diagnostics.py" \
  --manifest "$manifest" \
  --inventory "$inventory" \
  --coordinates-out "$coordinates" \
  --frozen-world-root "$frozen" \
  --packet-source "$diagnostic_root/all-packets" \
  --packet-output "$diagnostic_root/mismatch-packets"

row_count="$(awk -F= '$1 == "mismatch_count" { print $2 }' "$inventory")"
if [[ -z "$row_count" || "$row_count" = 0 ]]; then
  echo "authenticated parity scan found no mismatches"
  exit "$scan_status"
fi

LODESTONE_ORACLE_FROZEN_WORLD_ROOT="$frozen" \
LODESTONE_ORACLE_OUTPUT_ROOT="$output_root" \
LODESTONE_ORACLE_DIMENSION="$dimension" \
  "$HERE/large-parity.sh" --mode diagnostic --dimension "$dimension" --raw-packet \
  --diagnostic-coordinates "/oracle-out/$diagnostic_root_name/coordinates.tsv" \
  --diagnostic-packet-dir "/oracle-out/$diagnostic_root_name/reference-packets"

LODESTONE_LARGE_PARITY_MANIFEST="$manifest" \
LODESTONE_LARGE_PARITY_MISMATCH_OUT="$inventory" \
LODESTONE_LARGE_PARITY_MISMATCH_PACKET_DIR="$diagnostic_root/mismatch-packets" \
LODESTONE_LARGE_PARITY_MISMATCH_REFERENCE_PACKET_DIR="$diagnostic_root/reference-packets" \
LODESTONE_LARGE_PARITY_MISMATCH_COMPONENT_REPORT="$report" \
  cargo test -p lodestone-v26-2 --test large_worldgen_parity \
  parity_mismatch_packet_files_report_components -- --ignored --nocapture

echo "component diagnostics written to $report"
