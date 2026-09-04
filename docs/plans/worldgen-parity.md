# Plan: full 26.2 chunk-generation parity, and the version seam for later generators

## What it is

The dispatch plan for taking `lodestone-worldgen` from today's composed subset (shape, aquifer,
biomes, surface, carvers, ores, most vegetation) to **full 26.2 chunk parity**, plus the design —
not the build — of the seam that lets other versions' generators be added later. It sequences the
prerequisite work the owner already expects (structures and the other unported decoration steps),
names agent-sized units with file ownership, and states what "full parity" cannot mean yet.

Companion docs, which this plan extends rather than repeats: the
[Worldgen engine overview](../worldgen.md) (module layout, whole-chunk harness, and existing
performance evidence), [Overworld biome assignment and surface material](../worldgen-biomes.md),
and [Decoration: features, vegetation, ores and generation-time mob spawns](../worldgen-decoration.md).
This plan covers remaining decoration, structures, performance measurement, and the future
version-selection seam.

## How it works

### 1. Parity census, stage by stage

The reference generation pipeline is a chunk-generation-status chain: `empty → structure_starts →
structure_references →
biomes → noise → surface → carvers → features → initialize_light → light → spawn → full`, with the
`features` status fanning out into eleven decoration steps run per biome in order. Status per stage:

| # | reference stage | status | evidence |
|---|---|---|---|
| 1 | RNG / noise / density router (reference density-function and noise-router data) | **ported, bit-exact** | `rng_parity` 663/663, `noise_parity` 1224/1224, `region_parity` 34048/34048, interpolated `final_density` 98304/98304 — committed JVM dumps in `crates/lodestone-worldgen/tests/support/*_jvm.txt` |
| 2 | biome assignment (per-status sampling and climate-parameter nearest-point search over the climate R-tree) | **partial: 2-D only** | `crates/lodestone-worldgen/src/biome.rs` module doc: one sample per horizontal quart at that quart's own surface height, broadcast down the column — the reference is per quart **cube**. Table: 7594 rows dumped from the authoritative bootstrap, brute-force search. Cave biomes (`dripstone_caves`, `lush_caves`, `deep_dark`) are unreachable underground |
| 3 | noise fill + real aquifer | **ported + composed** | `aquifer_parity`; composed harness: chunk (0,0) `postcarve` **0 real mismatches** (this plan's parity harness) |
| 4 | surface rules, including badlands bands | **ported + composed** | `surface_parity`; the badlands band lookup is covered and the badlands fixture is included |
| 5 | carvers (17×17 per-source-chunk biome) | **ported + composed** | `carver_parity`; same (0,0) zero-real-mismatch postcarve result |
| 6a | `UNDERGROUND_ORES` | **ported + composed (real 3×3)** | `feature_parity`; composed residual vs the *single-source* oracle stage 2237/98304 at (0,0); single-source debug toggle isolates the engine at 563/98304 — the rest is real spill the oracle can't model yet |
| 6b | `VEGETAL_DECORATION` | **partial: composed 3×3, named gaps** | `vegetation_parity`: bit-exact at 4 fixtures modulo gaps; `KNOWN_VEGETATION_GAPS` (`crates/lodestone-server/src/worldgen_data.rs`) names them per biome: `multiface_growth` (glow lichen — in **every** biome's list), `fallen_tree`, fancy/giant trunks (jungle, dark oak, mangrove, cherry; acacia is covered), kelp/seagrass/coral/`sea_pickle` (oceans), bamboo, vines, huge mushrooms, `root_system`/`vegetation_patch` (lush caves), `random_boolean_selector`, two `simple_block`/`block_column` config variants. Plus two measured, mechanism-unknown savanna residuals (11 leaf-`distance` cells; 1 `short_grass` cell) |
| 6c | `LAKES` (`lake_lava_underground`/`_surface`) | **absent** | in every biome's step list (verified across `assets/worldgen/biome/*.json`); no Rust feature kind |
| 6d | `LOCAL_MODIFICATIONS` (`amethyst_geode` everywhere; `large_dripstone`; `iceberg_packed`/`iceberg_blue` in frozen oceans) | **absent** | same verification |
| 6e | `UNDERGROUND_STRUCTURES` step *features* (`monster_room`, `monster_room_deep` everywhere; `fossil_upper`/`_lower` in swamp) | **absent** | these are features (dungeons/fossils), cheaper than real structures |
| 6f | `SURFACE_STRUCTURES` step *features* (`blue_ice` in frozen oceans; `desert_well` etc.) | **absent** | |
| 6g | `UNDERGROUND_DECORATION` (`dripstone_cluster`, `pointed_dripstone`, sculk...) | **absent** — and unreachable until 3-D biomes, since only cave biomes carry these | |
| 6h | `FLUID_SPRINGS` (`spring_water`, `spring_lava`) | **absent** | in every biome's list |
| 6i | `TOP_LAYER_MODIFICATION` (`freeze_top_layer` — snow layers + ice) | **ported + composed, bit-exact at 4 fixtures** | `top_layer_parity` (in `lodestone-server/src/worldgen_data.rs`) loads the authoritative post-vegetation field and requires the same writes at the same coordinates: snowy_plains 250 snow + 250 `snowy` flips, frozen_ocean 36 ice + 0 snow, windswept_hills 115 snow, desert 0. Plus 1,024 columns of `MOTION_BLOCKING` heightmap against the authoritative heightmap query. Four controls were run and observed. See [Overworld biome assignment and surface material](../worldgen-biomes.md) |
| 7 | structures (`structure_starts`/`structure_references`, placement, jigsaw assembly, terrain adaptation) | **implemented and composed** | `lodestone-worldgen::structure` supplies salted placement, starts/references, templates, jigsaw, coded pieces, and the beardifier; `pre_ore_stage` consumes the result before fill. Durable residuals are the incomplete-generator ledger and broader structure-positive outside-oracle coverage, including a whole-chunk fixture, before claiming final parity near structures. |
| 8 | `initialize_light` / `light`; heightmaps | **absent from the served wire** | `encode_column_body`, `crates/versions/26.2/src/server_protocol.rs`: heightmaps sent empty, light all-`Missing` — documented gap; client relights locally |
| 9 | `spawn` (initial mob generation, `disable_mob_generation`) | **absent from generation** | runtime spawning is `crates/lodestone-server/src/natural_spawn.rs` (a different mechanism, currently another agent's file) |
| — | old-chunk blending (region blending, below-zero retrogen, and per-status biome/noise generation) | **absent, deliberately out of scope** | only matters for worlds imported from older versions |

Also known and bounded: fluid block-states are emitted without the `level` property (the "Known
representation gap" in this plan) — representation-only, tracked separately in the diff so it
never inflates real-mismatch counts.

### 2. Prerequisite graph

```
oracle/harness expansion (U1)  ──────────────┐  (gates *measuring* everything below)
                                             ▼
independent, parallel now:          whole-chunk gate per stage
  freeze_top_layer (U2)  ── CLOSED
  lakes + springs (U3)
  geodes + dungeons + fossils + icebergs (U4)
  glow lichen / multiface_growth (U5)
  trunk placers + fallen_tree (U5)
  ocean vegetation: kelp/seagrass/coral (U6)
  performance benches (U9)
  version seam (U8)

3-D biome sampling + climate R-tree port (U7)
  └─► UNDERGROUND_DECORATION / cave-biome vegetation (dripstone, lush-caves flora, sculk)
        └─► "full FEATURES-status parity" measurable

structures (implemented production path):
  salted placement + starts/references + piece assembly + terrain adaptation
    └─► incomplete-generator ledger + per-family NBT and block gates
          └─► structure-positive outside-oracle coverage, including a whole-chunk fixture
                └─► bit-exact whole-chunk parity near structures becomes measurable
```

Everything in the "independent" block extends the existing, proven placed-feature interpreter
(`crates/lodestone-worldgen/src/feature/`) with new `configured_feature` kinds — engine risk is low;
the risk is **oracle-first discipline** (§3). 3-D biomes gate the cave-decoration step because only
cave biomes carry it; structures require complete generators and structure-positive outside-oracle
coverage before their final parity claim can be made.

### 3. How parity is measured

**The instrument exists — extend it, don't reinvent it.** `crates/lodestone-worldgen-parity` is the
whole-chunk gate: `ComposedChunkOracle.java` boots the real 26.2 server, drives the reference
generator behavior directly, and dumps full 16×384×16 stage snapshots into a committed
run-length-encoded fixture; `tests/chunk_parity.rs` asserts measured ceilings/floors per (chunk,
stage) with **no wildcard arm**, and its negative controls are run, not described
(`control_mutate_one_block_is_caught`, `control_self_diff_is_exact`, `fixtures_are_non_vacuous`).
Isolated per-stage oracles follow the `VegetationOracle.java` pattern with `LODESTONE_REGEN=1`
generate-or-assert fixtures. The expected value always originates in the JVM, never in
`decode(encode(x))` self-play.

Per new stage, the sequence is fixed: **(1) oracle stage first** (extend `ComposedChunkOracle` or
add an isolated oracle), **(2) fixture chosen to guarantee non-zero content of the feature under
test**, (3) engine port, (4) composed gate with a measured floor and a control that must fail.

**Oracle dumps needed** (each ~one Docker/JVM run, committed as RLE text):

- Extend `ComposedChunkOracle` with `postfeatures` **full-3×3** (the named next increment — 8 more
  real chunks per fixture; until it exists the ore stage cannot be gate-closed to zero), a
  `postvegetation` stage, and a `final` (post-`TOP_LAYER_MODIFICATION`) stage.
- Grow the fixture set from 2 chunks / 1 seed to **~8 chunks across ≥2 seeds**, chosen per biome
  class so no stage is vacuously tested: ocean (0,0), badlands (-120,-120) (both exist), one snowy
  (freeze_top_layer + ice), one forest (fancy oak), one jungle, one warm ocean (coral), one frozen
  ocean (icebergs + blue_ice), one over a large cave volume (3-D biomes / dripstone).
- Isolated oracles for new feature families: lakes/springs/geodes can share one
  `MiscFeatureOracle` (same proxy pattern); `freeze_top_layer` needs a centre-only oracle because
  it consumes no RNG and never crosses a chunk boundary.
- One **author-independent end-to-end control**: generate a region on the reference `terrain.sh`
  oracle server, read the region file via `lodestone-anvil` (once its reader lands), and diff
  against our generator. A region file written by the reference server avoids sharing the oracle
  driver's implementation with the code under test.

**Three traps, all already paid for here, that every unit must design against:**

1. **A self-authored oracle validates only what it models.** The vegetation proxy must support
   the block-state lookup used by tree placement; without it, a fixture can exercise grass while
   never placing a trunk. Rule:
   every oracle fixture must contain content that is structurally impossible to be absent (the
   savanna tree-rate argument), and a zero from a new oracle is a defect report about the oracle
   until proven otherwise.

   **Proxy completeness is a hard requirement:** every unmodelled proxy operation must fail, and
   the oracle must emit the operations it did call for the Rust gate to assert. The vegetation
   oracle must cover random access, scheduled waterlogged-leaf updates, and brightness queries
   before further vegetation measurements are trusted. Mark the generated chunk as having reached
   the features stage before reading `MOTION_BLOCKING`; otherwise only generation heightmaps are
   maintained and a feature-placed block does not raise the surface.
2. **Same distribution, wrong draw count desyncs the whole RNG stream.** Replacing a trapezoidal
   integer provider with a uniform provider preserves mean and support but moves every downstream
   placement. Rule: reproduce sampling exactly, never approximate a
   provider; a parity failure far downstream of a new provider is an RNG-desync signature — audit
   draw counts first.
3. **Memo caches make generation-count gates vacuous.** `OverworldGenerator`'s 512-entry
   `pre_ore_cache` (and `post_ore_cache`) mean a second `column()` call may recompute nothing.
   Any gate that
   counts generation work constructs a **fresh generator per arm**.

### 4. The version seam — verdict on shape

**What the owner asked for is a generator-constructor seam, not a data-file seam.** Pre-1.18
worldgen is code-driven (different engine, not different JSON), so "generation for other versions"
can never be "swap the JSON bundle" — a future v1-9 generator is a second engine behind a common
trait. That trait already exists: `lodestone_server::ChunkSource` (`crates/lodestone-server/src/chunk.rs` —
`OverworldChunkSource` is the 26.2 implementation). The selection idiom already exists too:
`lodestone-registry`'s `SERVER_FAMILIES` table (`crates/lodestone-registry/src/lib.rs`),
where each feature-gated family entry carries constructor fns and consumers ask by protocol number,
never by name.

**Recommended shape — no third idiom:**

- Add one field to `ServerFamily`: `make_chunk_source: fn(i64 /*seed*/) -> Option<Box<dyn
  lodestone_server::ChunkSource>>` (Option because "can host" and "can generate" are different
  sets, exactly the `Family`/`ServerFamily` argument). The shell keeps asking by protocol number;
  `None` means "no world generation for this version" — surfaced, never routed around.
- Move `crates/lodestone-server/assets/worldgen/` + the `build.rs` embedding + `EmbeddedResolver`
  into `crates/versions/26.2`. This keeps behavior data selected with its protocol family and
  preserves the canonical-census-versus-behavior-data boundary:
  `lodestone-data` holds the *canonical censuses* of the one internal version, but
  per-version **behavior** data (like v1-9's flattening table) lives in its family crate — worldgen
  JSON is behavior data that must remain selectable. Follow v1-9's in-family table pattern and
  relocate the existing `lodestone-server` `build.rs` embedding.
- `lodestone-server` loses its unconditional 26.2 dependency and keeps only the version-free
  `ChunkSource`/`OverworldGenerator` plumbing. The architectural gate is a new required check
  mirroring `check-seam`: `cargo check -p lodestone-server --no-default-features` must compile with
  **no** worldgen data embedded — without it, the hardcoded dependency will creep back and nothing
  else will catch it (that is the whole lesson of the shell's seam check).
- `lodestone-worldgen` (the engine) does not move and does not change: it is already version-free
  by construction (data arrives through `Resolver`).

**Cost:** ~90 asset files + `build.rs` relocated; one registry field + one v26-2 entry; threading
the constructed `ChunkSource` through `integrated.rs`/`server.rs` (both currently owned by other
agents — sequence after they land); one new health-check line. **Not built now:** any second
generator. The seam is done when a grep for `assets/worldgen` in `lodestone-server` returns
nothing and the new check is green.

### 5. Agent-sized units, ownership, choke points

`overworld.rs` is the choke for every composition step: stage *engines* land in parallel (each in
its own new file under `crates/lodestone-worldgen/src/feature/` + its own oracle + tests), but the
few-line composition hook into `OverworldGenerator::column` is serialized — one unit at a time, or
the unit states the exact patch for the orchestrator. Broker edits to `crates/lodestone-server/src/lib.rs`,
`chunk.rs`, `server.rs`, `tick.rs`, `integrated.rs`, and `mobs/` before implementation.

| unit | delivers | owns exclusively | waits on |
|---|---|---|---|
| U1 harness expansion | 3×3 `postfeatures`, `postvegetation`, `final` stages; ~6 new fixture chunks / 2nd seed; anvil-based end-to-end control | `crates/lodestone-worldgen-parity/**`, `scripts/worldgen-oracle/ComposedChunkOracle.java` | anvil control half waits on `lodestone-anvil`'s reader landing |
| U2 freeze_top_layer | snow/ice engine + `TopLayerOracle.java` + 4 bit-exact fixtures + 4 observed controls; `lodestone_data::snow_support` (5 archive-derived columns); the **first release-profile** composed figure | `feature/top_layer.rs`, `noise/perlin_simplex.rs`, `TopLayerOracle.java`, `lodestone-data/{src,oracle-java}/snow_support*` | complete |
| U3 lakes + springs | `LakeFeature`/`SpringFeature` kinds + oracle | `feature/lake.rs`, `feature/spring.rs` (new) | same |
| U4 geodes, dungeons, fossils, icebergs | `GeodeFeature`, `MonsterRoomFeature`, `FossilFeature`, `IcebergFeature` + oracle | `feature/geode.rs`, `feature/misc_structures.rs` (new) | same |
| U5 vegetation gap burn-down | `multiface_growth` (every biome), fancy/giant trunk placers, `fallen_tree` | `feature/vegetation.rs`, `vegetation_parity.rs`, `VegetationOracle.java`, `KNOWN_VEGETATION_GAPS` | one agent only — all in one file |
| U6 ocean vegetation | kelp, seagrass, sea_pickle, coral (3 kinds) | `feature/ocean.rs` (new) | same overworld.rs hook |
| U7 3-D biomes | per-quart-cube climate sampling; a port of vanilla's own climate R-tree with its brute-force nearest-point search as its own in-tree control | `biome.rs`, `BiomeOracle.java` | perf gate from U9 before landing |
| U8 seam | §4 as written | `worldgen_data.rs`, `assets/worldgen/` move, v26-2 additions, registry field | `integrated.rs`/`server.rs` owners; lib.rs broker |
| U9 benches | `StageTimes` extended past its current 4 fields (shape/fluid_heightmap/surface/intern — it predates carve/ore/vegetation entirely); persisted split; RD 8/16/32 sweep with peak RSS via `lodestone-allocbench`'s `/usr/bin/time -l` pattern | `benches/generation.rs`, `column_timed`/`StageTimes` in overworld.rs (small brokered touch) | — |
| S1..Sn structures | salted placement, starts/references, templates, jigsaw, coded pieces, and terrain adaptation; retain and extend per-family gates | `crates/lodestone-worldgen/src/structure/` and `overworld/structures.rs` | incomplete-generator ledger and structure-positive outside-oracle coverage |

### 6. Performance: predicted, then measured

**A release-profile baseline now exists, from U2**: the composed `column()` costs **853.5 ms/chunk**
in release, measured over 8 chunks (snowy, frozen and warm) by
`worldgen_data::tests::freeze_stage_release_timing`. Every *other* number on record for the
composed pipeline is still **debug**-profile — the 144-chunk sweep at ~68 s pre-ore-composition and
700.57 s after (`overworld.rs` module doc), the dense-grid ~12.7 %/chunk win, the ore-sweep 700 s
figure. Debug timings are ordering evidence only; every claim must state its profile. U9 still owns
the full sweep and the persisted split.

`StageTimes` grew a `top_layer` field for this (it had four: shape / fluid_heightmap / surface /
intern), which is the mechanism U9 should extend rather than replace. **Note `column_timed` is a
second copy of the pipeline, not a wrapper** — adding a stage to `column()` and not to
`column_timed` makes the timed path silently diverge; a non-zero field for every composed stage is
therefore a required timing-gate assertion.

Predictions to verify (the *predict-then-measure* discipline, not direction-only):

- New decoration steps (U2–U4, U6) are per-chunk RNG walks over existing machinery: predicted
  <5 % each of composed column cost; verified by the extended `StageTimes` split. **U2 measured:
  2.196 ms/chunk = 0.257 %** of the composed column, release profile — inside the prediction by an
  order of magnitude, and the gate asserts the 5 % ceiling so a regression fails rather than being
  absorbed. Note `freeze_top_layer` needs no 3×3 driver at all (it writes only within its own
  chunk), so it is the *cheapest* shape in this group; U3/U4/U6 do spill and should not inherit
  this figure.
- 3-D biomes without an index: 16 → 1536 samples/chunk × 7594 rows ≈ **~96×** the biome stage.
  That is why U7 carries the climate R-tree port and a perf gate, with vanilla's own
  brute-force nearest-point search as the correctness control for the index.
- The 3×3 drivers' 9× recompute is already amortized by `pre_ore_cache`/`post_ore_cache` (512-entry
  FIFO) for sweep-shaped access; the open cost item is release-profile confirmation plus the
  benches unit's peak-RSS story (each cache entry is a ~200 KiB grid; two caches ≈ up to ~200 MiB worst case —
  measure, don't assume).

### 7. What "full parity" cannot mean yet — the honest scope

1. **No structure-positive whole-chunk bit-exact claim yet.** Structures now write into terrain,
   ores, and vegetation through the production pipeline, but incomplete generators remain ledgered
   and the final claim needs structure-positive outside-oracle coverage, including a fixture that
   compares the complete generated chunk.
2. **The ore stage cannot be gate-closed to zero** until U1's 3×3 `postfeatures` oracle exists —
   today's residual (2237/98304 at (0,0)) is dominated by real spill the single-source oracle
   stage structurally cannot see.
3. **Underground biome-dependent content is unmeasurable** until U7 — there is no 3-D biome for a
   gate to compare.
4. **Wire parity ≠ block parity**: served chunks carry empty heightmaps and `Missing` light
   (`encode_column_body`, `crates/versions/26.2/src/server_protocol.rs`), and fluids lack the `level` property. Real gaps, tracked, not
   part of block-field parity numbers.
5. **Two measured savanna residuals with unknown mechanism** (11 leaf-distance cells, 1
   `short_grass` cell) stand as open findings — report-shaped, not bug-to-route-around-shaped.
6. **Old-chunk blending and generation-time mob spawning are out of scope** (imported worlds;
   runtime spawner respectively).

### Keeping this plan current

The census, dependency graph, and ownership table are current-state claims. Re-verify their
production consumers and gates before dispatching work; replace obsolete rows rather than retaining
historical status notes. `freeze_top_layer` has a centre-only oracle because it consumes no RNG and
does not cross a chunk boundary; other vegetation steps need a 3×3 driver.

## How to change it

Re-verify the census table against the tree before dispatching from it. When a unit closes, update
its row and the prerequisite graph; when a new gap is found, add a row with evidence, not prose.
Fixture regeneration commands live in this plan and each oracle's own header.

## Configuration

`LODESTONE_REGEN=1` regenerates committed oracle fixtures; `LODESTONE_ORE_SINGLE_SOURCE_DEBUG=1` /
`LODESTONE_VEG_SINGLE_SOURCE_DEBUG=1` / `LODESTONE_CARVE_HASHMAP_DEBUG=1` narrow composed stages
for isolation (debug-only, never production paths). Oracles run through Apple `container` via
`scripts/worldgen-oracle/run.sh`.

## Dependencies

`crates/lodestone-worldgen` (engine), `crates/lodestone-worldgen-parity` (whole-chunk gate),
`crates/lodestone-server` (`worldgen_data.rs` embedding — until U8 moves it), `scripts/
worldgen-oracle/` plus the locally inspected 26.2 runtime package (oracles), `lodestone-registry` (U8's
selection table), `lodestone-anvil` (the author-independent control, once its reader lands).
