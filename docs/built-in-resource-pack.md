# Built-in resource pack

## What it is

Lodestone uses Whimscape by kavast for its built-in visual assets. A generated archive combines that pack with the game definitions the renderer and recipe UI need. Default player skins are the only images kept from the game archive, and the original client archive is a staging input, never a runtime asset.

## How it works

- `web/scripts/stage_resource_pack.py` reads the verified client archive and the vendored Whimscape release, checks both ZIPs, drops base images except the default skin sheets, and overlays Whimscape entries on the remaining definitions. It writes deterministic `lodestone-resources.zip` bytes and a SHA-256 manifest; the staged `pack.mcmeta` credits kavast and links [Whimscape](https://www.curseforge.com/minecraft/texture-packs/whimscape). The original pack is kept under `assets/resource-packs/` so native and browser builds use identical artwork.
- Staging reads the target resource format from the game archive's `version.json` and rejects a visual pack whose declared range excludes it. Compatible overlay directories flatten over the visual pack in declaration order (Whimscape's `format_84-88` overlay applies to format 88 and supplies six map images). The merged archive declares only its target format in the modern range and in the `pack_format`/`supported_formats` fields `lodestone-assets::PackMeta` reads, with overlay declarations removed. This shows format compatibility, not texture coverage.
- Native discovery needs the staged archive beside `generated/reports/blocks.json` (`just run` stages it). The browser build stages it in Trunk's post-build hook; the standalone page fetches it and embedded hosts supply it via `resourcePack` or `assetProvider("resourcePack")`. The SDK holds the staged archive, not the client archive or separate panorama objects. User and server pack selections still layer above it.
- Coverage checks must resolve model parents and texture-variable aliases before comparing paths. Palette-derived trim sprites are generated from base images and palettes (their inventory-atlas integration is separate from worn-armour trims). Custom entity models, connected textures and OptiFine colour or emissive descriptors have no production consumers.
- Static image gaps remain for redstone overlays, powered lightning rods, test blocks, light-item icons, some terrain and particle sprites, llama spit, seasonal chest sheets, optional font art, fallback list icons and the water-side overlay. A missing block-face sprite can currently discard the whole baked model. Clouds and nausea are optional passes whose absence must not suppress other sky or screen effects. Exception: without `textures/misc/shadow.png` the renderer synthesises an anti-aliased black disc (`synthetic_shadow_image` in the shell's entity renderer), since the shadow pipeline reads only alpha. Do not fill other gaps with base-game images.

## How to change it

Replace the vendored pack only after checking redistribution terms, version compatibility, model and texture coverage, and on-screen rendering. Update the pack path in `Justfile` and `web/Trunk.toml`, then run `python3 -m unittest web/scripts/test_stage_resource_pack.py`, `just test-wasm-sdk` and `just wasm-check`. The staging tests keep base images out while retaining default skins, check overlay precedence and reject incompatible metadata. Check a native and a browser world visually, since a successful atlas build does not prove every surface has art; missing textures need pack-side artwork.

## Configuration

`LODESTONE_ASSETS` may point at a directory with `lodestone-resources.zip` and `generated/reports/blocks.json`. `just stage-resources` regenerates the local archive. `LODESTONE_WEB_CLIENT_JAR_PARTS=1` splits the staged archive for hosts with per-file limits (the name is kept for deployment scripts).

## Dependencies

Python standard-library ZIP and hashing for staging; `lodestone-assets::ZipSource` and the block-state report at runtime; the browser SDK packages those bytes with its Wasm modules.
