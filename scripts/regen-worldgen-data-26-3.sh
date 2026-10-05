#!/usr/bin/env bash
# Refreshes crates/lodestone-worldgen-data-26-3/assets from the cached release's
# generated data (`.cache/mc/<mc-version>/src/data/minecraft/worldgen`).
#   usage: scripts/regen-worldgen-data-26-3.sh [registry ...]
# With no arguments the default registry set is copied.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${LODESTONE_MC_VERSION:-$(tr -d '[:space:]' < "$REPO_ROOT/mc-version")}"
SRC="$REPO_ROOT/.cache/mc/$VERSION/src/data/minecraft/worldgen"
DST="$REPO_ROOT/crates/lodestone-worldgen-data-26-3/assets"
[ -d "$SRC" ] || { echo "no worldgen data at $SRC" >&2; exit 1; }
if [ "$#" -eq 0 ]; then set -- biome carver density_function feature material_condition material_rule noise noise_settings placed_feature block_state_provider tag_block processor_list structure; fi
for registry in "$@"; do
  # `tag_block` is the block tag list under data/minecraft/tags/block, not a worldgen registry.
  if [ "$registry" = tag_block ]; then from="$REPO_ROOT/.cache/mc/$VERSION/src/data/minecraft/tags/block"; else from="$SRC/$registry"; fi
  # `structure` holds the binary templates the template and fossil features place; only the
  # directories those features name are copied (the full corpus belongs to the structure bundle).
  if [ "$registry" = structure ]; then
    from="$REPO_ROOT/.cache/mc/$VERSION/src/data/minecraft/structure"
    rm -rf "$DST/structure"
    for dir in desert_well fossil spring; do
      mkdir -p "$DST/structure/$dir"
      cp "$from/$dir"/*.nbt "$DST/structure/$dir/"
    done
    continue
  fi
  [ -d "$from" ] || { echo "missing registry $registry" >&2; exit 1; }
  rm -rf "$DST/$registry"
  mkdir -p "$DST/$registry"
  (cd "$from" && find . -name '*.json' -print0 | sort -z | while IFS= read -r -d '' f; do
    mkdir -p "$DST/$registry/$(dirname "$f")"
    cp "$f" "$DST/$registry/$f"
  done)
done
