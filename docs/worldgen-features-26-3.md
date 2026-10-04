# 26.3 feature decoration

## What it is

`lodestone-worldgen-feature-26-3` runs the 26.3 `feature` and `placed_feature` registries over a chunk: it orders every biome's feature lists into the global per-step sequence, derives the per-chunk decoration seed and the per-feature seed, runs each placed feature's placement modifiers and places the feature, all bit-identical to the real 26.3 server. Ores (`ore`, `scattered_ore`) are ported and oracle-verified; the other feature types parse to `Feature::Unported` and place nothing until their family lands.

## How it works

- `blocks::BlockTable` is the real server's block-state table (`assets/block_facts.txt` in the data bundle, dumped by `BlockFactsOracle263`): property domains plus a fact word per state (air, replaceable, solid, occlusion, fluid, sturdy faces, light). A `State` is the global state id; `with(state, prop, value)` edits a property by the sorted mixed-radix layout. `tags::BlockTags` resolves the bundled block tags (nested `#tag` entries included) into per-block bitsets.
- `env::Env` bundles both plus the six per-state heightmap flags. `level::Level` is the 3x3 chunk window features run in. `set` is the region write (keeps the live heightmaps current, drops writes outside the build height); `set_raw` is the section write ore blobs use (no heightmap upkeep). The two world-generation heightmaps `WORLD_SURFACE_WG` and `OCEAN_FLOOR_WG` are frozen at terrain time; `WORLD_SURFACE`, `OCEAN_FLOOR`, `MOTION_BLOCKING` and `MOTION_BLOCKING_NO_LEAVES` follow writes. `Level::biome` zooms with the obfuscated seed and clamps the quart Y to the stored range (queries above the world, such as an ore range reaching y=384, read the top stored cell).
- `registry::Features::load` parses every placed feature; `registry::sort_features` is the feature sorter: each biome contributes edges between consecutive features, a depth-first search over `(step, first-seen index)` order gives a reverse topological order, and each step's list index is the feature's global index (the seed input). `Decorator::decorate` then mirrors the reference loop: decoration seed from the chunk origin, then per step the sorted union of the present biomes' feature indices, `set_feature_seed(decoration_seed, index, step)`, `PlacedFeature::place`.
- `placement::Placement::modify` appends each modifier's output; `PlacedFeature::place` recurses depth-first, which is the reference's stack discipline (a modifier's whole output is drawn before the first output is processed further). The `biome` modifier asks whether the zoomed biome at the position lists the top-level placed feature in any step.
- Value providers (`provider`), block predicates and rule tests (`predicate`) are parsed once into enums; no JSON is read while placing.

## How to change it

- New feature type: add a variant to `feature::Feature`, parse it in `Feature::parse`, place it in `Feature::place`, and add the type name to the oracle's `types` list in the test. Draw order is the contract: read the real placement once and keep every `nextX` call in order. Reads that the real region answers with void air (outside the build height) go through `Level::get`.
- New placement modifier or block predicate: `placement::Placement`, `predicate::BlockPred`.
- `survive::can_survive` covers the blocks `would_survive` predicates name; an unported block panics rather than guessing.
- After a version bump: `scripts/regen-worldgen-data-26-3.sh` (documents and tags) and `scripts/regen-block-facts-26-3.sh` (state table), then re-run the oracle commands in the test headers.

## Configuration

None at runtime. Tests: `cargo test -p lodestone-worldgen-feature-26-3 --release` (add `-- --ignored --nocapture` for the decoration timing, about 2 ms per chunk for the ore families over the mixed-biome terrain).

## Dependencies

`lodestone-worldgen-data-26-3` (documents, tags, state table), `lodestone-worldgen-core` (random sources, the biome zoom, simplex noise; the tests also use its terrain generator). Verification: `DecorationOracle263` builds the nine chunks the features step receives (fill and surface over a synthetic quart-resolution biome layout, final heightmaps primed, no carvers), allocates the real region without a server level and runs the real sorter, placer and features chunk by chunk. It prints, per executed feature, the random-draw count and a hash of the changed blocks, and a hash of each final chunk, so a mismatch names the first diverging feature. `BlockFactsOracle263` dumps the state table. A wrong-seed control must fail.
