# Overworld world-type selection

## What it is

`worldgen_data::WorldType` (`crates/lodestone-server/src/worldgen_data.rs`), a
parameter that picks which bundled overworld `noise_settings` document and
density-function set a generator uses. Before issue #519 the overworld's
settings were a hardcoded `OnceLock`, so the `amplified` and `large_biomes`
`noise_settings`/`density_function` documents — bundled and byte-identical to
the jar — were unreachable from any code path. `WorldType` closes that gap for
the two presets that need no new engine: `Overworld` (the pre-existing
default), `Amplified` and `LargeBiomes`.

## How it works

`WorldType::settings_asset` maps each variant to its embedded
`noise_settings/<id>` key; `settings_for(world_type)` parses and caches it
(one `OnceLock` per variant — the set is small and fixed, so this stays
simpler than a keyed map). `overworld_generator_of_type(seed, world_type)` and
`overworld_chunk_source_of_type(seed, world_type)` build a generator/chunk
source from that settings document; `overworld_generator`/
`overworld_chunk_source` are unchanged in signature and behavior — they call
the `_of_type` functions with `WorldType::Overworld`, so every pre-existing
caller (the shell's singleplayer path, the integrated server) is unaffected.

No new engine code was needed. `OverworldGenerator::new` already takes an
arbitrary `settings: &Value` and builds its density-function tree and
`ClimateSampler` from it per call, and `EmbeddedResolver::density_function`
already resolves any dotted id under `density_function/` — `amplified.json`'s
`noise_router` referencing `minecraft:overworld_amplified/depth` resolves the
same way `overworld.json` referencing `minecraft:overworld/depth` always has.
Both presets' own `world_preset/{amplified,large_biomes}.json` select
`biome_source.preset: "minecraft:overworld"`, so
`EmbeddedResolver::biome_parameters`'s hardcoded `biome_parameters/overworld`
table is the *correct* table for them, not a stand-in — their biome-size
difference comes entirely from `noise_settings/large_biomes.json`'s own
`temperature`/`vegetation` router entries pointing at `noise/temperature_large`
and `noise/vegetation_large` instead of the plain noises, which
`ClimateSampler::new(settings, &builder)` already builds fresh per generator.
So selecting a `WorldType` really was the entire gap for these two presets.

Verified, not just wired — `crates/lodestone-server/tests/world_type_selection.rs`:

* At seed 4242, chunk `(0, 0)`, local column `(0, 0)`: `Overworld` yields
  top-non-air `y = 64`; `Amplified`, at the *same* seed and column, yields
  `y = 130`. A silent fallback to the default settings would make these equal.
* Over a 120-chunk strip at the same seed: `Overworld`'s `biome_state(0, 0)`
  changes 20 times across 12 distinct biomes; `LargeBiomes`' changes once
  across 2 — an order of magnitude sparser, the statistic large-biomes worlds
  exist to produce and a height check cannot see.

## How to change it

* **Add a preset that needs no new engine** (there are none left in the
  bundled data — every other `noise_settings`/`world_preset` document needs a
  generator or biome source this tree does not have; see below): add a
  `WorldType` variant, a `settings_asset` arm, an `OnceLock` in
  `settings_for`, and a discriminating test following
  `world_type_selection.rs`'s pattern — pick a statistic the new preset
  changes by construction and confirm what the default arm produces at the
  same input first.
* **Add a preset that needs new engine work** — do not add a `WorldType`
  variant until the generator exists. A selection that silently produces
  ordinary overworld terrain under a preset's name is worse than the preset
  being absent (it reads as a generation bug, not a missing feature). What is
  still missing, per preset:
  * `single_biome_surface` needs a `FixedBiomeSource` — absent (`grep
    FixedBiomeSource crates/**/src` → 0 hits). `OverworldGenerator` already has
    a fixed-biome *fallback* path (`dynamic_biome: None`, used when a resolver
    supplies no biome-parameter table at all), but selecting a fixed biome
    deliberately, from a resolver that *does* supply real biome parameters
    otherwise, is unbuilt.
  * `flat`/`flat_all_dimensions` need a `FlatLevelSource`-style generator — a
    `ChunkSource` distinct from `OverworldGenerator` entirely, since a
    superflat column is a constant layer stack, not a sampled density field.
  * `debug_all_block_states` needs a special block-grid generator.
  * `CheckerboardColumnBiomeSource`, needed by the debug/custom presets, is
    also absent.
  * A per-preset or per-dimension `Resolver::biome_parameters` (it currently
    takes no argument) is a **separate** blocker from any of the above — it
    matters for a Nether/custom biome source, not for `Amplified`/
    `LargeBiomes`, whose own `world_preset` documents point at the ordinary
    overworld biome table (see "How it works").
* **The `world_preset/*.json` documents themselves are still unparsed.**
  `WorldType`'s variants name a `noise_settings` id directly in Rust; nothing
  resolves a preset id to its `generator.settings` field the way vanilla's
  `WorldPresets` registry does. A world-creation UI therefore cannot hand this
  module an arbitrary preset id yet — it picks a `WorldType` variant.

## Configuration

No runtime configuration. `WorldType::default()` is `Overworld`. A
world-creation UI (`crates/lodestone-shell/src/menu/create_world.rs`, owned
elsewhere) needs to: offer `Amplified`/`LargeBiomes` as choices, persist the
chosen `WorldType` alongside the seed the same way world metadata already
persists a seed, and call `overworld_generator_of_type`/
`overworld_chunk_source_of_type` (re-exported from `lodestone_server`) instead
of the `Overworld`-only `overworld_generator`/`overworld_chunk_source` at
world-open time.

## Dependencies

* `crates/lodestone-server/assets/worldgen/noise_settings/{amplified,large_biomes}.json`
  and `density_function/overworld_{amplified,large_biomes}/*` — bundled by
  `build.rs`, see [`worldgen-dimensions.md`](./worldgen-dimensions.md) for how
  the embedding works.
* `lodestone_worldgen::overworld::OverworldGenerator` — no changes needed; see
  "How it works" for why.
* [`worldgen-gap-census.md`](./worldgen-gap-census.md) §1 — the fuller
  inventory of which world types are reached, and what each remaining one
  needs.
