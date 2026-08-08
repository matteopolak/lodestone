#!/usr/bin/env bash
# Regenerate `web/assets/blocks_pack.zip` — the trimmed real resource pack the
# browser build fetches at runtime.
#
# # Why a trimmed pack exists at all
#
# The browser has no filesystem, so `lodestone-assets` is handed *bytes*: one
# `fetch` of a zip, then the ordinary synchronous blockstate/model/atlas pipeline
# runs unchanged. The full vanilla block corpus is ~21 MB uncompressed
# (1198 blockstates, 1371 block textures), which is not something to commit or to
# ship over the wire for a spike — so this script pulls a **named subset**, with
# every model parent and texture reference it transitively needs.
#
# # How to change it
#
# Add block names to `BLOCKS` below and re-run. Two things to know:
#
#  * The list is deliberately overworld-terrain-shaped, because the browser's
#    live-join scene is whatever a real server streams. A block that is missing
#    renders as a **hole** (`terrain::build_terrain_assets_with(.., skip_missing =
#    true)` counts and names them on the HUD), not as an error — so the cost of an
#    incomplete list is cosmetic and visible, never a silent failure.
#  * Fluids (`water`, `lava`) resolve to a model with no elements and no bound
#    texture, so they are skipped by the classifier even when present. Listing
#    them is harmless; expecting them to draw is not.
#
# The fixture render path (`web/fixtures/chunks.bin`) builds its assets
# **strictly** — a block it contains and this pack lacks is a hard error, not a
# hole. `bedrock`/`dirt`/`grass_block` are what that fixture needs, so they must
# stay in the list.
#
# Usage:  scripts/wasm-blocks-pack.sh [path/to/client.jar]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
JAR="${1:-$ROOT/.cache/mc/26.2/client.jar}"
OUT="$ROOT/web/assets/blocks_pack.zip"

if [ ! -f "$JAR" ]; then
  echo "no client jar at $JAR" >&2
  echo "pass one explicitly: scripts/wasm-blocks-pack.sh /path/to/client.jar" >&2
  exit 1
fi

# The blocks the pack covers. Keep this sorted-ish and grouped; it is read far
# more often than it is edited.
BLOCKS="
bedrock dirt grass_block coarse_dirt rooted_dirt podzol mycelium
stone granite diorite andesite tuff calcite deepslate cobbled_deepslate
cobblestone mossy_cobblestone gravel sand red_sand sandstone clay
coal_ore iron_ore copper_ore gold_ore redstone_ore lapis_ore diamond_ore emerald_ore
deepslate_coal_ore deepslate_iron_ore deepslate_copper_ore deepslate_gold_ore
deepslate_redstone_ore deepslate_lapis_ore deepslate_diamond_ore deepslate_emerald_ore
obsidian magma_block blackstone basalt smooth_basalt netherrack
moss_block dripstone_block amethyst_block budding_amethyst
snow_block ice packed_ice blue_ice terracotta
oak_log oak_leaves oak_planks birch_log birch_leaves spruce_log spruce_leaves
jungle_log jungle_leaves acacia_log acacia_leaves dark_oak_log dark_oak_leaves
cherry_log cherry_leaves mangrove_log mangrove_leaves azalea_leaves
water lava
"

python3 - "$JAR" "$OUT" $BLOCKS <<'PY'
import json, os, sys, zipfile

jar_path, out_path, *blocks = sys.argv[1:]

jar = zipfile.ZipFile(jar_path)
names = set(jar.namelist())

def read_json(path):
    if path not in names:
        return None
    return json.loads(jar.read(path))

wanted = set()          # zip entry paths to copy into the pack
model_queue = []
missing_blocks = []

def strip(loc):
    return loc.split(":", 1)[-1]

for block in blocks:
    path = f"assets/minecraft/blockstates/{block}.json"
    data = read_json(path)
    if data is None:
        missing_blocks.append(block)
        continue
    wanted.add(path)
    # Every model this blockstate can select, from either shape (`variants` or
    # `multipart`). We take them all: which one a state selects is decided at
    # runtime by properties we do not know here.
    def collect(node):
        if isinstance(node, dict):
            if "model" in node and isinstance(node["model"], str):
                model_queue.append(strip(node["model"]))
            for value in node.values():
                collect(value)
        elif isinstance(node, list):
            for value in node:
                collect(value)
    collect(data)

# Walk the model graph: parents, and every texture a model binds.
seen_models = set()
textures = set()
while model_queue:
    model = model_queue.pop()
    if model in seen_models:
        continue
    seen_models.add(model)
    path = f"assets/minecraft/models/{model}.json"
    data = read_json(path)
    if data is None:
        continue
    wanted.add(path)
    parent = data.get("parent")
    if isinstance(parent, str):
        model_queue.append(strip(parent))
    for value in (data.get("textures") or {}).values():
        if isinstance(value, str) and not value.startswith("#"):
            textures.add(strip(value))

for texture in textures:
    path = f"assets/minecraft/textures/{texture}.png"
    if path in names:
        wanted.add(path)
    meta = path + ".mcmeta"
    if meta in names:
        wanted.add(meta)

os.makedirs(os.path.dirname(out_path), exist_ok=True)
# Deflate, and sorted so the archive is byte-stable across runs (a pack that
# rewrites itself on every regeneration is a diff nobody can review).
with zipfile.ZipFile(out_path, "w", zipfile.ZIP_DEFLATED) as out:
    for path in sorted(wanted):
        info = zipfile.ZipInfo(path, date_time=(2026, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        out.writestr(info, jar.read(path))

blockstates = sum(1 for p in wanted if "/blockstates/" in p)
models = sum(1 for p in wanted if "/models/" in p)
pngs = sum(1 for p in wanted if p.endswith(".png"))
print(f"{out_path}: {len(wanted)} entries "
      f"({blockstates} blockstates, {models} models, {pngs} textures), "
      f"{os.path.getsize(out_path)} bytes")
if missing_blocks:
    print("not in the jar (typo, or renamed in this version): "
          + " ".join(missing_blocks))
PY
