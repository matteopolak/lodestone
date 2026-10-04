# Built-in resource pack

## What it is

Lodestone uses Whimscape by kavast for its built-in visual assets. A generated archive combines that pack with the game definitions needed by the renderer and recipe UI. Default player skins are the only images retained from the game archive; the original client archive is an input to staging, not a runtime asset.

## How it works

`web/scripts/stage_resource_pack.py` reads the verified client archive and the vendored Whimscape release, checks both ZIPs, excludes base images except the default player-skin sheets, and overlays Whimscape entries on the remaining definitions. It writes deterministic `lodestone-resources.zip` bytes and a SHA-256 manifest. The staged `pack.mcmeta` credits kavast and links to [Whimscape](https://www.curseforge.com/minecraft/texture-packs/whimscape). The original pack is kept under `assets/resource-packs/` so native and browser builds use identical artwork.

Staging reads the target resource format from the game archive's `version.json` and rejects a visual pack whose declared range excludes it. Compatible overlay directories are flattened over the visual pack in declaration order. Whimscape's `format_84-88` overlay applies to format 88 and supplies six map-related images. The merged archive declares only its target format in both the modern range and the `pack_format`/`supported_formats` fields understood by `lodestone-assets::PackMeta`; overlay declarations are removed after application. This describes compatibility with the selected data format, not verified texture coverage.

Native resource discovery requires the staged archive beside `generated/reports/blocks.json`. `just run` stages it before launching. The browser build stages the same archive during Trunk's post-build hook; the standalone page fetches it, while embedded hosts provide it through `resourcePack` or `assetProvider("resourcePack")`. The SDK contains the staged archive, not the original client archive or separate panorama objects. Resource-pack selections still layer above the built-in archive.

Coverage checks must resolve model parents and texture-variable aliases before comparing image paths. Palette-derived trim sprites are generated from supplied base images and palettes, not missing authored PNGs; their inventory-atlas integration is separate from worn-armour trims. Custom entity models, connected textures and OptiFine colour/emissive descriptors have no production consumers.

Resolved image gaps remain for redstone overlays, powered lightning rods, test blocks, light-item icons, some terrain/particle sprites, llama spit, seasonal chest sheets, optional font art and fallback list icons. The water-side overlay also lacks an image. These are static unresolved references, not a visual-coverage result: a missing block-face sprite can currently discard the whole baked model. Clouds and nausea are independently optional passes; their absence must not suppress unrelated sky or screen effects. The entity ground-shadow mask is the exception: when the pack lacks `textures/misc/shadow.png`, the renderer synthesises an anti-aliased black disc (`synthetic_shadow_image` in the shell's entity renderer), because the shadow pipeline reads only its alpha. Do not fill other gaps with base-game images.

## How to change it

Replace the vendored pack only after checking its redistribution terms, version compatibility, model and texture coverage, and on-screen rendering. Update the pack path in `Justfile` and `web/Trunk.toml`, then run `python3 -m unittest web/scripts/test_stage_resource_pack.py`, `just test-wasm-sdk`, and `just wasm-check`. The staging tests keep ordinary base images out while retaining default player skins, check compatible overlay precedence, and reject incompatible metadata. Check a native and browser world visually; a successful atlas build alone does not prove every surface has art. Missing textures require pack-side artwork rather than a base-game image fallback.

## Configuration

`LODESTONE_ASSETS` may point to a directory containing `lodestone-resources.zip` and `generated/reports/blocks.json`. `just stage-resources` regenerates the local archive. `LODESTONE_WEB_CLIENT_JAR_PARTS=1` asks the browser hook to split the staged archive for hosts with a per-file limit; the variable name is retained for existing deployment scripts.

## Dependencies

Staging uses Python's standard-library ZIP and hashing modules. Runtime loading uses `lodestone-assets::ZipSource` and the generated block-state report. The browser SDK packages those bytes alongside its Wasm modules.
