# Biome mob-spawn settings: the `spawners` / `spawn_costs` parse

Issue [#518](https://github.com/matteopolak/lodestone/issues/518)'s part 1 — the parse this
document describes. Parts 2-4 (the `SPAWN` generation stage itself, the light/ground
re-validation, and the persistence decision) have since landed too — see
[`docs/worldgen-mob-generation-spawn.md`](./worldgen-mob-generation-spawn.md).

## What it is

`crates/lodestone-worldgen/src/spawners.rs` parses the `spawners` and
`spawn_costs` fields every one of 26.2's 66 bundled biome documents carries.
Until it existed, **nothing in the workspace read either field** — the data
shipped in `crates/lodestone-server/assets/worldgen/biome/*.json` and was named by
no line of code. `OverworldGenerator::biome_spawners(biome)` exposes the per-biome
answer.

## How it works

`parse_biome_spawners(document)` reads a whole biome document — the same
`Resolver::biome_document` value `feature::top_layer::parse_biome_climate` already
consumes, so it costs no extra JSON parse — and yields a `BiomeSpawners`:

| type | vanilla | shape |
|---|---|---|
| `MobCategory` | `MobCategory.java:7-14` | 8 variants, in declaration order, with `key()` and `max_instances()` |
| `SpawnerEntry` | `MobSpawnSettings.java:108` + `:33` | `entity_type`, `weight`, `min_count`, `max_count` |
| `MobSpawnCost` | `MobSpawnSettings.java:101` | `energy_budget`, `charge` |

The generator resolves it once per biome at construction, in the same
`biome_names` walk that already builds `carvers_by_biome`, `ores_by_biome`,
`vegetation_by_biome`, `biome_climates` and `freeze_biomes`. A biome with no
spawner entry and no spawn cost is **absent** from the map rather than stored
empty, so `biome_spawners` returning `None` means "this biome declares nothing" —
the same "missing data means do nothing, never assume" convention every other
resolver-fed table in the crate follows, and why every fixture `Resolver` keeps
working unchanged.

Measured across the 66 bundled documents: 795 spawner entries total, non-empty in
`monster` 63 biomes, `ambient` 54, `underground_water_creature` 53, `creature` 43,
`water_ambient` 13, `water_creature` 11, `axolotls` 1, and `misc` **0**.
`spawn_costs` is non-empty in exactly 5 (all Nether).

## This is data, not behaviour — and why that was the right cut

Parts 2–4 of #518 are not here, and the reasoning is on the issue rather than
being rediscovered:

* **The `SPAWN` stage** (`spawnMobsForChunkGeneration`) is gated on
  `SpawnPlacements.checkSpawnRules(…, CHUNK_GENERATION)`, which lives in
  `crates/lodestone-entity/src/spawn.rs`. Porting a second copy inside worldgen
  would create exactly the duplicate part 3 exists to resolve.
* **Most `Creature` spawn rules consult block light, and the server computes
  none.** A port that quietly treats light as 0 or 15 passes its own tests and
  places mobs in the wrong places — the *world* species of vacuous test from
  `CLAUDE.md`'s table, where the flaw is in the input data and cannot be read off
  the test source.
* **There is no entity slot and no entity persistence anywhere**
  (`grep '"Entities"\|entity_nbt'` over `lodestone-server/src` and
  `lodestone-anvil/src` is still 0 hits), so a chunk-generation spawn would be
  lost on unload and re-run on regeneration. Choosing between "accept re-running"
  and "land persistence" is a decision, not an implementation detail.

So by this repo's own island rule the parse is an island until a runtime spawner
exists. It landed anyway because a bundled asset field that no code can even name
is strictly worse, and because the parse is the one part of #518 whose correctness
settles against the record definition rather than against a simulation.

## How to change it, and the gotchas

* **`MobCategory::parse` panics on an unknown key.** Same reason
  `TemperatureModifier::parse` does: a silently-dropped category is a whole
  missing mob class, and it would read as a subtle spawn-rate residual rather than
  a missing port.
* **`weight` is a `WeightedList` weight, not a `SpawnerData` field.**
  `record SpawnerData(EntityType<?> type, int minCount, int maxCount)`
  (`MobSpawnSettings.java:108`); the `weight` key belongs to the
  `WeightedList.codec` wrapper one level out (`:33`). `SpawnerEntry` flattens the
  two because nothing here needs the distinction, but a port of vanilla's weighted
  pick must read it as the **list** weight.
* **`MobSpawnCost`'s record order is `(energyBudget, charge)` while the JSON keys
  are alphabetical (`charge` first).** Both are read by name here, so the order is
  inert — recorded because transcribing a positional record from a JSON sample is
  exactly how vanilla's `DepthStencilState(…, 1.0F, 10.0F)` got reversed once.
* **Two vanilla behaviours are deliberately not modelled**, both named in the
  module doc: `SpawnerData`'s compact constructor rewriting a `MISC`-category
  entity type to `EntityTypes.PIG` (`:123-125`, needs an entity-type -> category
  table that belongs in `lodestone-entity`, and is unreachable from 26.2's own
  data since every `misc` list is empty), and `ExtraCodecs.POSITIVE_INT` /
  `minCount <= maxCount` validation (these are embedded generated assets, so a
  violation is a build-time defect rather than untrusted input).
* **A consumer wanting per-category iteration order** should use
  `MobCategory::ALL`, which is vanilla's declaration order — also `MobCategory.CODEC`'s
  key order and the order `Util.makeEnumMap` iterates. The internal map is a
  `BTreeMap` keyed by the enum, so *its* iteration order is variant order, which
  happens to be the same thing; do not rely on that coincidence, use `ALL`.

## Configuration

None. No feature gate, no env var. The parse runs for every biome name the
generator's biome table can produce, at construction.

## Dependencies

`serde_json`, and `density::Resolver::biome_document` for the input. Nothing in
the module reads noise, RNG or the block grid.
