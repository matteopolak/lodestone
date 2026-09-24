#!/usr/bin/env bash
# Run two independent read-only P07 exports from one sealed P06/P07 root and
# prove that the source tree is unchanged and both authenticated streams agree.
set -euo pipefail

root="${LODESTONE_ORACLE_FROZEN_WORLD_ROOT:?LODESTONE_ORACLE_FROZEN_WORLD_ROOT must name the sealed source root}"
dimension="${LODESTONE_ORACLE_DIMENSION:-overworld}"
cx="${LODESTONE_LIGHT_FREE_CONTROL_X:-0}"
cz="${LODESTONE_LIGHT_FREE_CONTROL_Z:-0}"
here="$(cd "$(dirname "$0")" && pwd)"
out="$(mktemp -d "${TMPDIR:-/tmp}/lodestone-light-free-control.XXXXXX")"
trap 'rm -rf "$out"' EXIT

tree_digest() {
  (
    cd "$root"
    find . -type f -print0 | sort -z | while IFS= read -r -d '' file; do
      shasum -a 256 "$file"
    done
  ) | shasum -a 256 | awk '{print $1}'
}

before="$(tree_digest)"
LODESTONE_ORACLE_OUTPUT_ROOT="$out" "$here/large-parity.sh" --mode export --light-free --dimension "$dimension" --out /oracle-out/read-a.lwp --cx "$cx" "$cx" --cz "$cz" "$cz"
LODESTONE_ORACLE_OUTPUT_ROOT="$out" "$here/large-parity.sh" --mode export --light-free --dimension "$dimension" --out /oracle-out/read-b.lwp --cx "$cx" "$cx" --cz "$cz" "$cz"
after="$(tree_digest)"

test "$before" = "$after"
cmp "$out/read-a.lwp" "$out/read-b.lwp"
cmp "$out/read-a.lwp.light-free-audit" "$out/read-b.lwp.light-free-audit"
python3 "$here/large-parity-manifest.py" validate "$out/read-a.lwp" >/dev/null
python3 "$here/large-parity-manifest.py" validate "$out/read-b.lwp" >/dev/null
echo "light-free read control ok: sealed root unchanged and independent P07 exports match"
