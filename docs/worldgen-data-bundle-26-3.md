# 26.3 worldgen data bundle

## What it is

`lodestone-worldgen-data-26-3` embeds the vanilla 26.3 `density_function`, `noise` and `noise_settings` registries as sorted `(name, json)` tables. It carries text only; the density engine parses it.

## How it works

`assets/<registry>/` holds the JSON copied verbatim from the decompiled 26.3 data under the cache root. `build.rs` walks each directory and emits one `include_str!` table per registry (`DENSITY_FUNCTION`, `NOISE`, `NOISE_SETTINGS`, `MATERIAL_RULE`, `MATERIAL_CONDITION`, `BIOME`, `CLIMATE_POINTS`). Names are the resource path without `minecraft:` or `.json`, such as `overworld/final_density`. `find(table, name)` binary-searches a table and accepts the namespaced form.

## How to change it

Re-run `scripts/regen-worldgen-data-26-3.sh` after a version bump (see [Minecraft version bump](mc-version-bump.md)); it reads the version from `mc-version` and replaces the asset directories. To bundle another registry, pass its name to the script and add its table in `build.rs`. Do not hand-edit the JSON: a fixture comparison against the real server (see [26.3 density engine](worldgen-engine-26-3.md)) is only meaningful while the assets are the shipped files.

## Configuration

None at runtime. The script honours `mc-version` and the cache root resolved by `lodestone-mc-cache`.

## Dependencies

No Rust dependencies. Consumed by `lodestone-worldgen-core` tests today and by the server once the 26.3 terrain scope is switched on.

`climate_points/<preset>.json` is not copied from the jar's data: the multi-noise parameter lists are generated in code, so `scripts/regen-worldgen-data-26-3.sh` dumps them with `ClimateListOracle263` (rows of quantised intervals plus offset and biome, in list order, which seeds the search tree).
