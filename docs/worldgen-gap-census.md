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
`overworld_generator(seed)` in `crates/lodestone-server/src/worldgen_data.rs`, wrapped
as `OverworldChunkSource`, and encoded by `encode_column_body` in
`crates/protocol/v770/src/server_protocol.rs` — the only `ServerProtocol` impl. The
shell also calls the generator directly for singleplayer
(`crates/lodestone-shell/src/worldgen.rs`'s `generator` function). Anything not reached from one of those two
is orphaned.

---

## 1. Dimensions and world types

Vanilla source of truth: `net/minecraft/world/level/levelgen/NoiseGeneratorSettings.java`,
`presets/WorldPresets.java`, `level/biome/{MultiNoise,TheEnd,Fixed,CheckerboardColumn}BiomeSource.java`.

| item | verdict | evidence |
|---|---|---|
| Overworld terrain generator | **reached** | `OverworldGenerator::column_timed` runs 10 stages: aquifer, shape, biome, surface, materialize, carve, ore, vegetation, top-layer, intern (`overworld/mod.rs`, `column_timed`) |
| Nether generator | **landed since this census (`1c372b0e`, 2026-08-08), and wired into the server** | Stale: this row's grep found 0 hits at `98433351` on 2026-08-07; `crates/lodestone-worldgen/src/nether/` exists today with a real `NetherGenerator` (disabled-aquifer fill, `nether_cave`, measured "17,855/17,856 biome quarts against a real vanilla world" per that commit). `crates/lodestone-server/src/worldgen_data::nether_generator` constructs one and `chunk.rs` adopts its columns (`NetherColumn`) into the chunk store — a real production caller, not just a generator sitting alone. Re-run `grep -rni nether crates/lodestone-worldgen --include=*.rs` to reconfirm; it is no longer 0. |
| End generator | **landed since this census (`f02bd321`, 2026-08-08) — but *not* wired into the server, a genuine island** | Stale for the same reason as Nether: `crates/lodestone-worldgen/src/end/` exists with a real `EndBiomeSource`, `EndColumn`, `EndGenerator` implementing the `end_islands` algorithm end to end ("complete without the density interpreter" per that commit — i.e. it does not go through the generic `Density` graph at all). **Unlike Nether, nothing in `crates/lodestone-server` references it**: `grep -rn "worldgen::end" crates/lodestone-server/src` is empty, and `dimension.rs`'s own doc comment still lists "Adding the End" as future work. This is the one row in this section that is a real island rather than a stale "absent" — the generator exists, is presumably tested, and has zero production callers. |
| `minecraft:end_islands` density type | **landed** | `Density::EndIslands` is a real variant in `lodestone-worldgen-core/src/density/mod.rs` (parses `"end_islands"`, evaluates via `EndIslandNoise::compute`, has a signature hash arm) — not the `panic!` fallback this row described. Loading End data no longer panics on this specifically; whatever remains blocking a fully generic End density graph is narrower than this one type. |
| non-overworld `noise_settings` | **partly reached** | all 7 bundled (`assets/worldgen/noise_settings/{overworld,nether,end,amplified,caves,floating_islands,large_biomes}.json`); issue #519 made `amplified` and `large_biomes` selectable (`worldgen_data::WorldType`, `overworld_generator_of_type`/`overworld_chunk_source_of_type`). `nether`/`end` are read by their own dedicated generators (see below); `caves`/`floating_islands` are custom-dimension `noise_settings`, not overworld presets, and remain orphaned — referenced only from `crates/lodestone-data/tests/worldgen_dimension_data.rs` |
| `legacy_random_source` (Nether/End gate) | **absent** | 3 tree-wide hits, all in `crates/lodestone-data/tests/worldgen_dimension_data.rs`. Both non-overworld settings set it `true`; `overworld.json` sets it `false`. Tracked as **#486** |
| `world_preset/*.json` documents (7) + flat presets (9) | **orphaned (data)** | still true literally — no code parses a `world_preset/*.json` document itself. `worldgen_structure_corpus.rs`'s `EXPECTED_COUNTS` table is still the only Rust reference. **Note the distinction**: `amplified`/`large_biomes` are now reachable (row below), but via a hand-written `WorldType` enum naming the `noise_settings` id directly, not by resolving `world_preset/amplified.json`'s own `generator.settings` field — so even those two presets' *own* JSON documents remain unread |
| amplified / large_biomes | **reached** (issue #519) | `worldgen_data::WorldType::{Amplified,LargeBiomes}` selects the already-bundled `noise_settings/{amplified,large_biomes}.json` and their `density_function/overworld_{amplified,large_biomes}/*` documents through `overworld_generator_of_type`/`overworld_chunk_source_of_type`. Verified, not merely wired: at seed 4242, chunk `(0,0)` local `(0,0)`, `Overworld` yields top-non-air `y=64` and `Amplified` yields `y=130` — a 66-block divergence at the identical input (`crates/lodestone-server/tests/world_type_selection.rs`). `LargeBiomes`' climate noise is measurably coarser: over a 120-chunk strip at the same seed, `Overworld` crosses a biome boundary 20 times across 12 distinct biomes and `LargeBiomes` crosses once across 2. Both presets select `biome_source.preset: "minecraft:overworld"` in their own `world_preset/*.json`, so the existing `biome_parameters/overworld` table is correct for them — no `Resolver::biome_parameters` widening was needed, contrary to this doc's earlier "also found" note below (that widening is still needed for a *Nether* biome source, just not for these two) |
| superflat generator | **absent** | no `FlatLevelSource` analogue; grep `flat_level_generator\|superflat` in `crates/**/src` → hits are only light-propagation prose. #519 deliberately did not add a `WorldType` variant for it — one would either need this generator behind it or would silently serve ordinary overworld terrain under a "Flat" label, which is worse than the preset being absent |
| debug-world generator | **absent** | no implementation. Same deliberate omission as superflat, for the same reason |
| `FixedBiomeSource` / `CheckerboardColumnBiomeSource` | **absent** | grep → 0 hits. Needed by single-biome and debug presets |
| server multi-dimension plumbing | **absent** | Anvil paths hardcode `dimensions/minecraft/overworld` (`lodestone-server/src/region_source.rs`); tracked as **#330** |

**Also found:** `Resolver::biome_parameters(&self)` (`density/mod.rs`'s `Resolver` trait) takes **no
dimension argument**, so `assets/worldgen/biome_parameters/nether.json` — which cost a
bespoke JVM oracle to produce — is structurally unreachable: `EmbeddedResolver`'s override
hardcodes `biome_parameters/overworld`. A Nether biome source needs this trait method
widened, which #485 does not mention.

**Also found:** vanilla registers **three** carvers; we implement two (below). The missing
one is `nether_cave`, which `CarverConfig::parse` handles by
`panic!("unsupported carver type")`. #485's engine list does not
name it.

---

## 2. Biomes

Vanilla source of truth: `level/biome/{Climate,MultiNoiseBiomeSource}.java`,
`level/levelgen/NoiseRouterData.java`.

| item | verdict | evidence |
|---|---|---|
| Multi-noise climate sampling (7 channels) | **reached** | `crates/lodestone-worldgen/src/biome/mod.rs`, driven from `overworld/biome.rs`'s `biome_stage` |
| `Climate.RTree` search | **reached** | `biome/tree.rs` (717 lines, literal port); landed `7ff942dd` / `71dd8b22`. Issue **#491 is stale-open** |
| per-source-chunk biome memo | **reached** | `biome/memo.rs` |
| **3-D biome sampling (4×4×4 quart cells)** | **reached** (#512, generator `0ccb2e5d` + consumer `a617454d`) | `biome_cells_stage` builds a `BiomeCells` of 16 × (height/4) cells and `biome_stage`'s 16-entry surface array is now *read out of* it rather than sampled separately. `ChunkColumn` carries the grid, the v770 encoder resolves the column's small biome palette once and indexes per cell, and `chunk_nbt` reads and writes every section's own container. Measured over 576 generated columns: 503 carry a biome the surface array does not, across `lush_caves`, `dripstone_caves` and `sulfur_caves`. The prior verdict was **absent**, with the consequence that no cave biome could appear at any depth |
| biome-specific surface rules | **reached** | `Cond::BiomeIs` in `surface/mod.rs`, driven per column |
| `TheEndBiomeSource` / `FixedBiomeSource` / `CheckerboardColumnBiomeSource` | **absent** | see §1 |
| biome wire ids | **partial, known-divergent** | `server_protocol.rs`'s `BIOME_NAMES` const and `resolve_biome_id` use a *sorted* 55-entry local id space, not vanilla's registration order, because `worldgen/biome` is not in the configuration-phase registry sync (**#275**). Documented in place, not a hidden gap |

---

## 3. Surface rules

Vanilla source of truth: `SurfaceRules.ConditionSource`/`SurfaceRules.Condition` (conditions) and
`SurfaceRules.RuleSource` (rule sources).

**Type coverage is complete — 11/11 conditions, 4/4 rules.** Measured as a set difference,
not eyeballed:

| | vanilla | ours |
|---|---|---|
| rule sources | `bandlands`, `block`, `sequence`, `condition` | all 4 — `surface/mod.rs`'s `RuleParser::rule` |
| conditions | `biome`, `noise_threshold`, `vertical_gradient`, `y_above`, `water`, `temperature`, `steep`, `not`, `hole`, `above_preliminary_surface`, `stone_depth` | all 11 — `surface/mod.rs`'s `RuleParser::cond` |

Verdict: **reached**, and the only surface-rule axis in this census with no gap. Unknown
types panic loudly rather than degrading (`RuleParser::rule`/`RuleParser::cond`'s `other =>` arms), which is the right
failure mode. Per-dimension surface rules therefore need **no new condition type** — the
Nether's rules are a strict subset (independently confirmed in #485).

---

## 4. Carvers

Vanilla source of truth: `WorldCarver.CAVE`/`NETHER_CAVE`/`CANYON`.

| carver | verdict | evidence |
|---|---|---|
| `cave` | **reached** | `CarverConfig::Cave` (`carver/mod.rs`), `CaveConfig::carve`, driven from `carve_stage` |
| `canyon` | **reached** | `CarverConfig::Canyon` (`carver/mod.rs`), `CanyonConfig::carve` |
| `nether_cave` | **absent** | `carver/mod.rs`'s `CarverConfig::parse` `panic!("unsupported carver type")` arm. `configured_carver/nether_cave.json` is bundled (4/4 configured carvers) and unreferenced by any overworld biome, so the panic is latent, not live |

All 4 `configured_carver` documents are bundled; carver composition is per-biome through
`compose.rs`'s `build_biome_carvers`, in the biome JSON's own array order (which
`set_large_feature_seed`'s index depends on).

---

## 5. Ores and ore veins

Vanilla source of truth: `level/levelgen/feature/OreFeature.java`,
`level/levelgen/OreVeinifier.java`.

| item | verdict | evidence |
|---|---|---|
| `minecraft:ore` feature | **reached** | `feature/mod.rs`'s `parse_ore_config`, driven by `ore_stage` over the real 3×3 neighbourhood |
| per-biome ore lists from step 6 | **reached** | `compose.rs`'s `build_biome_ores`, index-preserving |
| `minecraft:scattered_ore` | **absent** | `build_biome_ores` skips any configured feature whose `type != "minecraft:ore"` (`compose.rs`, the `continue` on the type check). 2 bundled files (Nether gold/quartz-shaped placement) |
| **`OreVeinifier` (large copper/iron veins)** | **landed since this census — the live Overworld parity defect is closed** | Stale: the 1-hit grep was true at `98433351` on 2026-08-07. `crates/lodestone-worldgen/src/overworld/veins.rs` is now a real, documented port of `OreVeinifier` — its own module doc opens "Issue #496: `OreVeinifier` — the large copper and iron veins" and names the exact channels this row called out (`vein_toggle`, `vein_ridged`, `vein_gap`, `ore_vein_a`/`ore_vein_b`). `overworld/fill.rs` binds it once per chunk (a comment there explains why: `vein_toggle`/`vein_ridged` are `minecraft:interpolated`) and reaches it through the real `MaterialRuleList` after the aquifer pass. Re-run the row's own grep to reconfirm — it is no longer 1 hit. |

**#496 is now fully landed, both halves.** The second half landed first (`a27cbb98`, plus
U17/U18 `d50feba7`/`22982b99` addressing the same hashing/allocation costs); this census's own
2026-08-07 measurement caught it mid-flight with only that half done. `OreVeinifier` itself
(the first half, and the one this row tracked) landed since.

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
| 6 | `UNDERGROUND_ORES` | **reached** — `feature/mod.rs`'s `STEP_UNDERGROUND_ORES` |
| 7 | `UNDERGROUND_DECORATION` | absent (glow lichen, sculk) |
| 8 | `FLUID_SPRINGS` | absent (water/lava springs) |
| 9 | `VEGETAL_DECORATION` | **reached** — `feature/mod.rs`'s `STEP_VEGETAL_DECORATION` |
| 10 | `TOP_LAYER_MODIFICATION` | **reached** — `feature/top_layer.rs`'s `STEP_TOP_LAYER_MODIFICATION` |

**Feature-type coverage, measured against the bundle rather than recalled.** The 226
bundled `configured_feature` documents use **55 distinct types**. The engine implements:

- `ore` (`feature/mod.rs`'s `parse_ore_config`) — 30 of the 226 files
- `freeze_top_layer` (`feature/top_layer.rs`) — 1
- `simple_block`, `tree`, `block_column`, `random_selector`, `simple_random_selector`
  (`feature/vegetation/config.rs`'s `ConfiguredFeature` parsing) — 32 + 39 + 4 + 21 + 5

**Everything else — 48 of 55 types — falls to `ConfiguredFeature::Unsupported`
and silently places nothing.** That silence is deliberately made
*legible* rather than hidden: `collect_unsupported` (`feature/vegetation/config.rs`) walks the resolved
tree and `worldgen_data.rs`'s `vegetation_placer_gaps_are_named_not_silent` diffs it
against an allow-list. So this is a measured gap with a live instrument, not a blind spot.

**Placement-modifier coverage: 10 of vanilla's 15** (`PlacementModifierType.java`), split
across two independent engines that share no instances:

| engine | modifiers |
|---|---|
| ore (`feature/mod.rs`'s `Placement::parse`) | `count`, `rarity_filter`, `in_square`, `height_range`, `biome` — unknown types `panic!` |
| vegetation (`feature/vegetation/config.rs`'s `VegPlacement::try_parse`) | `count`, `in_square`, `heightmap`, `biome`, `rarity_filter`, `surface_water_depth_filter`, `noise_threshold_count`, `random_offset`, `block_predicate_filter` — unknown types return `None` |

Absent from both: `count_on_every_layer`, `environment_scan`, `fixed_placement`,
`noise_based_count`, `surface_relative_threshold_filter`.

Also open on this axis: **#428** (fancy/giant trunk placers — jungle, mangrove, cherry,
`FallenTreeFeature`).

---

## 7. Structures

Vanilla source of truth: `level/levelgen/structure/`, `structure/pools/`,
`structure/placement/`, `chunk/status/ChunkStatus.java`.

**Re-verdicted 2026-08-14 — this entire section is stale in the "understates" direction, and
this is the section the task brief specifically flagged for it.** `crates/lodestone-worldgen/src`
now has a `structure/` module totalling **13,101 lines** across `mod.rs`, `beardifier.rs`,
`jigsaw.rs`, `pool.rs`, `template.rs`, `processor.rs`, `mineshaft.rs`, `placement.rs` and
`coded.rs` — every S1–S4 row below is now real code, most of it wired into the server. Today's
landings list independently corroborates this: ruined portals, buried treasure and mineshafts are
named as landed, and this module is where they live.

| item | verdict | evidence |
|---|---|---|
| structure **data** corpus | **orphaned (data) as of 2026-08-07 — now consumed, re-verify the drift-gate-only claim** | 34 structures, 20 structure sets, 188 template pools, 40 processor lists, 1212 `.nbt` templates, 92 worldgen tags — all bundled and byte-identical to the jar (`docs/worldgen-structure-corpus.md`; landed `6c6c0e10` under **#484**). **No longer read by only the drift gate**: `structure/template.rs`, `structure/pool.rs` and `structure/processor.rs` are real consumers now, and `crates/lodestone-server/src/structure_loot.rs` reads `StructureTemplate` and `template::transform` directly for chest loot. |
| structure placement / `/locate` (S1) | **landed and wired into the server** | `crates/lodestone-worldgen/src/structure/mod.rs` exists (contrary to this row's "no `structure` module" claim, which was accurate only at `98433351`). `crates/lodestone-server/src/chunk.rs` holds a real `structure_starts: Vec<Arc<lodestone_worldgen::structure::StructureStart>>` field and a `structure_starts_stage` documented at 8-chunk `REFS_RADIUS` with a measured 7.4× fan-out (1,024 → 7,569 references) — this is production chunk generation, not a test fixture. |
| template placement + processors (S2) | **landed** | `structure/template.rs` + `structure/processor.rs` exist and are read by `structure_loot.rs`'s `transform` call, which places loot-bearing structures (chests) using the real template/processor pipeline. |
| **beardifier (S3)** | **still a constant-zero leaf — re-verified, this specific sub-claim is NOT stale** | `Density::Beardifier => 0.0` is still the evaluation arm in `lodestone-worldgen-core/src/density/mod.rs`, unchanged. So even though structures are now placed (S1) and templated (S2), terrain still does not deform around them — a mineshaft or ruined portal sits on unmodified terrain rather than vanilla's beard-adapted surface. This is the one S-row that did **not** move; do not flip it without finding a non-zero `Beardifier` evaluation. |
| jigsaw (S4) | **landed** | `structure/jigsaw.rs` and `structure/pool.rs` exist, backing the ruined-portal and other jigsaw-assembled structures named in today's landings. |
| `WorldgenRandom.setLargeFeatureWithSalt` | **not re-verified this pass** | re-grep `large_feature_with_salt`/the equivalent salting call in `structure/placement.rs` before trusting either verdict — S1 landing makes "absent" unlikely but this specific function was not checked. |
| ChunkStatus pipeline (S0 prerequisite) | **partially addressed, not a formal `ChunkStatus` type** | Still no vanilla-shaped `ChunkStatus` enum, but `chunk.rs`'s `structure_starts`/`structure_refs` stages (§ evidence above) now encode an explicit stage ordering with `store::StageSlot`, and `spawn_stage.rs` cites `ChunkStatus.SPAWN` by name as the vanilla stage it corresponds to — the *ordering concept* has arrived piecemeal even without the enum. Whether `STRUCTURE_STARTS` truly precedes `NOISE` the way vanilla requires (so the still-zero beardifier could someday consult it) was not re-verified this pass. |

The generator's own module doc no longer says structures are unbuilt — re-grep
`overworld/mod.rs` for "Still not composed" before citing that quote; it describes the tree at
`98433351`, not today's.

---

## 8. Slime chunks

Vanilla source of truth: `WorldgenRandom.seedSlimeChunk` and
`Slime.checkSlimeSpawnRules`:

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
`set_decoration_seed`, `set_feature_seed`, `set_large_feature_seed` and nothing
else).

This one is worth naming as cheap: it is a **pure function of `(chunk_x, chunk_z, seed)`**
over primitives we already have (`LegacyRandomSource` in `rng/legacy.rs`), so it is
*exactly checkable* against a JVM oracle — bit-exact or wrong, no tolerance discussion
available. Its consumer (slime spawning) is blocked on the spawning gaps in §9, but the
predicate itself is not blocked on anything.

---

## 9. Mob spawning

Vanilla sources of truth: `chunk/status/ChunkStatus.java` (`SPAWN`),
`ChunkStatusTasks.generateSpawn`, `NaturalSpawner.spawnMobsForChunkGeneration`,
`entity/SpawnPlacements.java`.

| item | verdict | evidence |
|---|---|---|
| worldgen-time `SPAWN` step (initial animal population) | **absent** | zero occurrences of `spawners`, `spawn_costs`, `SpawnPlacement`, `MobCategory`-from-biome anywhere in `crates/**/src`. The data **is** bundled and unread: `assets/worldgen/biome/forest.json` carries `spawners` (sheep/pig/chicken/cow with weight/min/max) and `spawn_costs`; `EmbeddedResolver::biome_document` (`worldgen_data.rs`) is consumed only for `carvers` and `features[6]`/`features[9]` (`compose.rs`) |
| runtime natural spawning | **orphaned** | `crates/lodestone-server/src/mob_spawn.rs` (660 lines) is a faithful cap/despawn engine — `MAGIC_NUMBER = 289`, caps `70/10/15/5/5/5/20`, `check_despawn`. The only `impl SpawnCandidateSource` is a test mock (`tests/mob_spawn.rs`); `run_spawn_cycle` (`mobs/mod.rs`) and `despawn_pass` (`mobs/mod.rs`) have **zero production callers**; `crates/lodestone-server/src/tick.rs`'s `run_tick_loop` never calls them. Tracked as **#221** / **#222** |
| second, independent orphan | **orphaned** | `crates/lodestone-entity/src/spawn.rs` (442 lines) duplicates the same engine *plus* `SpawnConditions::permits` — the nearest thing to `SpawnPlacements.checkSpawnRules`. `grep DespawnCtx\|SpawnConditions\|SpawnSample\|mob_cap` outside `crates/lodestone-entity/` → 0 hits |
| what actually puts mobs in the world | **reached (a demo roster)** | `seed_demo_mobs` (`mobs/mod.rs`) places `mob_count = 6` mobs in a radius-6 ring around `(8,8)`, one per `DEMO_SPECIES` entry (`mobs/mod.rs`), called once from `MobHandle::reseed` (`mobs/mod.rs`) inside `integrated.rs`. No `/summon` (every production `CommandDispatch` is `::none()`), no spawn eggs, no spawner blocks (**#224**), no entity persistence, no mobs at all on LAN or wasm |

---

## 10. Heightmaps

Vanilla source of truth: `level/levelgen/Heightmap.java` — six types, four of them
persisted and sent.

**Verdict: partial, and a parity defect in something we ship.**

- **On the wire: empty.** `server_protocol.rs`'s `encode_column_body` encodes `Heightmaps::new()` — a default,
  zero-entry `Vec<(u32, Heightmap)>` (`crates/lodestone-world/src/heightmap.rs`'s `Heightmaps` struct). Valid
  framing, no data. The function's own doc says so.
- **Not persisted.** `crates/lodestone-server/src/chunk_nbt.rs` — deliberately omitted,
  relying on vanilla's `Heightmap.primeHeightmaps`. A reasoned decision.
- **No storage.** `GeneratedColumn` (`overworld/output.rs`) and server-side
  `ChunkColumn` (`lodestone-server/src/chunk.rs`) have **no heightmap field**, so nothing
  could survive the generator → server → wire hop even if computed.
- **Four internal scans exist**, all consumed inside the generator only:
  `heights_from_field` (`overworld/fill.rs`, serving as *both* `WORLD_SURFACE_WG` and
  `OCEAN_FLOOR_WG`), `VegGrid::height_world_surface` (`feature/vegetation/grid.rs`),
  `VegGrid::height_ocean_floor` (`grid.rs`), and `motion_blocking_first_free`
  (`feature/top_layer.rs`, a real per-state predicate).
- **`MOTION_BLOCKING_NO_LEAVES` is collapsed onto `MOTION_BLOCKING`**
  (`feature/vegetation/config.rs`) with no leaf/log exclusion.
- **The island shape is explicit:** `GeneratedColumn::top_non_air_y` (`output.rs`),
  documented as matching `WORLD_SURFACE_WG`, has **no production caller** — only
  `worldgen_data.rs` inside `#[cfg(test)]`. And
  `motion_blocking_heightmap_matches_vanilla_per_column` (`worldgen_data.rs`) proves a
  `MOTION_BLOCKING` against a JVM oracle that no shipped byte ever carries.

Incremental-vs-snapshot, since vegetation cost candidate 3 in the rewrite plan depends on
it: the veg-grid pair are **live rescans per query** against the already-mutated grid
(`grid.rs`), observationally equivalent to vanilla's incremental maintenance for in-pass
reads; `motion_blocking_first_free` is a one-shot post-decoration scan; `heights_from_field`
is a shape-stage snapshot never updated by decoration. **No stored heightmap is maintained
incrementally, because no heightmap is stored.**

---

## 11. Light

Vanilla source of truth: `level/lighting/{LightEngine,BlockLightEngine,SkyLightEngine}.java`,
and `ChunkStatus.INITIALIZE_LIGHT` / `LIGHT`.

**Verdict: orphaned relative to the server output path — a parity defect in something we
ship.**

- **The server sends nothing real.** `server_protocol.rs`'s `compute_column_light` method encodes
  `ColumnLight::new(section_count)`, which is `vec![LightData::Missing; section_count + 2]`
  for both sky and block (`crates/lodestone-world/src/light.rs`'s `ColumnLight`/`LightData`).
  `grep "sky_light\|block_light\|LightEngine" crates/lodestone-server/src` → **0 hits**.
  `LIGHT_UPDATE` (packet 48) exists only as a client *decode* arm
  (`crates/protocol/v770/src/adapter/chunk.rs`); nothing encodes it.
- **No worldgen light stage.** The generator relies on this: `feature/top_layer.rs`'s
  own doc comment — "Block light is not modelled, and does not need to be… `initialize_light` runs strictly
  after `features`" — and the block-light gate is hard-true in the same file. That
  argument is sound *for the top-layer stage* and says nothing about the served chunk.
- **A real engine exists and is a genuine port**: `crates/lodestone-world/src/lighting.rs`
  (1,105 lines) — descending-level 15-bucket propagation, `opacity = max(1, lightDampening)`,
  sky seeded per `ChunkSkyLightSources.isEdgeOccluded`, exposing `compute_column_light`,
  `compute_column_light_with_neighbours` and `diff_column_light`.
- **Its only production caller is the client's local world**, not the server:
  `crates/lodestone-shell/src/worldgen.rs`'s `generate_column` function. So singleplayer-direct is lit and the
  integrated-server path is not.
- **Do not conflate with `docs/light-ramp.md`**, which documents the per-vertex *lightmap
  curve* in `crates/lodestone-render/src/light.rs` — client rendering of an
  already-known light byte, a different subsystem.

---

## 12. Block entities from worldgen

Vanilla: worldgen places block entities (structure chest loot, dungeon/mineshaft spawners,
trail-ruins decorated pots, bee nests with bees) and they travel in the chunk-data packet's
`block_entities` array.

**Verdict: reached for bee nests** (#520, generator `60c9a5f9` + consumer `a617454d`);
still absent for every other producer, because no other feature makes one.

- The slot exists end to end now: `GeneratedColumn::block_entities()` →
  `ChunkColumn::block_entities()` (via `chunk_nbt::generated_block_entity`, which
  turns the generator's typed enum into a `BlockEntity::Opaque` holding the vanilla
  save-form compound) → the chunk packet's block-entity array.
- The wire array is no longer a literal zero. As a side effect a **chest read off
  disk** now reaches the client too: `region_source::load` copies the chunk's
  entities onto the column, where before they went only into the tick loop's
  registry and the packet still claimed the chunk had none.
- Structure-borne block entities (chest loot, spawners, decorated pots) remain
  absent for the reason §7 gives — no piece generator, so no structure blocks at
  all, let alone their block entities.
- **The one shipped defect on this axis, now fixed**, was honest and in-source:
  `place_beehive_decorator` (`feature/vegetation/place.rs`) writes
  `minecraft:bee_nest[facing=south,honey_level=0]` (`:425`) and then at `:428`:
  *"Bee-entity storage (2-3 bees) is not modelled"* — `let _bee_count = 2 +
  random.next_int_bounded(2);`. The draw is consumed (so the RNG stream stays aligned, which
  is correct) and discarded, so **every generated bee nest reaches the client empty**. The
  fix was not adding a draw but starting to *use* one — plus a genuinely new
  `nextInt(599)` per bee, which vanilla makes and this engine had been short of.
- `docs/block-entities.md` is a different thing: four server-side tick *simulations*
  (composter, furnace, hopper, brewing) plus Anvil NBT round-trip. None is fed by the
  generator. Related but distinct: **#477** (loading a real vanilla world drops 1608 of
  1613 block entities).

---

## 13. Seed derivation

Vanilla source of truth: `level/levelgen/WorldgenRandom.java`, `RandomState.java`.

| derivation | verdict | evidence |
|---|---|---|
| `WorldgenRandom.next(bits)` legacy-shape wrapping | **reached** | `rng/mod.rs`'s `WorldgenRandom` struct — reproduces vanilla's "all draws use the legacy `BitRandomSource` structure even when the wrapped source is xoroshiro", which is load-bearing (naive delegation diverged, and did) |
| `setDecorationSeed` | **reached** | `WorldgenRandom::set_decoration_seed` (`rng/mod.rs`) |
| `setFeatureSeed` | **reached** | `WorldgenRandom::set_feature_seed` (`rng/mod.rs`) — per-feature stream isolation, which is why adding a feature cannot desync its neighbours |
| `setLargeFeatureSeed` | **reached** | `WorldgenRandom::set_large_feature_seed` (`rng/mod.rs`) — carvers |
| `setLargeFeatureWithSalt` | **reached** (#514 S1) | `set_large_feature_with_salt`, consumed by structure-set placement (§7) |
| `seedSlimeChunk` | **absent** | 0 hits (§8) |
| `RandomState`'s algorithm switch on `legacy_random_source` | **absent** | `density/mod.rs`'s `Builder::new` hardcodes the Xoroshiro branch; **#486** |
| `NormalNoise.createLegacyNetherBiome` (raw-seed, non-positional) | **absent** | `PerlinNoise::new_legacy` is private and blended-noise-only; no `NormalNoise` legacy-init path (recorded in #485/#486) |
| positional forks for noise / aquifer / ore RNG | **reached** | `Builder::positional_factory` (`density/mod.rs`), consumed by the noise instantiation and the carve/ore drivers |

---

## Ranked by player-visible impact

Ranking is about what a player would notice standing in the world, not about cost.

| rank | gap | why it ranks here | verdict |
|---|---|---|---|
| 1 | ~~**No Nether, no End**~~ | **re-verdicted 2026-08-14**: the Nether generates and is wired into the server (`1c372b0e`); the End's generator and biome source are complete but **not wired into the server** — a real island, not an absence. Re-verify player-facing reachability (can a player actually portal in?) before fully closing this row. | Nether reached; End built-but-unwired (§1) |
| 2 | ~~**No structure *blocks***~~ | **re-verdicted 2026-08-14**: `structure/` (13,101 lines: `jigsaw.rs`, `pool.rs`, `template.rs`, `processor.rs`, `mineshaft.rs`, `beardifier.rs`) landed and is wired into `chunk.rs`'s `structure_starts`/`structure_refs` stages and `structure_loot.rs`'s template placement — pieces do generate now. **Terrain still does not deform around them** (beardifier is a hardcoded `0.0`), so a structure can still sit oddly against unmodified terrain. | placement + pieces reached; beardifier still absent (§7) |
| 3 | **8 of 11 decoration steps, 48 of 55 feature types** | no lakes, springs, geodes, dripstone, icebergs, disks, dungeons, fossils, glow lichen, sculk, coral. The world reads as terrain + ore + grass/trees + snow — **not re-verified this pass**, treat as unconfirmed rather than re-audited | absent (§6) |
| 4 | **No mob spawning** | **stale, see the server-gameplay gap census's §9 re-verdict (2026-08-14)**: `natural_spawn.rs` now drives real spawning from the tick loop with per-species light/biome rules, closing most of this row. Not re-verified from the worldgen side of §9 in *this* document — re-audit before trusting either doc's §9 in isolation. | was absent + orphaned, now landed per the sibling census (§9) |
| 5 | **All-`Missing` light on the served chunk** | the integrated-server path ships no light while a working engine sits one crate away — not re-verified this pass | orphaned — **parity defect** (§11) |
| 6 | ~~**No 3-D biomes**~~ | **fixed** (#512): the generator samples a real 4x4x4 grid and the encoder, region writer and region reader all carry it per section | reached (§2) |
| 7 | ~~**Ore veins never generate**~~ | **fixed, re-verdicted 2026-08-14**: `overworld/veins.rs` is a real `OreVeinifier` port, bound per-chunk in `overworld/fill.rs` and reached through the production `MaterialRuleList` after the aquifer pass. | reached (§5) |
| 8 | **Empty heightmap NBT** | not directly visible, but it is a wrong value in a field we populate, and a JVM-proven `MOTION_BLOCKING` exists and is unreachable — not re-verified this pass | partial — **parity defect** (§10) |
| 9 | **Slime chunks** | one predicate; blocks slime spawning, and a well-known player-facing mechanic — not re-verified this pass | absent (§8) |
| 10 | **World presets: superflat / single-biome / debug still unreachable** | ~~amplified / large_biomes~~ **reached** (#519); the remaining three need generator code this tree does not have (`FlatLevelSource`, `FixedBiomeSource`, a block-grid debug generator), not just wiring | partial — 2/5 reached (§1) |
| 11 | ~~**Empty bee nests**~~ | **fixed** (#520): the decorator's discarded bee draw is used, and the chunk packet's block-entity array is no longer a hardcoded zero | reached (§12) |
| 12 | **`nether_cave` carver, `scattered_ore`** | **the `nether_cave` half is fixed**: `CarverConfig::parse` now matches `"minecraft:nether_cave"` explicitly (a `nether` flag on `CaveConfig`) instead of panicking, and the Nether generator that makes it live has landed. `scattered_ore` was not re-verified this pass. | `nether_cave` reached; `scattered_ore` not re-verified (§4, §5) |

**The parity-defect set — worse than absences, because they look done:** ore veins (§5),
heightmaps (§10), light (§11), ~~empty bee nests (§12)~~, and one this census missed —
~~the chunk NBT's permanently empty `structures{starts,References}` compound~~ (both now
fixed, kept listed because the *shape* is the lesson). Each is a field or a world we already
populate, populated wrongly, inside a subsystem whose gates are green. The `structures` stub
is the sharpest instance: a well-formed field that always says nothing, so no reader ever
errors and the absence is invisible until you go looking for a village.

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
| world presets / alternate generators | **#519** | amplified/large_biomes landed; superflat/single-biome/debug and world-creation-screen wiring still open |
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
Nether/End and omits (a) the `nether_cave` carver, which `CarverConfig::parse` panicked on
at the time this was written, and (b) that `Resolver::biome_parameters(&self)`
(`density/mod.rs`'s `Resolver` trait) has no dimension argument, so the Nether parameter
list it landed is structurally unreachable until the trait widens.

**Found stale during this citation-cleanup pass, itself, and left for a content
re-audit rather than silently corrected:** claim (a) above, and the matching "also found"
paragraph and §4 Carvers table row earlier in this doc, no longer hold —
`CarverConfig::parse` in `crates/lodestone-worldgen/src/carver/mod.rs` now matches
`"minecraft:cave" | "minecraft:nether_cave"` explicitly (with a `nether` flag threaded onto
`CaveConfig`) instead of panicking on `nether_cave`. More broadly, `crates/lodestone-worldgen/src`
now contains `nether/mod.rs` (980 lines) and `end/mod.rs` (637 lines), which directly
contradicts §1's "Nether generator: absent" / "End generator: absent" verdicts as measured
by this document's own `grep -rni nether` check. This document's substantive verdicts —
not just its citations — appear to have drifted significantly since the `98433351` measurement
and warrant a dedicated re-audit; this pass only touched citation format and did not
re-verify verdicts.

**A second confirmed-stale verdict found the same way:** §5's `OreVeinifier` row
(**absent — live Overworld parity defect**) no longer matches the tree either.
`crates/lodestone-worldgen/src/overworld/veins.rs` (227 lines) is a real port —
`VeinPrograms::build`, all three router channels, `OreVeinifier.create`'s block-state
filler — and `overworld/mod.rs` wires it in (`mod veins;`, a `veins: Option<veins::VeinPrograms>`
field, built at construction from `veins::VeinPrograms::build`). That is a production call
site, not just a test, so the "absent" verdict and the "1 hit, test-only" grep evidence both
read as stale. Not corrected here for the same reason as the carver finding above: verdict
correctness is a content-audit question, not a citation-format one.

**Closing the loop, 2026-08-14: the recommended re-audit happened, for §1/§5/§7.** All three
sections above (Nether/End generators and the `end_islands` density type in §1; `OreVeinifier`
in §5; structures S1/S2/S4 and the data corpus in §7) are now corrected in place with fresh
evidence, dates and re-run grep commands, rather than left as this after-the-fact note. One
sub-finding survived the re-audit unchanged: §7's beardifier (S3) is still a hardcoded `0.0`
leaf even though structures now place and template — terrain does not yet deform around them.
§9 (mob spawning), §2 Biomes' `TheEndBiomeSource` row, and `docs/live-mob-sim.md`/
`docs/mob-species-spawning.md`'s own stale claims (named below) were **not** re-audited this
pass — treat those as still open per this note, not as covered by the above.

**A third, larger confirmed-stale area: §7 Structures.** `crates/lodestone-worldgen/src`
now has a `structure/` module (`beardifier.rs`, `coded.rs`, `jigsaw.rs`, `mineshaft.rs`,
`mod.rs`, `placement.rs`, `pool.rs`, `processor.rs`, `template.rs` — over 13,000 lines
combined) plus `overworld/structures.rs` (554 lines), and `overworld/mod.rs` carries a
`structure_starts: store::StageSlot<...>` field and a `structures: Option<StructureRegistry>`
field built from `crate::structure::StructureRegistry::new` at construction — a production
call site. §7's "structure placement / `/locate` (S1): absent", "template placement +
processors (S2): absent" and "jigsaw (S4): absent" rows, and the §"Ranked by player-visible
impact" table's #2 entry ("No structure *blocks*"), read as stale against this. `spawners.rs`
(427 lines) and `spawn_stage.rs` (339 lines) also exist and may bear on §9's mob-spawning
verdicts, not independently confirmed here. **Recommendation: this document needs a full
content re-audit, not just the citation-format pass this session performed** — the gap
between what it claims and what the tree now contains looks large enough that most of its
"absent"/"orphaned" verdicts should be re-measured from scratch rather than trusted.

**`docs/live-mob-sim.md`** has a stale architecture diagram (lines 23–30): it puts
`ChunkWorld::from_source` and `seed_demo_mobs` inside `run_tick_loop` and calls the world a
"second, independent snapshot". At HEAD, seeding is its own task (`open_in_memory_with_mobs_using`'s `seed_task` in `integrated.rs`)
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

- **Whether `heights_from_field`'s `.max(sea_level - 1)` clamp (`overworld/fill.rs`) is
  faithful to `OCEAN_FLOOR_WG`** as well as to the oracle's `solidTop`. One array serves two
  vanilla types (`surface/mod.rs`'s module doc, on `WORLD_SURFACE_WG`, vs `overworld/decorate.rs`'s
  `stitch_heights`, on `OCEAN_FLOOR_WG`) and the `OCEAN_FLOOR_WG`
  role has no JVM fixture.
- **What the client renders on receiving all-`Missing` light** — full-bright or
  dimension-default. Not traced, and it decides how visible §11 actually is.
- **Whether `ChunkWorld` can expose block light at all**, which bounds how large a real
  `SpawnCandidateSource` would be. `grep light` in `mobs/mod.rs` returns nothing, so the sim has
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
