# World generation: plan, and a correction to the brief

**Correction first, because it changes everything downstream.** The brief states "there is no
terrain generation at all" and asks to scope worldgen as the largest net-new area. That premise is
stale — this is exactly `CLAUDE.md`'s "issue tracker lags the tree" trap, at brief-writing scale.
`crates/lodestone-worldgen` (6,351 lines src / 2,600 lines tests) already exists, is wired into
`lodestone-server`, and is served over the real v770 wire protocol today. What follows is a plan for
the *remaining* ~40% (biomes, caves, ores, and a stopping-point call on structures), not a from-zero
plan. Section 0 documents the correction with evidence; sections 1–8 answer the brief's numbered
questions against that corrected baseline.

---

## 0. What exists today (evidence)

**The crate and its parity discipline.** `crates/lodestone-worldgen/src/lib.rs:1-7` describes itself
as a "version-free Minecraft Java Edition world-generation engine … no version-specific data,"
parameterised by JSON a version crate supplies. Modules: `rng/`, `hash/`, `noise/`, `density/`
(noise-router DAG interpreter, `Density` enum with ~28 variants at
`crates/lodestone-worldgen/src/density/mod.rs:207-247`+), `surface/` (surface rules), `aquifer/`
(the real, non-approximated aquifer), `carver/` (caves/ravines), `feature/` (ore placement),
`overworld.rs` (the composed driver).

**JVM-oracle parity, measured, not claimed** (`DESIGN.md:1945-1949`, `scripts/worldgen-oracle/*.java`,
run via `docker run --rm eclipse-temurin:25-jdk` per `scripts/worldgen-oracle/run.sh:1-20` — the exact
pattern the brief describes, already in use, not something to invent):

| stage | result |
|---|---|
| RNG | 663/663 bit-exact |
| noise | 1224/1224 bit-exact |
| density router | 5120/5120 bit-exact |
| noise router, whole region | 34048/34048 = 100.0000% (`DESIGN.md:859-861`) |
| interpolated `final_density`, whole chunk | 98304/98304 = 100.0000% (`DESIGN.md:1029-1038`) |
| carvers | 98304/98304 × 2 chunks (`DESIGN.md:1947`) |
| ore features | whole-chunk exact **both directions**, 3 fixtures / 2 seeds / 2 terrain profiles (`DESIGN.md:1949`) |
| surface rules | block-for-block, own oracle (`crates/lodestone-worldgen/src/surface/mod.rs:21-27`) |

None of this is `decode(encode(x))==x` self-play: every comparison is against the **running compiled
26.2 server's own methods**, called through the oracle classes in `scripts/worldgen-oracle/` — e.g.
`DESIGN.md:859` states it precisely: "the Rust interpreter reads disk JSON, while the oracle evaluates
the running server's live `RandomState` router… If disk JSON were an incomplete picture, the two would
diverge. They agree."

**What's actually served, right now.** `crates/lodestone-server/src/worldgen_data.rs:100-108`
(`overworld_generator`) builds an `OverworldGenerator` from JSON embedded at build time
(`crates/lodestone-server/build.rs:1-15`, from `crates/lodestone-server/assets/worldgen/`, 90+ files).
`crates/lodestone-server/src/chunk.rs:261-308` (`OverworldChunkSource`) wraps it as the server's
`ChunkSource`, with edit retention. `crates/lodestone-server/src/worldgen_data.rs:182-226`
(`chunk_source_serves_generator_block_for_block`) proves the served chunk matches the generator
block-for-block, non-vacuously (asserts water and non-stone-surface material are both present). The
real v770 wire encoder is already the consumer: `crates/protocol/v770/src/server_protocol.rs:915`
(`encode_chunk`) — not a stand-in. `crates/lodestone-shell/src/worldgen.rs:43,68-69` shows the shell's
local-world path calls the same `overworld_generator`, so direct-singleplayer and
server-then-loopback are not two generators.

**The honest gap, in the code's own words.** `crates/lodestone-worldgen/src/overworld.rs:29-41`:

> "Not yet composed here: carvers (no caves), the real aquifer, and features (no ores/vegetation/trees)... The multi-noise biome source is not built yet, so generation runs a single fixed biome."

So `OverworldGenerator::column` today runs shape → sea-level-only fluid fill (a documented
*approximation* of the aquifer, not the real one, `overworld.rs:18-24`) → surface rules, under one
hardcoded biome (`minecraft:plains`, `crates/lodestone-server/src/worldgen_data.rs:35-36`,
`DEFAULT_BIOME_SNOWS = false`). That is a genuine, walkable, real-shaped overworld with correct
terrain contours, oceans, beaches, and grass/dirt/sand/gravel surfacing — but flat-biome, cave-free,
ore-free, structure-free.

**Where the container/lighting boundary actually is** (the brief's item 1). `lodestone-world` (a
different crate from `lodestone-worldgen`) supplies the storage and lighting the brief names:
`crates/lodestone-world/src/column.rs:26` (`ChunkColumn`, the palette/section container),
`crates/lodestone-world/src/heightmap.rs:110` (`Heightmaps`), `crates/lodestone-world/src/light.rs:299`
(`ColumnLight`), `crates/lodestone-world/src/lighting.rs:167` (`compute_column_light`). **This is a
third, distinct `ChunkColumn`** from `lodestone-worldgen::overworld::GeneratedColumn` (the raw
generator output) and `lodestone-server::chunk::ChunkColumn` (the server's edit-tracking wrapper,
`crates/lodestone-server/src/chunk.rs:88-97` — a simple flat palette+index grid, not
`lodestone-world`'s richer section container). **These three do not currently unify**: the server path
generates via `OverworldGenerator` → `GeneratedColumn` → its own light-weight `ChunkColumn` → the wire,
never touching `lodestone-world::ChunkColumn`/`Heightmaps`/`ColumnLight` at all for served terrain. So
the brief's framing ("container and lighting exist, generation didn't") had the right instinct but the
wrong crate: `lodestone-world` provides container+lighting for **loaded/decoded** chunks (the client
side, receiving chunks over the wire and needing local light for rendering); the **server's own**
generation path uses a simpler purpose-built column type and does not compute or send per-block light
at generation time — light is expected to be computed client-side from the decoded chunk (this is
vanilla's own division of labour: the server sends blocks, the client lights them locally for
rendering, though vanilla *also* computes and ships lighting data on the wire, which is a separate,
already-tracked gap outside this plan's scope — check before assuming; not verified further here since
it is not a worldgen question).

**Existing GitHub issues that already scope adjacent work** (checked so this plan does not duplicate
them — `HANDOFF.md`'s standing rule):

- **#295** "Chunk lifecycle: wire carver and ore-feature placement into the served chunk pipeline"
  (open, `tier-4`, `island`, parent #5) — already contains the precise diagnosis this plan would
  otherwise re-derive: carver/feature modules exist and are proven, `OverworldGenerator` imports
  neither, `grep` for their symbols outside their own modules is empty, and the wiring is "the single
  cheapest, highest-visual-impact win in the whole persistence/chunk area." This plan's Phase 2
  **is** #295; do not open a duplicate.
- **#132** "Design: custom world generator hook for plugins" (open, `question`) — plugin-API surface
  over the generator, not core fidelity. Relevant to §6 below (crate boundary) but a separate decision.
- **#136** "Structure placement API for plugins" (open) — explicitly recorded as blocked on core
  worldgen gaining any structure concept at all, and says "do not start implementation against this
  issue." Confirms structures are unscoped anywhere in the tracker today.
- **#134** "Custom dimension registration from a plugin" (open) — adjacent, not overlapping.
- **#85/#86/#87** — worldgen bench issues (stage-cost split, parallel scaling, region throughput),
  already filed, orthogonal to the correctness phases below.
- No open or closed issue mentions multi-noise biome assignment specifically. That is a real gap in
  the tracker, addressed by the new issue in §9.

---

## 1. Vanilla 26.2's shape (from `.cache/mc/26.2/src/`, names real — 26.2 ships de-obfuscated)

Already-built pieces map to:

- **Noise router / density functions**: `net/minecraft/world/level/levelgen/DensityFunction.java`,
  `DensityFunctions.java`, `NoiseRouterData.java` — ported as `Density` (the ~28-variant enum,
  `density/mod.rs:207+`) interpreted by `Builder`/`NoiseChunkSampler` (`density/chunk.rs`).
- **`NoiseGeneratorSettings`**: `net/minecraft/world/level/levelgen/NoiseGeneratorSettings.java` — the
  per-dimension settings record (`noise` block/height, `sea_level`, `default_block`/`default_fluid`,
  `noise_router`, `surface_rule`, `disable_mob_generation`, `aquifers_enabled`). Consumed today via
  `overworld_settings()` (`crates/lodestone-server/src/worldgen_data.rs:85-91`) from
  `noise_settings/overworld.json`.
- **Surface rules**: `net/minecraft/world/level/levelgen/SurfaceRules.java`,
  `SurfaceSystem.java` — ported as `SurfaceSystem` (`crates/lodestone-worldgen/src/surface/mod.rs`).
- **Aquifers**: `net/minecraft/world/level/levelgen/Aquifer.java` — ported
  (`crates/lodestone-worldgen/src/aquifer/mod.rs`), proven, **not composed into `OverworldGenerator`**.
- **Carvers**: `net/minecraft/world/level/levelgen/NoiseBasedChunkGenerator.java:304`
  (`applyCarvers`), `net/minecraft/world/level/levelgen/carver/*Carver.java` — ported
  (`crates/lodestone-worldgen/src/carver/mod.rs`), proven, **not composed**.
- **Ore/feature placement**: `net/minecraft/world/level/levelgen/feature/OreFeature.java` and the
  placement-modifier pipeline — ported for ores (`crates/lodestone-worldgen/src/feature/mod.rs`,
  `OreConfig`/`place_ore_feature`), proven, **not composed**. Vegetation/tree features are unbuilt
  (the module doc says "and, later, vegetation and trees," `feature/mod.rs:2`).
- **Biome assignment**: `net/minecraft/world/level/biome/Climate.java` (`ParameterPoint`,
  `TargetPoint`, 7-dimension `Parameter` space: temperature, humidity, continentalness, erosion,
  depth, weirdness, offset — `Climate.java:29-30,42-70`), `MultiNoiseBiomeSource.java` (95 lines,
  nearest-point search via `Climate.Sampler.findValue`), and — the part that matters for scoping —
  `OverworldBiomeBuilder.java` (**1124 lines**), which *procedurally constructs* ~700+
  `(ParameterPoint, Biome)` pairs in Java code via nested loops over discretized climate bands
  (`OverworldBiomeBuilder.java:36-50` shows the `temperatures`/`humidities` arrays that seed the
  construction). **Unbuilt anywhere in Rust.**
- **Structures**: `net/minecraft/world/level/levelgen/structure/{Structure,StructureSet}.java`,
  jigsaw/template-pool assembly, `AncientCityStructurePieces.java` etc. — **entirely unbuilt**, no
  Rust module, no design doc section beyond acknowledging the gap.

---

## 2. Data vs. code — and the highest-value finding

`DESIGN.md:857` (§12.30) already established the headline: vanilla ships worldgen as **963 JSON
files** — 35 `density_function`, 63 `noise`, 7 `noise_settings`, 66 `biome`, 226
`configured_feature`, 262 `placed_feature`, 188 `template_pool`, 54 `structure(_set)`, 40
`processor_list`, 4 `configured_carver` (counts re-verified directly against
`.cache/mc/26.2/src/data/minecraft/worldgen/` in this session — `biome` 66, `configured_feature` 226,
`placed_feature` 262, `template_pool` 188, `structure` 34, `structure_set` 20, `processor_list` 40,
all matching). This is why the *engine* is ~700 lines of interpreter per stage, not a 10k-line port,
and it is already the architecture in use.

**The one piece that looked like data and isn't — and the cheapest-in-the-plan finding.**
`data/minecraft/worldgen/multi_noise_biome_source_parameter_list/overworld.json` is **two lines**:

```json
{ "preset": "minecraft:overworld" }
```

The actual ~700 `(ParameterPoint, Biome)` pairs are **not** in that file or any other JSON — they are
built at runtime by `OverworldBiomeBuilder.java` (1124 lines of Java, referenced above), registered
through `MultiNoiseBiomeSourceParameterLists.java:10-17` (`OVERWORLD` preset). Porting that builder
faithfully (nested loops, named climate bands, special-cased river/badlands/frozen-ocean logic) would
be a meaningful, error-prone transliteration effort — exactly the kind of thing `CLAUDE.md` warns is
easy to get subtly wrong with no single test able to say why.

**It doesn't need to be ported.** `MultiNoiseBiomeSourceParameterLists.OVERWORLD` is a registry object,
reachable the same way every other oracle in this repo reaches its data: boot the game
(`SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();`), resolve the built
`MultiNoiseBiomeSourceParameterList` for the overworld preset, and walk its resolved
`List<Pair<Climate.ParameterPoint, Holder<Biome>>>` — a public field/accessor already used by
`MultiNoiseBiomeSource.java`'s own constructor. Dump each entry's 7 climate parameters (as `[min,max]`
spans per vanilla's `Climate.Parameter`) plus the resulting biome id, once, to a flat JSON/table file,
following the exact `LODESTONE_REGEN=1` generate-or-assert pattern `crates/protocol/v770/tests/{collision_shapes,hardness}.rs`
already establish. **~700 code-only rows become one static data table**, and the 1124-line builder is
never transliterated at all — only the nearest-neighbour search (Climate.java's nearest-point-in-7D
logic) is code, and it's small: vanilla's own `RTree.findValueBruteForce`
(`Climate.java:182`) is the un-optimized reference implementation sitting right next to the RTree,
i.e. vanilla ships its own "spec example" for the brute-force version, satisfying the evidence
standard for free. The RTree itself is a JVM performance optimization over an O(n) brute-force search
of ~700 points — safe to skip per "never transliterate," since a few hundred squared-distance
comparisons per column is fast in Rust and produces bit-identical results to the indexed search (same
nearest point, different lookup structure).

**Everything else needed by the phases below is genuinely data**: `configured_carver` (4 files,
already consumed as JSON per `carver/mod.rs`'s `CarverConfig::parse`), `configured_feature` +
`placed_feature` (488 files total for the ore subset already in use, plus the vegetation/tree subset
Phase 3 would need), and `biome/*.json` (66 files: climate-independent per-biome data — surface
builder reference already resolved via `surface_rule`, ambient/mob-spawn tables out of scope here).
Structures' `structure_set`/`structure`/`template_pool`/`processor_list` (282 files total) are data
too, but the *placement machinery* (jigsaw assembly, piece collision, NBT template pasting) is
substantial unported code — this is why §5's recommendation excludes structures.

---

## 3. Phased plan

Each phase below ends at something on-screen/on-server, per `CLAUDE.md` rule 1. Phases 1–2 are
independently landable in either order relative to each other's *code*, but biome (Phase 1) should
land first because Phase 2's carvers/features are biome-parameterised in vanilla (which carver set and
which ore list a chunk gets depends on its biome) — landing Phase 2 first means re-touching the same
composition call for both.

### Phase 0 — baseline (already done, included for completeness of the plan's numbering)
**State:** shape + sea-level-fluid + surface, one fixed biome, served over the real wire protocol.
**Gate:** already passing — `worldgen_data.rs`'s `chunk_source_serves_generator_block_for_block` and
the whole JVM-parity suite in §0's table.

### Phase 1 — real biome variety
**Deliverable:** `MultiNoiseBiomeSource`-equivalent: per-column climate sample (temperature, humidity,
continentalness, erosion, depth, weirdness from the existing noise router's climate outputs — these
noises are *already computed* for shape, per `overworld.rs`'s doc on `final_density`'s dependencies)
→ nearest-parameter-point lookup against the dumped overworld table (§2) → real biome id per column,
threaded into `SurfaceSystem::build_surface` (which already accepts a `biome` parameter,
`overworld.rs:78-85`) and, once Phase 2 lands, into carver/feature selection. **Observable:** flying
over a singleplayer world shows visibly different biomes — plains next to desert next to taiga — with
correct surface materials per biome (snow in cold biomes, sand in desert, etc.), not one uniform
plains everywhere.
**Cost note:** this is the phase §2's finding makes cheap. The search algorithm is ~50 lines; the data
is one oracle dump.

### Phase 2 — caves, real aquifer, ores (this is issue #295 — do not re-file)
**Deliverable:** compose `carver::apply_carvers`, the real `aquifer::AquiferSystem` (replacing the
sea-level approximation), and `feature::place_ore_feature` into `OverworldGenerator::column`, in
vanilla's own order (`ChunkGenerator.applyCarvers` before decoration; `DESIGN.md`'s vanilla-order note
in `feature/mod.rs`'s doc comment). **Observable:** walking underground finds real cave systems and
ravines instead of solid stone; mining exposes actual ore veins (coal/iron/copper/gold/diamond/etc.,
including the buried-ore RNG-draw subtlety `DESIGN.md:1957` already caught and fixed once); underground
water/lava pockets appear where the real aquifer places them instead of only at sea level.
**Note:** all three math engines are already proven bit-exact in isolation (§0's table) — this phase
is pure composition risk (ordering, biome threading from Phase 1, RNG-seed derivation per chunk), not
new algorithm risk. #295's own text calls this out: "the hard part... is done and proven... find the
seam."

### Phase 3 — vegetation and tree features (recommended stopping point before this — see §5)
**Deliverable:** extend `feature/mod.rs`'s placement-modifier interpreter (already handles the ore
case's modifier pipeline generically) to the `configured_feature` kinds trees and grass/flowers use
(`TreeFeature`, `RandomPatchFeature` roughly), and wire per-biome feature step lists.
**Observable:** forests have trees, plains have grass and flowers, deserts have cacti — a world that
looks alive rather than bare terrain.
**This is where §5 recommends stopping**, argued below.

### Phase 4 (past the recommended stop) — minimal structures
**Deliverable, if pursued:** a narrow structure subset (e.g. villages or a single simple structure
type) via `StructureSet` placement-grid RNG + template-pool NBT pasting, *not* the full jigsaw system.
**Observable:** a specific structure type appears at a deterministic seed location, block-for-block
matching vanilla.
**Cost:** large — jigsaw assembly, piece-collision resolution, and NBT structure-file loading are all
unported. This is why #136 records "do not start implementation" until this has a scope.

### Phase 5 (further out) — full structures, full feature catalogue, world-preset variety (nether/end)
Not scoped here; see §5's cost/benefit discussion.

---

## 4. Verification strategy per phase

The evidence standard already established for this crate (and to keep using, per `CLAUDE.md`'s "an
expected value must originate outside the code under test") is: **vanilla's own generated output for
a known seed**, captured either via the JVM-oracle pattern (`scripts/worldgen-oracle/`, boots the real
26.2 classes headlessly) or via one of the three live oracles already running (`terrain.sh` on `:25580`
exists, per its own comment, "for light gates" — equally usable here by generating chunks at a fixed
seed via RCON and reading them back with a world/region-file inspector or a debug command). Do **not**
re-verify with `decode(encode(x))==x` against our own encoder — that trap is explicitly named in
`CLAUDE.md` and worldgen is exactly the shape it warns about.

- **Phase 1 (biome):** two-part gate. (a) The dumped parameter table itself: assert row count matches
  the oracle's own count and spot-check known biomes at known climate points (e.g. the origin at a
  fixed seed) against `/data get` or a `minecraft:locate biome` RCON call on the terrain oracle
  (`terrain.sh`, `:25580`/`:25581`). (b) A whole-region biome-assignment parity test structured like
  `region_parity.rs`: for a fixed seed and chunk range, dump vanilla's per-column biome via a small new
  oracle class (`BiomeOracle.java`, same `run.sh` pattern) and assert exact biome-id match, not just
  "some variety appeared" — a control that only checks "more than one biome present" cannot catch a
  boundary that's off by one climate band, matching the "vacuous test" species table's *magnitude* row
  (right direction, wrong number). The anti-vacuity floor: assert at least N distinct biomes across the
  probed region (N derived from the actual oracle output, not guessed).
- **Phase 2 (carvers/aquifer/ores):** the composition-only gate #295 already specifies — generate a
  chunk with `OverworldGenerator::column` post-composition and diff it block-for-block against
  `RegionOracle`/`DensityChunkOracle`-style full-chunk dumps (extending the existing oracle classes
  rather than writing new math, since the math itself already passed `carver_parity`/`feature_parity`/
  `aquifer_parity` in isolation). The two-directional ore check already used (`DESIGN.md:1949`, "exact
  BOTH directions") is worth keeping as a pattern: compare Rust→JVM and, separately, assert the JVM
  oracle's own ore *count* per chunk matches a hand-derived expectation, so a systematic RNG-order bug
  that shifted every ore by one call (which would still "match" a same-order oracle-vs-Rust diff if
  both were wrong the same way) is caught by an independent count. Live spot-check: mine into a cave on
  the `terrain.sh` oracle at a known seed/coordinate and compare block-for-block against the same
  coordinate generated by lodestone-server for that seed.
- **Phase 3 (features):** same whole-chunk block-for-block diff, extended to the tree/vegetation
  `configured_feature` set. Add an aggregate-statistics gate alongside exact-match (tree count per
  biome type within an expected band) — `DESIGN.md:1957`'s lesson is that exact-match on one chunk
  catches ordering bugs while count bands catch a plausible-but-wrong distribution, and neither
  substitutes for the other.
- **Phase 4 (structures), if pursued:** block-for-block diff of the placed structure's full bounding
  volume against vanilla's own generation at the same seed/coordinate — captured via the terrain
  oracle's RCON (`/locate structure`, then read back the chunk region) rather than any transliteration
  of the jigsaw algorithm's *expected* output, since a self-authored placement algorithm "validates the
  behaviour you chose to model," per `CLAUDE.md`'s evidence-standards section.

Anti-vacuity discipline to carry into every one of these (already the house style —
`checked == 16*16*height` assertions appear throughout the existing test files): every diff loop must
assert it actually visited every probed cell, and every "no X" assertion (no cave here, no structure
here) needs a positive control chunk where X is known present, so an always-passing comparator can't
hide behind "nothing to compare."

---

## 5. What not to build, and the recommended stopping point

**Recommendation: stop after Phase 3 (noise terrain + real biomes + caves/aquifer/ore features +
vegetation), skip structures entirely for now.**

Reasoning:

- **Cost asymmetry is stark.** Phases 1–3 compose or lightly extend machinery that is *already built
  and already proven bit-exact* — §0's table shows the hard numerical work (noise router, density,
  carvers, aquifer, ore placement) is done. The remaining work is wiring (Phase 2, already scoped as
  #295 and correctly labelled "cheapest, highest-visual-impact win") and one new but bounded subsystem
  (Phase 1's climate search, made cheap by §2's oracle-dump finding; Phase 3's vegetation placement,
  which reuses Phase 2's placement-modifier interpreter rather than needing a new one). Structures are
  a different order of magnitude: jigsaw assembly, piece-collision resolution against already-generated
  terrain, NBT structure-file parsing and pasting, and per-structure-type special-casing (villages,
  ancient cities, trial chambers each have bespoke piece logic per the decompiled source file list in
  §1) — a second full subsystem roughly the size of everything built so far, for content
  (villages, temples, strongholds) that a "plausible world a player can survive in" does not need.
- **The brief's own framing supports this.** "A plausible-looking world a player can survive in" is
  fully satisfied by Phase 3: correct terrain shape, correct oceans/beaches, correct biome variety,
  explorable caves, mineable ores, and a world that looks populated (trees, grass) rather than bare
  stone. A player cannot tell from moment-to-moment play that villages are absent; they can immediately
  tell if terrain is flat, monotonous, or has a bare-stone underground.
- **The tracker already treats structures as out of scope.** #136 explicitly says "do not start
  implementation against this issue" pending core worldgen structure support, and no issue anywhere
  scopes vanilla (non-plugin) structure generation. This plan should not be the first to open that
  door without a much larger, separately-argued case.
- **Multi-dimension (nether/end) generation** is out of scope for the same reason — it's `noise_settings`
  variety plus nether-specific biome source (`BiomeSources.java` shows a different, simpler biome
  source for the nether — checkerboard-adjacent, not full multi-noise) — worth a follow-up plan once
  the overworld reaches Phase 3, not before. `docs/backlog.md`'s tier framing (issue #330, "multi-
  dimension support and server-driven portal travel") already tracks the prerequisite (portal travel,
  dimension switching) as separate, larger, tier-4 work.
- **What Phase 4+ would cost if pursued anyway:** roughly bound it by the data-file count difference —
  282 structure-related files (`structure_set` 20, `structure` 34, `template_pool` 188,
  `processor_list` 40) versus curernt ~90 files embedded — a 3x+ data surface increase, plus new
  algorithm classes (jigsaw graph solving, bounding-box collision) that have no existing Rust
  counterpart to extend, unlike Phase 3's reuse of Phase 2's placement-modifier machinery.

---

## 6. Crate boundaries and the version seam

**Current state is already correct and should not change shape, only grow.** `lodestone-worldgen`
(`src/lib.rs:1-7`) is explicitly and deliberately version-free: "no version-specific data… Dropping a
version drops its data, never this engine." It depends on nothing version-specific — `serde_json` for
the data shape, and its own `rng`/`hash`/`math`/`noise` primitives. This is the right split and Phases
1–3 do not disturb it: Phase 1's biome search is climate-math over a data table, same pattern as
existing density/surface interpretation; Phase 3's feature interpreter extends the existing
placement-modifier engine, which is already version-free.

**Where the 26.2 data lives today, and why that's a seam smell worth flagging (not fixing in this
plan).** `crates/lodestone-server/assets/worldgen/` + `worldgen_data.rs`'s `EmbeddedResolver` hold the
actual JSON, embedded via `build.rs` directly into `lodestone-server` — **not** into
`crates/protocol/v770`, where the rest of 26.2-specific data lives (block registries, collision
shapes, hardness tables, per `CLAUDE.md`'s data-sources section). `worldgen_data.rs:10-16`'s own doc
comment already flags this as provisional: "Per plan §3 version-specific worldgen data eventually
lives in the version crate… This bundled copy is the singleplayer default." Concretely,
`lodestone-server` currently has a compile-time dependency on 26.2 data that has no version gate — it
is not behind the `live`/`--no-default-features` seam `CLAUDE.md` requires the shell to respect, and
`lodestone-server` is depended on by *both* the shell (singleplayer) and the standalone dedicated-
server binary, so today the integrated/dedicated server can only ever generate 26.2 terrain, for any
protocol family it might otherwise be built to speak. **This plan does not fix that** — it is real
scope, but it is a refactor of already-shipped, already-verified code, not a new-terrain-generation
task, and doing it well needs the same multi-version-data mechanism the block/item/collision data
already uses elsewhere (check how `lodestone-v770`'s own registry data is structured and mirror it,
rather than inventing a second pattern). **Recommendation:** file it as its own follow-up (see §9) —
move `assets/worldgen/` and `EmbeddedResolver`'s data (not its code) into `crates/protocol/v770`,
behind the same feature-gate discipline as the rest of that crate's data, with `lodestone-server`
depending on it only when a version family providing worldgen data is compiled in. Until that lands,
every phase above should still be built inside `lodestone-worldgen` (version-free engine) +
`crates/lodestone-server/assets/worldgen/` (26.2 data, wrong long-term home but the *only* home today,
consistent with where Phase 0's data already sits) rather than inventing a third location.

---

## 7. Anything cheaper than expected

Already covered in depth in §2, restated as the headline: **the multi-noise biome parameter list looks
like it needs a 1124-line Java class ported, and it does not** — `OverworldBiomeBuilder.java` builds
the table procedurally, but the *result* is a static ~700-row table reachable by dumping the resolved
registry object through the same oracle pattern already in `scripts/worldgen-oracle/`. This turns the
single scariest-looking remaining subsystem (biome assignment, the thing the brief specifically calls
out as "the crux") into: one oracle class (~30-50 lines, following `RegionOracle.java`'s existing
shape), one generated data table, and a small (~50-100 line) nearest-neighbour search — with vanilla's
own `Climate.RTree.findValueBruteForce` serving as the un-optimized reference algorithm, so there is
no ambiguity about what "correct" means even before the oracle dump exists.

A secondary, smaller instance: **the placement-modifier interpreter Phase 3 needs already exists** —
`feature/mod.rs`'s doc comment (`feature/mod.rs:20-27`) describes a general `Stream`-of-positions
composition model already implemented for ores. Vegetation features (trees, patches) use the same
placement-modifier pipeline in vanilla with different modifier *kinds*, not a different composition
model — so Phase 3 is closer to "add more `configured_feature`/modifier variants to an existing
interpreter" than "build a second feature system."

---

## 8. Summary answers to the brief's items

1. **Existing/missing boundary:** shape + sea-level-fluid-approximation + surface rules are composed
   and served (`overworld.rs`); real aquifer, carvers, ore features exist and are proven but
   uncomposed (`overworld.rs:29-34`, tracked as #295); biome assignment (multi-noise climate → real
   biome variety) is entirely unbuilt; vegetation features and structures are entirely unbuilt.
   `lodestone-world`'s `ChunkColumn`/`Heightmaps`/`ColumnLight`/`compute_column_light` are a *different*
   crate serving the client's decode/render/light path, not currently touched by the server's
   generation path at all (three distinct `ChunkColumn` types across three crates, see §0).
2. **Vanilla's shape:** mapped in §1, class-by-class, against what's already ported.
3. **Data vs. code:** §2 — almost everything is JSON (963 files across categories), engine is ~700
   lines/stage; the one deceptive case (biome parameter list, looks like code, is dumpable) is §7's
   headline finding.
4. **Phased plan:** §3, five phases, each with an observable end-state; Phase 0 already done, Phase 2
   already tracked as #295.
5. **Verification per phase:** §4 — vanilla-generated-output comparison throughout, never
   self-referential, extending the existing `scripts/worldgen-oracle/` pattern per phase.
6. **Crate boundaries / version seam:** §6 — `lodestone-worldgen` stays version-free and is the right
   home for all engine growth; the 26.2 *data*'s current home in `lodestone-server` (not
   `crates/protocol/v770`) is a pre-existing seam gap this plan flags but does not fix, with a
   follow-up issue recommended.
7. **What not to build / stopping point:** §5 — stop after Phase 3 (terrain + biomes + caves + ores +
   vegetation); skip structures and additional dimensions as a substantially larger, separately-scoped
   effort with low incremental survivability benefit.
8. **Cheaper than expected:** §7 — the biome parameter list.

---

## 9. Issues created

See the companion report for the exact numbers filed. In summary: one epic tying together the
worldgen-completion phases not already tracked, plus child issues for Phase 1 (biome), Phase 3
(vegetation features), the crate-boundary/version-seam follow-up (§6), and a pointer comment added to
existing #295 (Phase 2) and #136 (structures, confirming it should stay blocked) rather than new
issues for those two, since they already exist and are already correctly scoped.
