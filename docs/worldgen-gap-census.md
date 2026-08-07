# World-generation gap census against vanilla 26.2

## What it is

An evidenced inventory of what our world generation actually does, per axis, measured
against vanilla 26.2 — dimensions, biomes, surface rules, carvers, ores and ore veins,
decoration steps, structures, slime chunks, mob spawning, heightmaps, light, block
entities and the seed's derivation. Every row carries a verdict (**reached** /
**orphaned** / **partial** / **absent**), the vanilla source of truth, and a `file:line`
citation, so a later reader can re-verify rather than inherit. It exists because the
written record here goes stale silently and because this repo's dominant defect is the
*island* — code that is built, unit-tested green, and reached by nothing.

**Method.** Measured at `98433351` (2026-08-07) by grep and by reading the tree; vanilla
claims read from the de-obfuscated `.cache/mc/26.2/src/` and from the bundled data under
`crates/lodestone-server/assets/worldgen/`. No number here is quoted from a doc or an issue
body. Where a claim rests on absence, the grep that found nothing is named so the detector
can be re-run.

**The two headline findings.**

1. **The data is not the gap; the engine is.** The bundle is essentially complete —
   66 biomes, 226/262 configured/placed features, 35 density functions, 63 noises, all
   7 `noise_settings`, 34 structures, 20 structure sets, 188 template pools, 40 processor
   lists, 1212 NBT templates, 7 world presets, 9 flat presets. Most of it is consumed by
   **nothing but its own drift gate**.
2. **Three gaps are parity defects in something we already ship**, which is worse than an
   absence because the feature looks done: ore veins (bundled `ore_veins_enabled: true`,
   zero engine consumers), heightmaps (we encode an *empty* heightmap NBT while a
   JVM-proven `MOTION_BLOCKING` sits unreachable), and light (we encode all-`Missing`
   while a ported `LightEngine` sits unreachable from the server).

---

## Verdict key

| verdict | meaning |
|---|---|
| **reached** | implemented, and a non-test production caller drives it |
| **orphaned** | implemented, but every caller outside the crate's own tests is a test |
| **partial** | some of it exists; the row says which parts |
| **absent** | no implementation |

The single production path is `OverworldGenerator::column`
(`crates/lodestone-worldgen/src/overworld/mod.rs`), built by
`overworld_generator(seed)` at `crates/lodestone-server/src/worldgen_data.rs:365`, wrapped
as `OverworldChunkSource`, and encoded by `encode_column_body` at
`crates/protocol/v770/src/server_protocol.rs:1460` — the only `ServerProtocol` impl. The
shell also calls the generator directly for singleplayer
(`crates/lodestone-shell/src/worldgen.rs:78`). Anything not reached from one of those two
is orphaned.

---

## 1. Dimensions and world types

Vanilla source of truth: `net/minecraft/world/level/levelgen/NoiseGeneratorSettings.java`,
`presets/WorldPresets.java`, `level/biome/{MultiNoise,TheEnd,Fixed,CheckerboardColumn}BiomeSource.java`.

| item | verdict | evidence |
|---|---|---|
| Overworld terrain generator | **reached** | `OverworldGenerator::column_timed` runs 10 stages: aquifer, shape, biome, surface, materialize, carve, ore, vegetation, top-layer, intern (`overworld/mod.rs`, `column_timed`) |
| Nether generator | **absent** | `grep -rni nether crates/lodestone-worldgen crates/lodestone-worldgen-core --include=*.rs` → **0 hits**. There is exactly one generator type in the tree |
| End generator | **absent** | same grep; no `TheEndBiomeSource`, no `end_islands` |
| `minecraft:end_islands` density type | **absent** | not a `Density` variant (`lodestone-worldgen-core/src/density/mod.rs`, enum `Density`); `density/mod.rs:916` `panic!("unhandled density-function type…")`. Used by bundled `noise_settings/end.json` **and** `density_function/end/sloped_cheese.json` — two uses, so loading End data panics today |
| non-overworld `noise_settings` | **orphaned (data)** | all 7 bundled (`assets/worldgen/noise_settings/{overworld,nether,end,amplified,caves,floating_islands,large_biomes}.json`); production loads exactly one — `worldgen_data.rs:353` `EmbeddedResolver.raw("noise_settings/overworld")`. The other 6 are referenced only from `crates/lodestone-data/tests/worldgen_dimension_data.rs` |
| `legacy_random_source` (Nether/End gate) | **absent** | 3 tree-wide hits, all in `crates/lodestone-data/tests/worldgen_dimension_data.rs`. Both non-overworld settings set it `true`; `overworld.json` sets it `false`. Tracked as **#486** |
| world presets (7) + flat presets (9) | **orphaned (data)** | bundled; the only Rust references are `crates/lodestone-server/tests/worldgen_structure_corpus.rs:73,84`. No generator selects a preset |
| amplified / large_biomes | **absent (engine wiring)** | their density functions *and* `noise_settings` are bundled and byte-identical; nothing can choose them because `overworld_settings()` (`worldgen_data.rs:350`) is a hardcoded `OnceLock` |
| superflat generator | **absent** | no `FlatLevelSource` analogue; grep `flat_level_generator\|superflat` in `crates/**/src` → hits are only light-propagation prose |
| debug-world generator | **absent** | no implementation |
| `FixedBiomeSource` / `CheckerboardColumnBiomeSource` | **absent** | grep → 0 hits. Needed by single-biome and debug presets |
| server multi-dimension plumbing | **absent** | Anvil paths hardcode `dimensions/minecraft/overworld` (`lodestone-server/src/region_source.rs:91,400`); tracked as **#330** |

**Also found:** `Resolver::biome_parameters(&self)` (`density/mod.rs:228`) takes **no
dimension argument**, so `assets/worldgen/biome_parameters/nether.json` — which cost a
bespoke JVM oracle to produce — is structurally unreachable: `EmbeddedResolver`'s override
hardcodes `biome_parameters/overworld`. A Nether biome source needs this trait method
widened, which #485 does not mention.

**Also found:** vanilla registers **three** carvers; we implement two (below). The missing
one is `nether_cave`, which `CarverConfig::parse` handles by
`panic!("unsupported carver type")` (`carver/mod.rs:234`). #485's engine list does not
name it.

---

## 2. Biomes

Vanilla source of truth: `level/biome/{Climate,MultiNoiseBiomeSource}.java`,
`level/levelgen/NoiseRouterData.java`.

| item | verdict | evidence |
|---|---|---|
| Multi-noise climate sampling (7 channels) | **reached** | `crates/lodestone-worldgen/src/biome/mod.rs`, driven from `overworld/biome.rs:41` `biome_stage` |
| `Climate.RTree` search | **reached** | `biome/tree.rs` (717 lines, literal port); landed `7ff942dd` / `71dd8b22`. Issue **#491 is stale-open** |
| per-source-chunk biome memo | **reached** | `biome/memo.rs` |
| **3-D biome sampling (4×4×4 quart cells)** | **absent** | `biome_stage` returns `[(String, bool); 16]` — 16 quart *columns*, one sample each at that quart's own surface height, **broadcast vertically**. `GeneratedColumn::biome_quarts: [String; 16]` (`overworld/output.rs:142`) and the encoder resolves 16 ids once and reuses them for every section (`server_protocol.rs:1393-1401`). Vanilla stores 16 × (height/4) cells. Consequence: **no cave biome can ever appear** — lush caves, dripstone caves and deep dark are unreachable at any depth |
| biome-specific surface rules | **reached** | `Cond::Biome` at `surface/mod.rs:1016`, driven per column |
| `TheEndBiomeSource` / `FixedBiomeSource` / `CheckerboardColumnBiomeSource` | **absent** | see §1 |
| biome wire ids | **partial, known-divergent** | `server_protocol.rs:233-256` uses a *sorted* 55-entry local id space, not vanilla's registration order, because `worldgen/biome` is not in the configuration-phase registry sync (**#275**). Documented in place, not a hidden gap |

---

## 3. Surface rules

Vanilla source of truth: `level/levelgen/SurfaceRules.java:265-275` (conditions) and
`:650-653` (rule sources).

**Type coverage is complete — 11/11 conditions, 4/4 rules.** Measured as a set difference,
not eyeballed:

| | vanilla | ours |
|---|---|---|
| rule sources | `bandlands`, `block`, `sequence`, `condition` | all 4 — `surface/mod.rs:945,949,957,961` |
| conditions | `biome`, `noise_threshold`, `vertical_gradient`, `y_above`, `water`, `temperature`, `steep`, `not`, `hole`, `above_preliminary_surface`, `stone_depth` | all 11 — `surface/mod.rs:1015-1073` |

Verdict: **reached**, and the only surface-rule axis in this census with no gap. Unknown
types panic loudly rather than degrading (`surface/mod.rs:962,1081`), which is the right
failure mode. Per-dimension surface rules therefore need **no new condition type** — the
Nether's rules are a strict subset (independently confirmed in #485).

---

## 4. Carvers

Vanilla source of truth: `level/levelgen/carver/WorldCarver.java:32-34`.

| carver | verdict | evidence |
|---|---|---|
| `cave` | **reached** | `carver/mod.rs:202`, `CaveConfig::carve` at `:482`, driven from `carve_stage` |
| `canyon` | **reached** | `carver/mod.rs:213`, `CanyonConfig` at `:696` |
| `nether_cave` | **absent** | `carver/mod.rs:234` `panic!("unsupported carver type")`. `configured_carver/nether_cave.json` is bundled (4/4 configured carvers) and unreferenced by any overworld biome, so the panic is latent, not live |

All 4 `configured_carver` documents are bundled; carver composition is per-biome through
`compose.rs:65` `build_biome_carvers`, in the biome JSON's own array order (which
`set_large_feature_seed`'s index depends on).

---

## 5. Ores and ore veins

Vanilla source of truth: `level/levelgen/feature/OreFeature.java`,
`level/levelgen/OreVeinifier.java`.

| item | verdict | evidence |
|---|---|---|
| `minecraft:ore` feature | **reached** | `feature/mod.rs:553` `parse_ore_config`, driven by `ore_stage` over the real 3×3 neighbourhood |
| per-biome ore lists from step 6 | **reached** | `compose.rs:88` `build_biome_ores`, index-preserving |
| `minecraft:scattered_ore` | **absent** | `build_biome_ores` skips any configured feature whose `type != "minecraft:ore"` (`compose.rs`, the `continue` on the type check). 2 bundled files (Nether gold/quartz-shaped placement) |
| **`OreVeinifier` (large copper/iron veins)** | **absent — live Overworld parity defect** | `grep -rn "vein_toggle\|vein_ridged\|vein_gap\|OreVein\|ore_vein\|veinif" --include=*.rs crates/` → **1 hit**, in `crates/lodestone-data/tests/worldgen_dimension_data.rs:311`. Bundled `noise_settings/overworld.json` has `ore_veins_enabled: true` and all three router channels plus the `vein_a`/`vein_b` noises. Tracked as **#496** (half one) |

**#496's second half already landed** (`a27cbb98`, plus U17/U18 `d50feba7`/`22982b99`
addressing the same hashing/allocation costs). The issue title bundles both halves, so it
reads as untouched work when only the `OreVeinifier` remains.

---

## 6. Decoration / feature steps

Vanilla source of truth: `level/levelgen/GenerationStep.java` — 11 `Decoration` steps,
indices 0–10.

**We run 3 of 11.** Measured from the step constants, which are the indices
`build_biome_*` reads out of each biome JSON's `features` array:

| # | vanilla step | ours |
|---|---|---|
| 0 | `RAW_GENERATION` | absent |
| 1 | `LAKES` | absent |
| 2 | `LOCAL_MODIFICATIONS` | absent (geodes, dripstone, amethyst) |
| 3 | `UNDERGROUND_STRUCTURES` | absent (mineshafts, dungeons, fossils) |
| 4 | `SURFACE_STRUCTURES` | absent |
| 5 | `STRONGHOLDS` | absent |
| 6 | `UNDERGROUND_ORES` | **reached** — `feature/mod.rs:82` |
| 7 | `UNDERGROUND_DECORATION` | absent (glow lichen, sculk) |
| 8 | `FLUID_SPRINGS` | absent (water/lava springs) |
| 9 | `VEGETAL_DECORATION` | **reached** — `feature/mod.rs:92` |
| 10 | `TOP_LAYER_MODIFICATION` | **reached** — `feature/top_layer.rs:113` |

**Feature-type coverage, measured against the bundle rather than recalled.** The 226
bundled `configured_feature` documents use **55 distinct types**. The engine implements:

- `ore` (`feature/mod.rs:553`) — 30 of the 226 files
- `freeze_top_layer` (`feature/top_layer.rs`) — 1
- `simple_block`, `tree`, `block_column`, `random_selector`, `simple_random_selector`
  (`feature/vegetation/config.rs:1018-1068`) — 32 + 39 + 4 + 21 + 5

**Everything else — 48 of 55 types — falls to `ConfiguredFeature::Unsupported`
(`config.rs:1068`) and silently places nothing.** That silence is deliberately made
*legible* rather than hidden: `collect_unsupported` (`config.rs:1085`) walks the resolved
tree and `worldgen_data.rs`'s `vegetation_placer_gaps_are_named_not_silent` diffs it
against an allow-list. So this is a measured gap with a live instrument, not a blind spot.

**Placement-modifier coverage: 10 of vanilla's 15** (`PlacementModifierType.java`), split
across two independent engines that share no instances:

| engine | modifiers |
|---|---|
| ore (`feature/mod.rs:352`) | `count`, `rarity_filter`, `in_square`, `height_range`, `biome` — unknown types `panic!` at `:371` |
| vegetation (`feature/vegetation/config.rs:511`) | `count`, `in_square`, `heightmap`, `biome`, `rarity_filter`, `surface_water_depth_filter`, `noise_threshold_count`, `random_offset`, `block_predicate_filter` — unknown types return `None` |

Absent from both: `count_on_every_layer`, `environment_scan`, `fixed_placement`,
`noise_based_count`, `surface_relative_threshold_filter`.

Also open on this axis: **#428** (fancy/giant trunk placers — jungle, mangrove, cherry,
`FallenTreeFeature`).

---

## 7. Structures

Vanilla source of truth: `level/levelgen/structure/`, `structure/pools/`,
`structure/placement/`, `chunk/status/ChunkStatus.java`.

| item | verdict | evidence |
|---|---|---|
| structure **data** corpus | **orphaned (data)** | 34 structures, 20 structure sets, 188 template pools, 40 processor lists, 1212 `.nbt` templates, 92 worldgen tags — all bundled and byte-identical to the jar (`docs/worldgen-structure-corpus.md`; landed `6c6c0e10` under **#484**, which is **stale-open**). The **only** Rust reader is the drift gate `crates/lodestone-server/tests/worldgen_structure_corpus.rs` |
| structure placement / `/locate` (S1) | **absent** | no `structure` module in `crates/lodestone-worldgen/src`; `lib.rs`'s module list is aquifer, biome, carver, compose, dense_grid, feature, interner, overworld, surface |
| template placement + processors (S2) | **absent** | nothing reads `assets/structure/**.nbt` |
| **beardifier (S3)** | **partial — a constant-zero leaf** | `Density::Beardifier` parses (`density/mod.rs:826`) and evaluates to `0.0` (`density/mod.rs:599`). So the density graph has the seam and no terrain adaptation: structures would sit on unmodified terrain |
| jigsaw (S4) | **absent** | — |
| `WorldgenRandom.setLargeFeatureWithSalt` | **absent** | `grep large_feature_with_salt --include=*.rs crates/` → 0 hits. Vanilla `WorldgenRandom.java:66`; it is how `RandomSpreadStructurePlacement` salts per-structure-set placement, so S1 needs it |
| ChunkStatus pipeline (S0 prerequisite) | **absent** | no `ChunkStatus` type anywhere; generation is one `column()` call with bare integer step constants. Vanilla's order (`ChunkStatus.java`) runs `STRUCTURE_STARTS` **before** `NOISE` so the beardifier can consult it. Adjacent open issue: **#289** |

The generator's own module doc still says so: `overworld/mod.rs:80` — "**Still not
composed:** structures (unbuilt anywhere in this repo)".

---

## 8. Slime chunks

Vanilla source of truth: `WorldgenRandom.seedSlimeChunk` (`WorldgenRandom.java:71`) and
`entity/monster/cubemob/Slime.java:93`:

```java
boolean slimeChunk = WorldgenRandom.seedSlimeChunk(
    chunkPos.x(), chunkPos.z(), worldGenLevel.getSeed(), 987234911L).nextInt(10) == 0;
if (random.nextInt(10) == 0 && slimeChunk && pos.getY() < 40) { … }
```

`seedSlimeChunk` returns `RandomSource.createThreadLocalInstance(seed + x*x*4987142 +
x*5947611 + z*z*4392871L + z*389711 ^ salt)` — a **legacy `java.util.Random` LCG**, salt
`987234911L`.

**Verdict: absent.** `grep -rn 987234911 --include=*.rs --include=*.json .` → **2 hits,
both in `.cache/mc/26.2/{src,client-src}/…/Slime.java`** — i.e. only the decompiled jar.
`grep -rni slime --include=*.rs crates/` → 150 hits, and **none** is a slime chunk: every
one is `slime_block` physics/rendering/brewing or the slime *entity* model. Nor does
`WorldgenRandom` carry the derivation (`lodestone-worldgen-core/src/rng/mod.rs` has
`set_decoration_seed:124`, `set_feature_seed:136`, `set_large_feature_seed:144` and nothing
else).

This one is worth naming as cheap: it is a **pure function of `(chunk_x, chunk_z, seed)`**
over primitives we already have (`LegacyRandomSource` at `rng/legacy.rs:16`), so it is
*exactly checkable* against a JVM oracle — bit-exact or wrong, no tolerance discussion
available. Its consumer (slime spawning) is blocked on the spawning gaps in §9, but the
predicate itself is not blocked on anything.

---

## 9. Mob spawning

Vanilla sources of truth: `chunk/status/ChunkStatus.java` (`SPAWN`),
`chunk/status/ChunkStatusTasks.java:170`, `NaturalSpawner.java:362`
(`spawnMobsForChunkGeneration`), `entity/SpawnPlacements.java`.

| item | verdict | evidence |
|---|---|---|
| worldgen-time `SPAWN` step (initial animal population) | **absent** | zero occurrences of `spawners`, `spawn_costs`, `SpawnPlacement`, `MobCategory`-from-biome anywhere in `crates/**/src`. The data **is** bundled and unread: `assets/worldgen/biome/forest.json` carries `spawners` (sheep/pig/chicken/cow with weight/min/max) and `spawn_costs`; `EmbeddedResolver::biome_document` (`worldgen_data.rs:179`) is consumed only for `carvers` and `features[6]`/`features[9]` (`compose.rs:70,93,147`) |
| runtime natural spawning | **orphaned** | `crates/lodestone-server/src/mob_spawn.rs` (660 lines) is a faithful cap/despawn engine — `MAGIC_NUMBER = 289` at `:97`, caps `70/10/15/5/5/5/20` at `:105`, `check_despawn` at `:303`. The only `impl SpawnCandidateSource` is a test mock (`tests/mob_spawn.rs:32`); `run_spawn_cycle` (`mobs.rs:2640`) and `despawn_pass` (`mobs.rs:2594`) have **zero production callers**; `crates/lodestone-server/src/tick.rs`'s `run_tick_loop` (`:711`) never calls them. Tracked as **#221** / **#222** |
| second, independent orphan | **orphaned** | `crates/lodestone-entity/src/spawn.rs` (442 lines) duplicates the same engine *plus* `SpawnConditions::permits` (`:220`) — the nearest thing to `SpawnPlacements.checkSpawnRules`. `grep DespawnCtx\|SpawnConditions\|SpawnSample\|mob_cap` outside `crates/lodestone-entity/` → 0 hits |
| what actually puts mobs in the world | **reached (a demo roster)** | `seed_demo_mobs` (`mobs.rs:3097`) places `mob_count = 6` mobs in a radius-6 ring around `(8,8)`, one per `DEMO_SPECIES` entry (`mobs.rs:3177`), called once from `MobHandle::reseed` (`mobs.rs:3035`) at `integrated.rs:611`. No `/summon` (every production `CommandDispatch` is `::none()`), no spawn eggs, no spawner blocks (**#224**), no entity persistence, no mobs at all on LAN or wasm |

---

## 10. Heightmaps

Vanilla source of truth: `level/levelgen/Heightmap.java` — six types, four of them
persisted and sent.

**Verdict: partial, and a parity defect in something we ship.**

- **On the wire: empty.** `server_protocol.rs:1465` encodes `Heightmaps::new()` — a default,
  zero-entry `Vec<(u32, Heightmap)>` (`crates/lodestone-world/src/heightmap.rs:110`). Valid
  framing, no data. The function's own doc says so at `server_protocol.rs:1454`.
- **Not persisted.** `crates/lodestone-server/src/chunk_nbt.rs:43` — deliberately omitted,
  relying on vanilla's `Heightmap.primeHeightmaps`. A reasoned decision.
- **No storage.** `GeneratedColumn` (`overworld/output.rs:134`) and server-side
  `ChunkColumn` (`lodestone-server/src/chunk.rs:94`) have **no heightmap field**, so nothing
  could survive the generator → server → wire hop even if computed.
- **Four internal scans exist**, all consumed inside the generator only:
  `heights_from_field` (`overworld/fill.rs:127`, serving as *both* `WORLD_SURFACE_WG` and
  `OCEAN_FLOOR_WG`), `VegGrid::height_world_surface` (`feature/vegetation/grid.rs:408`),
  `VegGrid::height_ocean_floor` (`grid.rs:441`), and `motion_blocking_first_free`
  (`feature/top_layer.rs:586`, a real per-state predicate).
- **`MOTION_BLOCKING_NO_LEAVES` is collapsed onto `MOTION_BLOCKING`**
  (`feature/vegetation/config.rs:41`) with no leaf/log exclusion.
- **The island shape is explicit:** `GeneratedColumn::top_non_air_y` (`output.rs:173`),
  documented as matching `WORLD_SURFACE_WG`, has **no production caller** — only
  `worldgen_data.rs:422,1152,1969` inside `#[cfg(test)]`. And
  `motion_blocking_heightmap_matches_vanilla_per_column` (`worldgen_data.rs:2362`) proves a
  `MOTION_BLOCKING` against a JVM oracle that no shipped byte ever carries.

Incremental-vs-snapshot, since vegetation cost candidate 3 in the rewrite plan depends on
it: the veg-grid pair are **live rescans per query** against the already-mutated grid
(`grid.rs:402`), observationally equivalent to vanilla's incremental maintenance for in-pass
reads; `motion_blocking_first_free` is a one-shot post-decoration scan; `heights_from_field`
is a shape-stage snapshot never updated by decoration. **No stored heightmap is maintained
incrementally, because no heightmap is stored.**

---

## 11. Light

Vanilla source of truth: `level/lighting/{LightEngine,BlockLightEngine,SkyLightEngine}.java`,
and `ChunkStatus.INITIALIZE_LIGHT` / `LIGHT`.

**Verdict: orphaned relative to the server output path — a parity defect in something we
ship.**

- **The server sends nothing real.** `server_protocol.rs:1496` encodes
  `ColumnLight::new(section_count)`, which is `vec![LightData::Missing; section_count + 2]`
  for both sky and block (`crates/lodestone-world/src/light.rs:305`).
  `grep "sky_light\|block_light\|LightEngine" crates/lodestone-server/src` → **0 hits**.
  `LIGHT_UPDATE` (packet 48) exists only as a client *decode* arm
  (`crates/protocol/v770/src/adapter.rs:3718`); nothing encodes it.
- **No worldgen light stage.** The generator relies on this: `feature/top_layer.rs:77` —
  "Block light is not modelled, and does not need to be… `initialize_light` runs strictly
  after `features`" — and the block-light gate is hard-true at `top_layer.rs:677`. That
  argument is sound *for the top-layer stage* and says nothing about the served chunk.
- **A real engine exists and is a genuine port**: `crates/lodestone-world/src/lighting.rs`
  (1,105 lines) — descending-level 15-bucket propagation, `opacity = max(1, lightDampening)`,
  sky seeded per `ChunkSkyLightSources.isEdgeOccluded`, exposing `compute_column_light`,
  `compute_column_light_with_neighbours` and `diff_column_light`.
- **Its only production caller is the client's local world**, not the server:
  `crates/lodestone-shell/src/worldgen.rs:261`. So singleplayer-direct is lit and the
  integrated-server path is not.
- **Do not conflate with `docs/light-ramp.md`**, which documents the per-vertex *lightmap
  curve* in `crates/lodestone-render/src/light.rs` — client rendering of an
  already-known light byte, a different subsystem.

---

## 12. Block entities from worldgen

Vanilla: worldgen places block entities (structure chest loot, dungeon/mineshaft spawners,
trail-ruins decorated pots, bee nests with bees) and they travel in the chunk-data packet's
`block_entities` array.

**Verdict: absent.**

- No slot in the path: neither `GeneratedColumn` (`overworld/output.rs:134`) nor
  `ChunkColumn` (`lodestone-server/src/chunk.rs:94`) has a block-entity field, and
  `ChunkColumn::from_generated` (`chunk.rs:130`) moves only
  `(min_y, height, palette, blocks, biome_quarts)`.
- The wire array is a literal zero: `server_protocol.rs:1494`
  `w.var_i32(0); // block entities: none generated yet`.
- **The one shipped defect on this axis** is honest and in-source:
  `place_beehive_decorator` (`feature/vegetation/place.rs:375`) writes
  `minecraft:bee_nest[facing=south,honey_level=0]` (`:425`) and then at `:428`:
  *"Bee-entity storage (2-3 bees) is not modelled"* — `let _bee_count = 2 +
  random.next_int_bounded(2);`. The draw is consumed (so the RNG stream stays aligned, which
  is correct) and discarded, so **every generated bee nest reaches the client empty**.
- `docs/block-entities.md` is a different thing: four server-side tick *simulations*
  (composter, furnace, hopper, brewing) plus Anvil NBT round-trip. None is fed by the
  generator. Related but distinct: **#477** (loading a real vanilla world drops 1608 of
  1613 block entities).

---

## 13. Seed derivation

Vanilla source of truth: `level/levelgen/WorldgenRandom.java`, `RandomState.java`.

| derivation | verdict | evidence |
|---|---|---|
| `WorldgenRandom.next(bits)` legacy-shape wrapping | **reached** | `rng/mod.rs:95-108` — reproduces vanilla's "all draws use the legacy `BitRandomSource` structure even when the wrapped source is xoroshiro", which is load-bearing (naive delegation diverged, and did) |
| `setDecorationSeed` | **reached** | `rng/mod.rs:124` |
| `setFeatureSeed` | **reached** | `rng/mod.rs:136` — per-feature stream isolation, which is why adding a feature cannot desync its neighbours |
| `setLargeFeatureSeed` | **reached** | `rng/mod.rs:144` — carvers |
| `setLargeFeatureWithSalt` | **absent** | 0 hits; needed by structure-set placement (§7) |
| `seedSlimeChunk` | **absent** | 0 hits (§8) |
| `RandomState`'s algorithm switch on `legacy_random_source` | **absent** | `density/mod.rs:708` hardcodes the Xoroshiro branch; **#486** |
| `NormalNoise.createLegacyNetherBiome` (raw-seed, non-positional) | **absent** | `PerlinNoise::new_legacy` is private and blended-noise-only; no `NormalNoise` legacy-init path (recorded in #485/#486) |
| positional forks for noise / aquifer / ore RNG | **reached** | `Builder::positional_factory` (`density/mod.rs:750`), consumed by the noise instantiation and the carve/ore drivers |

---

## Ranked by player-visible impact

Ranking is about what a player would notice standing in the world, not about cost.

| rank | gap | why it ranks here | verdict |
|---|---|---|---|
| 1 | **No Nether, no End** | two of three dimensions do not exist; the whole late game is unreachable | absent (§1) |
| 2 | **No structures** | villages, mineshafts, strongholds, monuments, temples, trial chambers — the entire exploration layer, and the corpus is already on disk | absent engine / orphaned data (§7) |
| 3 | **8 of 11 decoration steps, 48 of 55 feature types** | no lakes, springs, geodes, dripstone, icebergs, disks, dungeons, fossils, glow lichen, sculk, coral. The world reads as terrain + ore + grass/trees + snow | absent (§6) |
| 4 | **No mob spawning** | a world with six demo mobs and no others, ever; no cap, no despawn, no night hostiles | absent + orphaned (§9) |
| 5 | **All-`Missing` light on the served chunk** | the integrated-server path ships no light while a working engine sits one crate away | orphaned — **parity defect** (§11) |
| 6 | **No 3-D biomes** | lush caves / dripstone caves / deep dark can never appear; underground biome tint, fog and future spawn inputs are the surface biome's | absent (§2) |
| 7 | **Ore veins never generate** | large copper and iron veins are simply missing from a world we otherwise call Overworld-parity | absent — **parity defect** (§5) |
| 8 | **Empty heightmap NBT** | not directly visible, but it is a wrong value in a field we populate, and a JVM-proven `MOTION_BLOCKING` exists and is unreachable | partial — **parity defect** (§10) |
| 9 | **Slime chunks** | one predicate; blocks slime spawning, and a well-known player-facing mechanic | absent (§8) |
| 10 | **World presets / amplified / large_biomes / superflat / single-biome / debug** | player-selectable world types, all data-complete and unreachable | absent wiring (§1) |
| 11 | **Empty bee nests** | small, visible, and a value we already ship wrong | absent (§12) |
| 12 | **`nether_cave` carver, `scattered_ore`** | latent until the Nether exists | absent (§4, §5) |

**The parity-defect set — worse than absences, because they look done:** ore veins (§5),
heightmaps (§10), light (§11), empty bee nests (§12). Each is a field or a world we already
populate, populated wrongly, inside a subsystem whose gates are green.

---

## Issue coverage

Checked with `gh issue list --search` before filing; nine gaps had no issue and now do.

| gap | issue | note |
|---|---|---|
| Nether/End engine | **#485** | data phase landed; engine half open. Its list omits `nether_cave` and the `Resolver::biome_parameters` widening |
| `legacy_random_source` | **#486** | accurate at HEAD; gates all Nether/End noise |
| ore veins (`OreVeinifier`) | **#496** | **half-landed** — the hash-lookup half shipped, the veinifier did not |
| 3-D biome sampling | **#512** | filed |
| decoration steps + feature/placement-type coverage | **#513** | filed |
| structure engine (S0–S4) | **#514** | filed; #484 was data only |
| slime chunks | **#515** | filed |
| heightmaps | **#516** | filed |
| server light | **#517** | filed |
| worldgen-time `SPAWN` step | **#518** | filed; sibling of #221/#222, different mechanism |
| world presets / alternate generators | **#519** | filed |
| block entities from worldgen | **#520** | filed |
| runtime spawn rules / spawn cycle / spawner blocks | **#221** / **#222** / **#224** | pre-existing, accurate |
| multi-dimension travel | **#330** | pre-existing; gameplay, not worldgen |
| ChunkStatus / ticket pipeline | **#289** | pre-existing; shared prerequisite of #514 and #518 |
| tree trunk/foliage placers | **#428** | pre-existing, a subset of #513 |
| vanilla-world block entities dropped on load | **#477** | pre-existing; distinct from #520 |

---

## Stale claims found while auditing (reported, not edited)

These are open GitHub issues whose work has **landed**, verified with `git log --grep`:

| issue | landed as |
|---|---|
| **#482** U1+U2 benchmark harness + baseline | `b3989ed1`, `939a821e`, `e2c90c72` |
| **#483** U3 numeric block-state ids | `70ce8521` |
| **#484** structure corpus (S-data) | `6c6c0e10`, `5474d59f`, `4b8a5d7f` |
| **#488** U16 decomposition (all 3 phases) | `f1ec116a`, `091601e0`, `4aa7ac85` |
| **#489** U6 staged sharded store | `34202a21`, `9c4f0967` |
| **#491** U9 biome layer + RTree | `7ff942dd`, `71dd8b22` |
| **#494** U10 join scheduler | `7ba0176b`, `0a3ede8d` |

**#496 is half-landed and its title hides that**: "U15: ore-vein system (OreVeinifier) +
the ore stage's hash-lookup cost" — the hash-lookup half landed (`a27cbb98`, with U17/U18
`d50feba7`/`22982b99` on the same costs); the `OreVeinifier` has not. Its body is accurate
and does say "two things at once".

**#485's engine list is incomplete**, not wrong: it enumerates seven engine items for
Nether/End and omits (a) the `nether_cave` carver, which `carver/mod.rs:234` panics on, and
(b) that `Resolver::biome_parameters(&self)` (`density/mod.rs:228`) has no dimension
argument, so the Nether parameter list it landed is structurally unreachable until the trait
widens.

**`docs/live-mob-sim.md`** has a stale architecture diagram (lines 23–30): it puts
`ChunkWorld::from_source` and `seed_demo_mobs` inside `run_tick_loop` and calls the world a
"second, independent snapshot". At HEAD, seeding is its own task (`integrated.rs:599-629`)
off a **shared** `ChunkStore` since #454. Its two most important claims — no natural
spawning, no despawn pass — are **confirmed** true.

**`docs/mob-species-spawning.md`** pseudocode at lines 26–27 is superseded by
`lodestone_entity::ai::roster`; the doc half-admits this in prose but the diagram was not
updated.

**The rewrite plan's own inventory** (`docs/plans/worldgen-rewrite.md`) lists
`structure / structure_set` and `template_pool / processor_list` as `0 / 0` bundled and
`noise_settings` as 1 of 7. Both were true when written and are now wrong: the corpus and
all 7 noise settings landed. `docs/worldgen-dimensions.md` and
`docs/worldgen-structure-corpus.md` are the current record.

---

## Undetermined

Labelled rather than guessed.

- **Whether `heights_from_field`'s `.max(sea_level - 1)` clamp (`overworld/fill.rs:137`) is
  faithful to `OCEAN_FLOOR_WG`** as well as to the oracle's `solidTop`. One array serves two
  vanilla types (`surface/mod.rs:14` vs `overworld/decorate.rs:232`) and the `OCEAN_FLOOR_WG`
  role has no JVM fixture.
- **What the client renders on receiving all-`Missing` light** — full-bright or
  dimension-default. Not traced, and it decides how visible §11 actually is.
- **Whether `ChunkWorld` can expose block light at all**, which bounds how large a real
  `SpawnCandidateSource` would be. `grep light` in `mobs.rs` returns nothing, so the sim has
  no light on hand; whether the API could supply it was not audited.
- **Whether any crate under `crates/plugins/` registers a mob-spawning command.** Moot for
  the shipped singleplayer path (every production `CommandDispatch` is `::none()`), but not
  enumerated crate by crate.
- **Per-feature behavioural parity of the 7 implemented feature types beyond their existing
  JVM fixtures.** This census measures *type coverage and reachability*, not correctness;
  correctness is `docs/worldgen-parity.md`'s and the `*_parity.rs` suites' job. Two named
  savanna vegetation residuals (11/185, 1/116) remain unexplained there.
- **Whether the 26.2 `configured_feature` corpus really has no `random_patch` or `flower`
  type.** The bundle's 55-type census shows neither, which differs from 1.21-era
  expectations; I did not confirm from `Feature.java` whether they were removed or renamed,
  and it matters only for anyone porting from older sources.

---

## How to change this doc

It is a census, so it rots by construction. When a gap closes, move the row's verdict and
cite the landing commit; do not delete the row, because "this was absent at `98433351`" is
the part that stays useful. Re-derive counts rather than editing them: the bundle counts
come from `find crates/lodestone-server/assets/worldgen/<registry> -type f | wc -l`, the
feature-type census from a `json.load` sweep over `configured_feature/`, and every
"absent" verdict from the grep named in its row. **Do not quote a number here from memory
into an issue body** — five issue bodies in this repo already inherited one wrong figure
from each other.

## Configuration

None. This doc reads the tree; it configures nothing.

## Dependencies

`crates/lodestone-worldgen`, `crates/lodestone-worldgen-core`, `crates/lodestone-server`
(the `EmbeddedResolver` and `assets/worldgen/`), `crates/protocol/v770`'s
`server_protocol.rs`, and the de-obfuscated `.cache/mc/26.2/src/` as the vanilla reference.
Companion docs: [`plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md) (the plan),
[`worldgen-parity.md`](./worldgen-parity.md) (correctness of what exists),
[`worldgen-dimensions.md`](./worldgen-dimensions.md) and
[`worldgen-structure-corpus.md`](./worldgen-structure-corpus.md) (the two data phases that
landed).
