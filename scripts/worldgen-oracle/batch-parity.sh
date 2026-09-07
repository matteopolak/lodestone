#!/usr/bin/env bash
# Export and validate one authenticated multi-thousand-chunk parity shard.
#
# This is deliberately a thin orchestration layer around large-parity.sh.  It
# does not create a baseline or reinterpret legacy fingerprints: the Java
# exporter reads the sealed frozen world and the manifest validator authenticates
# the v4/v5 semantic payload before the shard is handed to the Rust comparator.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"

usage() {
  cat >&2 <<'EOF'
usage: batch-parity.sh [options]

Required environment:
  LODESTONE_ORACLE_FROZEN_WORLD_ROOT  sealed world, mounted read-only by the wrapper
  LODESTONE_ORACLE_OUTPUT_ROOT        writable directory for the resulting shard

Options:
  --dimension <overworld|nether|end>  dimension (default: overworld)
  --cx <low> <high>                  target chunk x range
  --cz <low> <high>                  target chunk z range
  --out <relative-path>              output below LODESTONE_ORACLE_OUTPUT_ROOT

With no coordinate options this exports 50 x 40 = 2,000 chunks at
cx=-250..-201, cz=-250..-211.  The range must stay inside the authenticated
501 x 501 target grid and contain at least 2,000 chunks.
EOF
  exit "${1:-2}"
}

dimension="${LODESTONE_ORACLE_DIMENSION:-overworld}"
cx0=-250
cx1=-201
cz0=-250
cz1=-211
out="batch-${dimension}-2000.lwp"
cx_set=0
cz_set=0

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
      cx_set=1
      shift 3
      ;;
    --cz)
      (($# >= 3)) || usage
      cz0="$2"
      cz1="$3"
      cz_set=1
      shift 3
      ;;
    --out)
      (($# >= 2)) || usage
      out="$2"
      shift 2
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

: "${LODESTONE_ORACLE_FROZEN_WORLD_ROOT:?LODESTONE_ORACLE_FROZEN_WORLD_ROOT must name the sealed frozen world}"
: "${LODESTONE_ORACLE_OUTPUT_ROOT:?LODESTONE_ORACLE_OUTPUT_ROOT must name a writable output directory}"

case "$dimension" in
  overworld|nether|end) ;;
  *) echo "--dimension must be overworld, nether, or end" >&2; exit 2 ;;
esac

if ((cx_set != cz_set)); then
  echo "--cx and --cz must be supplied together" >&2
  exit 2
fi
if [[ -z "$out" || "$out" = "." || "$out" = */ || "$out" = /* || "$out" = *..* ]]; then
  echo "--out must be a relative path below LODESTONE_ORACLE_OUTPUT_ROOT" >&2
  exit 2
fi
for coordinate in "$cx0" "$cx1" "$cz0" "$cz1"; do
  if ! [[ "$coordinate" =~ ^-?[0-9]+$ ]]; then
    echo "coordinate bounds must be signed decimal integers" >&2
    exit 2
  fi
done
if ((cx0 < -250 || cx1 > 250 || cz0 < -250 || cz1 > 250 || cx0 > cx1 || cz0 > cz1)); then
  echo "coordinate bounds must lie within -250..250 and be ordered" >&2
  exit 2
fi

count=$(( (cx1 - cx0 + 1) * (cz1 - cz0 + 1) ))
if ((count < 2000)); then
  echo "batch must contain at least 2,000 chunks (requested $count)" >&2
  exit 2
fi

mkdir -p "$LODESTONE_ORACLE_OUTPUT_ROOT"
mkdir -p "$LODESTONE_ORACLE_OUTPUT_ROOT/$(dirname -- "$out")"
LODESTONE_ORACLE_DIMENSION="$dimension" \
  "$HERE/large-parity.sh" --mode export --dimension "$dimension" \
  --out "/oracle-out/$out" --cx "$cx0" "$cx1" --cz "$cz0" "$cz1"

manifest="$LODESTONE_ORACLE_OUTPUT_ROOT/$out"
python3 "$HERE/large-parity-manifest.py" validate "$manifest"
echo "authenticated batch ready: dimension=$dimension chunks=$count manifest=$manifest"
