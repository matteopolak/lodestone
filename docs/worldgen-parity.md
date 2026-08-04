# Worldgen parity harness

## What it is

`crates/lodestone-worldgen-parity` is the shared chunk-for-chunk comparison
harness against a real vanilla 26.2 server, for every worldgen phase (epic
`#404`: biomes `#405`, carvers/aquifer/ores `#295`, vegetation `#406`) to
point at instead of improvising its own oracle diff. It answers one question:
**for a fixed seed and named chunk coordinates, how close is
`lodestone_server::overworld_generator`'s output to what a real vanilla 26.2
server actually generates, block for block, and *where* does it differ?**

It is infrastructure, not a feature test for any one phase — it does not
belong to `#405` or `#295`, it is what both should be measured against.

## How it works

```
scripts/worldgen-oracle/ComposedChunkOracle.java   (runs in Docker, dumps vanilla's own output)
        │  scripts/worldgen-oracle/run.sh ComposedChunkOracle
        ▼
crates/lodestone-worldgen-parity/src/bin/regen.rs  (parses + run-length-encodes)
        │  writes
        ▼
crates/lodestone-worldgen-parity/fixtures/composed_seed42.txt   (committed, 412 KB)
        │  read by
        ▼
crates/lodestone-worldgen-parity/tests/chunk_parity.rs  (the gate)
crates/lodestone-worldgen-parity/src/bin/compare.rs      (the one-command report)
```

### The oracle: what it actually composes

`ComposedChunkOracle.java` boots the real 26.2 server headlessly
(`SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();`, the same
pattern as every oracle in `scripts/worldgen-oracle/`) and drives
`NoiseBasedChunkGenerator`'s own public methods, in vanilla's own order:

1. `fillFromNoise` — shape + the **real** `Aquifer` (not an approximation).
2. Per-quart biome resolution, using the **real** `MultiNoiseBiomeSource`
   built from the same 7594-row overworld parameter table
   `BiomeOracle.java`'s `table` mode dumps (`MultiNoiseBiomeSourceParameterList.knownPresets()`)
   — not a biome pinned to a constant the way the other isolated oracles in
   this directory do (`CarverOracle.java`, `SurfaceOracle.java` both use
   `FixedBiomeSource`). This is the one thing this oracle does that none of
   the others do: real per-column biome variety feeding real biome-dependent
   surface/carver selection.
3. `buildSurface` — dumped as the **`postsurface`** stage.
4. `applyCarvers` — replicating `NoiseBasedChunkGenerator.applyCarvers`'s own
   per-source-chunk `carverBiome` resolution (each of the 17×17 source chunks
   in the carve neighbourhood gets its *own* biome, sampled at
   `(QuartPos.fromBlock(sourceChunk.minBlockX), 0, QuartPos.fromBlock(sourceChunk.minBlockZ))`
   — vanilla's real convention, replicated exactly, not simplified) — dumped
   as the **`postcarve`** stage.

Both stages are dumped as a full `16 × 384 × 16` block-state array per chunk,
canonicalised the same way every other oracle here does (`Block registry
id[sorted properties]`).

**Not composed by this oracle**: ore/vegetation features, structures. See
[What could not be isolated](#what-could-not-be-isolated-and-why) below.

### The fixture format

The raw oracle stdout is ~10.5 MB per chunk (98304 explicit
`postsurface.x,y,z <state>` / `postcarve.x,y,z <state>` lines). Real terrain
is mostly vertical runs of one block, so `regen`'s `encode_compact` run-length
-encodes each of the 256 columns per stage into `y_start count block_state`
triples before writing the committed fixture. Measured: **21,210,085 bytes
(20.2 MB) raw → 421,965 bytes (412 KB) committed**, a ~50x reduction, still
plain diffable text (`git diff -- crates/lodestone-worldgen-parity/fixtures/`
shows real per-run changes, not a binary blob).

### Seed determinism

Every oracle in `scripts/worldgen-oracle/` — this one included — passes a
plain `long seed` straight to `RandomState.create(provider,
NoiseGeneratorSettings.OVERWORLD, seed)`, and
`lodestone_worldgen::overworld::OverworldGenerator::new` takes the identical
`i64` straight through to its own `Builder::new(seed, resolver)`. There is
**no string-seed hashing layer** in play here — that hashing (Java's
`String.hashCode`-based conversion) only applies to a human-typed "Seed" field
in vanilla's world-creation UI, upstream of everything either side of this
harness consumes. Seed `42` is used because every other oracle in this
directory already does, so a reader cross-referencing `DensityChunkOracle`,
`CarverOracle`, `SurfaceOracle`, `BiomeOracle` and this one is comparing the
same world throughout.

**To pick a new seed**: change `long seed = 42L;` in
`ComposedChunkOracle.java`'s `main`, regenerate
(`cargo run -p lodestone-worldgen-parity --bin regen`), and update the
`ceiling`/`floor` constants in `tests/chunk_parity.rs` (they're a `match` with
no wildcard arm — a fixture chunk with no recorded threshold panics loudly
rather than silently skipping calibration).

## Measured current parity (seed 42)

Two named chunks, chosen to match coordinates other oracles in this directory
already use (`carver_parity`'s "ocean chunk (0,0)" / "land chunk
(-120,-120)"), so this fixture set is drawn from already-characterised,
non-degenerate columns.

**Before issue #295** (shape + sea-level fluid approximation + biome + surface;
no carvers, no real aquifer):

| chunk | stage | match | real mismatches | property-only mismatches |
|---|---|---|---|---|
| (0, 0) | postsurface (composed-then subset) | 93160/98304 (94.77%) | 316 | 4828 |
| (0, 0) | postcarve (full, minus features/structures) | 90100/98304 (91.65%) | 3376 | 4828 |
| (-120, -120) | postsurface | 93394/98304 (95.01%) | 4910 | 0 |
| (-120, -120) | postcarve | 93053/98304 (94.66%) | 5251 | 0 |

**After issue #295** (the real aquifer replaces the sea-level approximation,
and carvers are composed — `crates/lodestone-worldgen/src/overworld.rs`;
`column()`'s own output is now post-carve, so "postsurface" below is a
pre-carve *reference*, not the composed-today subset any more — see that
module's doc comment):

| chunk | stage | match | real mismatches | property-only mismatches |
|---|---|---|---|---|
| (0, 0) | postsurface (pre-carve reference) | 91400/98304 (92.98%) | 3060 | 3844 |
| (0, 0) | postcarve (full, minus features/structures) | **94460/98304 (96.09%)** | **0** | 3844 |
| (-120, -120) | postsurface | 93141/98304 (94.75%) | 5157 | 6 |
| (-120, -120) | postcarve | 93608/98304 (95.22%) | 4690 | 6 |

"Real" = the block *id* differs (a genuine composition/algorithm gap).
"Property-only" = same block id, different block-state properties —
see [Known representation gap](#known-representation-gap-fluid-level) below;
counted, never silently discarded, but not the same kind of finding.

Run `cargo run -p lodestone-worldgen-parity --bin compare` for the live
report (bounding box + per-16-tall-section breakdown), or
`cargo run -p lodestone-worldgen-parity --example breakdown_by_block_pair`
for a `(expected, got)` frequency table. What that breakdown shows, per
chunk, after #295:

- **(0, 0), postcarve**: **zero real mismatches.** The pre-#295 breakdown
  named this chunk's gap as `water → stone` (2780 positions, vanilla's flooded
  caves) and `air → stone` (218) — composing the real aquifer + carvers
  resolved both buckets completely for this chunk/seed; only the pre-existing
  fluid-`level` property gap remains. `tests/chunk_parity.rs`'s
  `water_to_stone_bucket_is_resolved_for_chunk_0_0` asserts this bucket is
  exactly 0 (not just "smaller") and was confirmed to *fail* at exactly 2780
  when carve composition was temporarily disabled as a control — see that
  test's own doc comment.
- **(0, 0), postsurface (pre-carve reference)**: now shows the *inverse* of
  the old gap — `stone → water`/`stone → air` at almost exactly the same
  2780/218 positions. This is expected, not a regression: `column()`'s output
  is post-carve, so comparing it against vanilla's pre-carve `postsurface`
  necessarily shows every cell the carver legitimately touched as a
  "mismatch" against the pre-carve reference. `docs/worldgen-parity.md`
  keeps both stages in the fixture specifically so this distinction is
  visible instead of collapsing into one number.
- **(-120, -120), both stages**: still dominated by `terracotta`/
  `orange_terracotta`/`yellow_terracotta`/`white_terracotta`/
  `red_terracotta`/`light_gray_terracotta` → `stone`/`dirt`/`grass_block` —
  the pre-existing badlands-exclusion gap (`crates/lodestone-worldgen/src/biome.rs`'s
  `usable_overworld_table`, unrelated to #295, see the original write-up
  below) — plus a modest **postcarve improvement over postsurface** (5157 →
  4690 real mismatches) from `deepslate → lava` and similar carve-introduced
  cells now agreeing with vanilla. The badlands gap dominates regardless of
  #295's work here.

**Original (pre-#295) badlands finding, unchanged:** this chunk's real
vanilla biome is `badlands`/`eroded_badlands` — and
`crates/lodestone-worldgen/src/biome.rs`'s `usable_overworld_table`
**deliberately excludes** badlands/eroded_badlands/wooded_badlands from the
searchable biome table (vanilla's `Bandlands` surface rule delegates to an
unported `SurfaceSystem.getBand` subsystem that would panic if reached), so
Rust falls back to the nearest *supported* biome and its surface rule never
produces terracotta banding. Composing carvers/the real aquifer does not
change this: the same excluded table feeds carver/ore biome resolution too, so
a column can never resolve to one of the three excluded names in the first
place (see `crates/lodestone-worldgen/src/overworld.rs`'s "Badlands" section).

**After the ore-oracle fix (issue #295's ore-oracle-parity increment,
`postfeatures` stage — before ore composition landed):**

| chunk | postfeatures vs postcarve (blocks the ore step placed) | postfeatures vs pre-composition `column()` |
|---|---|---|
| (0, 0) | 4113 real mismatches | 4113 real mismatches, 90347/98304 match (91.91%) |
| (-120, -120) | 4942 real mismatches | 9632 real mismatches, 88666/98304 match (90.20%) |

The `(-120,-120)` gap broke down cleanly: 4690 of the 9632 was the
pre-existing badlands-exclusion gap above (unrelated to ores), and the
remaining 4942 was exactly the ore step's own contribution.

**After composing `apply_ore_step_3x3_per_source` into `column()`** (the
real vanilla 3×3 neighbourhood driver, not the single-source primitive —
see `crates/lodestone-worldgen/src/overworld.rs`'s module doc "Performance"
and ore-features sections):

| chunk | vs `postfeatures` (issue) | vs `postcarve` (full-pipeline floor) | vs `postsurface` (reference ceiling) |
|---|---|---|---|
| (0, 0) | 92223/98304 match, **2237** real mismatches (down from 4113) | 88733/98304 match, 5727 real | 8787 real |
| (-120, -120) | 91703/98304 match, **6595** real mismatches (up from 4942) | 87508/98304 match, 10790 real | 11257 real |

**The gap against `postfeatures` did not go to (near) zero, and that is the
finding, not a bug to route around** — measured, not guessed, per
`CLAUDE.md`'s evidence standard:

- `postfeatures` is **single-source only** (`ComposedChunkOracle.java`'s own
  doc comment: it never extends to a real 3×3 with real per-quart biome
  variety). A faithful 3×3 composition *legitimately* diverges from a
  single-source oracle wherever real vanilla ore spill from a neighbour
  chunk lands in the centre — that is the composition working, not
  regressing.
- This was isolated, not assumed: a debug-only toggle
  (`LODESTONE_ORE_SINGLE_SOURCE_DEBUG=1`, read by `OverworldGenerator::ore_stage`)
  runs only the centre's own decoration pass — matching `postfeatures`'s own
  scope exactly — while still stitching the full 3×3 terrain so heightmap
  probes never panic. That measured **563/98304** real mismatches at chunk
  (0,0) (down from the pre-composition 4113, a 86% reduction) — strong
  evidence the ore *engine* and per-source biome resolution are correct, and
  that most of the full-3×3 residual (2237/98304) against `postfeatures` is
  real spill this oracle stage cannot model.
- At chunk (-120,-120), the single-source residual (5391/98304) is *larger*
  than the pre-composition baseline (4942) — because that chunk's real
  vanilla biome is badlands, which `usable_overworld_table` still excludes
  (Job 3, unported `SurfaceSystem.getBand`). Every source chunk there
  resolves to the *wrong* biome's ore list, not merely an incomplete one.
  This was confirmed directly, not inferred: a new per-ore-type count test
  (`ore_counts_by_type_are_predicted_and_measured`) found vanilla placed 51
  `minecraft:gold_ore` at this chunk while composed Rust placed **zero** —
  `badlands.json`'s own `UNDERGROUND_ORES` step names
  `minecraft:ore_gold_extra` (badlands' well-known bonus gold vein) as its
  27th entry, absent from `plains.json`'s step (and every other substitute
  biome's). A biome substitution can only ever place the substitute's ore
  list, so this is the pre-existing badlands gap cascading into ore
  selection, not a new defect — recorded as a documented exception in that
  test, not a loosened assertion.
- Per `CLAUDE.md`'s "an exact-match ore comparison can fail purely from RNG
  stream ordering": the per-ore-type count table (chunk (0,0), vanilla
  single-source / rust composed) shows the *narrow-target* ores (coal, iron,
  copper, gold, lapis, redstone, diamond, all their deepslate variants)
  matching within single digits — e.g. `iron_ore` 45→46, `coal_ore` 42→54 —
  while the *large blob* ores (`size=64`: andesite, diorite, granite, tuff,
  whose spread/radius genuinely crosses chunk boundaries) run
  consistently ~1.5-1.8× vanilla's single-source count (andesite 359→648,
  granite 607→852, tuff 672→1077). That ratio, applying uniformly across
  every large-blob type and none of the small ones, is exactly the
  signature real 3×3 spill predicts, not a random RNG-order bug (which
  would not respect that size/type split).

Run `cargo test -p lodestone-worldgen-parity --no-fail-fast -- --nocapture`
to see these printed live (`ore_composition_gap_is_measured_and_reported`,
`ore_counts_by_type_are_predicted_and_measured`,
`postfeatures_actually_differs_from_postcarve`) rather than trusting this
table to stay fresh forever.

## A determinism bug the dense-grid refactor (Job 2) introduced, found and fixed

Moving the working grid from `HashMap<(i32,i32,i32), String>` to
`crate::dense_grid::DenseBlockGrid` (a flat, palette-indexed array — see
`crates/lodestone-worldgen/src/overworld.rs`'s "Performance" module-doc
section for the full story) changed *when* a block state's palette index
gets assigned: incrementally, in `.set()` call order, rather than by a
separate fixed-order final pass the old code used regardless of how the
intermediate `HashMap` was populated. `OverworldGenerator::materialize_world`
applied `surface_diff` (a fresh `HashMap` per chunk) by iterating it
directly — and `std::collections::HashMap` iteration order is not guaranteed
stable even across two *separately constructed* maps with identical content.
Two independent `column()` calls for the *same* chunk therefore placed the
same blocks at the same positions but assigned them **different palette
indices** — same terrain, different serialised bytes.

This was not found by this crate's own parity tests (which compare resolved
block-state *strings*, immune to palette-index reshuffling) — it was found
by `lodestone-server`'s brand-new
`chunk::tests::parallel_generation_is_deterministic_and_matches_serial`
(issue #414, landed by a different agent mid-session as part of `275c765`,
which fanned chunk generation out over scoped threads), which failed
non-deterministically. Before assuming that test's own new
`generate_columns_parallel` code was at fault, an isolated `git worktree` at
the commit immediately before this session's ore composition landed
confirmed the test passed cleanly there — ruling that out and pointing back
at this session's own changes. This is a **different** bug from the one
`275c765` itself already found and fixed in the same area
(`ViewTracker::recenter`'s added-chunk list depending on
`HashSet::difference`'s run-to-run-unstable iteration order — send order,
not generation content; see `docs/server-chunk-generation-parallelism.md`,
not this doc, for that one) — noted here only so the two aren't conflated,
not duplicated. A permanent, threading-free regression control,
`lodestone_server::worldgen_data::tests::column_is_byte_identical_across_two_independent_sequential_calls`,
narrowed it further: two *sequential*, single-threaded `column()` calls for
the same chunk already produced different bytes, which meant the bug was a
pure logic error (palette order depending on `HashMap` iteration), not a
concurrency/Mutex issue in the (correctly-designed, `Mutex`-protected)
`Density` node caching described in `crates/lodestone-worldgen/src/density/mod.rs`.

Fixed by consulting `surface_diff` via a point lookup (`.get(&(lx, y, lz))`)
inside the same fixed `(lz, lx, ly)` loop the base fill already uses, never
iterating the map directly. Both controls above are green after the fix,
and the originally-failing `parallel_generation_is_deterministic_and_matches_serial`
passes reliably again.

## Anti-vacuity floors (`tests/chunk_parity.rs`)

- `fixtures_are_non_vacuous` — both fixtures have >25% non-air content and
  every biome quart resolved to a real `minecraft:`-namespaced id, so the
  comparisons below can't pass by both sides being empty.
- `control_self_diff_is_exact` — diffing a fixture against itself is asserted
  to be **exactly** zero mismatches over all 98304 cells, proving
  `diff_field` visits every cell rather than short-circuiting.
- `control_mutate_one_block_is_caught` — **run, not described**: clones a
  fixture's block field, overwrites exactly one cell with a value guaranteed
  different from what's there, diffs, and asserts exactly one mismatch, at
  exactly that `(lx, y, lz)`, with the right `expected`/`got` values and a
  bounding box collapsed to that single point. Confirmed passing.

## What could not be isolated, and why

- **Vegetation features**: not composed into `ComposedChunkOracle.java` and
  not built anywhere in this crate's Rust (epic #404 Phase 3). No isolated
  oracle for it exists yet in `scripts/worldgen-oracle/` either.
- **Ore features (composition into `OverworldGenerator::column`)**: **now
  composed** (`crate::feature::apply_ore_step_3x3_per_source`, the real
  vanilla 3×3 driver, per-source biome resolution). What still can't be
  isolated with *this* oracle is the *comparison*: `ComposedChunkOracle
  .java`'s `postfeatures` stage remains single-source only (extending it to a
  real 3×3 with real per-quart biome variety needs 8 more fully-generated
  real chunks per fixture dump — significant Docker/JVM cost, not attempted).
  So a faithful 3×3 Rust composition necessarily shows *more* divergence from
  `postfeatures` than a single-source comparison would, wherever real vanilla
  spill lands in the centre — see the measured numbers and the
  single-source-only debug toggle above, which isolates "is the engine
  correct" (yes, 563/98304 residual) from "does 3×3 spill widen the gap
  against an oracle that can't see it" (yes, expected). Extending this oracle
  to a real 3×3 is the natural next increment for closing this specific
  residual with hard evidence, not attempted in this pass.
  **The reason the oracle was fixed *before* composition is itself worth
  recording**: a #295 architecture review found that `FeatureOracle.java`
  used to share the very simplification it was supposed to be checking. Its
  header used to say it "deliberately does NOT model ore spill from the 8
  neighbouring chunks into the centre (a 3×3 driver, analogous to the carver
  17×17 driver)," and `crate::feature::OreInput`'s `get_height`/`in_center`
  used to wrap/drop edge probes to match that oracle rather than vanilla's
  real `blockStateWriteRadius(1)` (`ChunkPyramid.java:32-35`). The old
  `feature_parity`'s "whole-chunk exact both directions" agreement therefore
  proved the Rust port matched the oracle's chosen simplification, not that
  the simplification was vanilla's — the authored-oracle trap in a subtle
  form (the oracle shares the very simplification it's validating). Composing
  ore features into `OverworldGenerator` on top of *that* oracle would have
  baked a wrong edge band into every chunk with no gate able to see it.
- **Structures**: unbuilt anywhere in this repo's Rust (`#136`: "do not start
  implementation against this issue" until core worldgen has *any* structure
  concept). Nothing to compare them against yet, so `postcarve` is honestly
  the ceiling this harness can currently target.
- **Vertical biome variation**: both sides model one biome per horizontal
  quart, broadcast across all `y` (`crate::biome`'s "Resolution: one biome
  per quart column, not per quart cube" — a deliberate Phase 1 scope note,
  not this harness's own limitation). A true 3-D biome comparison (surface
  `plains` vs. a `dripstone_caves` pocket 40 blocks under it) isn't
  meaningful until Phase 2 gives Rust cave volume for a biome to vary across.

## Known representation gap: fluid `level`

Every vanilla block-state string for a fluid always lists its `level`
property (`minecraft:water[level=0]` for a full source block).
`OverworldGenerator`'s fluid-fill stage writes the bare `default_fluid`
string straight from `noise_settings/overworld.json` (`"minecraft:water"`,
no `Properties`) — the engine has no concept of partial fluid levels at all,
so it never emits `[level=...]`. `Mismatch::same_block_id` and
`DiffReport::representation_only_mismatches` isolate this bucket so it's
visible (measured: 4828 positions in the wet chunk, 0 in the dry one) without
inflating the "real" gap numbers the composition-progress gates key off.
This is a real, if boring, deficiency — the served wire protocol block-state
id for `"minecraft:water"` with no properties happens to already resolve to
the default (`level=0`) state, so it is very unlikely to be player-visible,
but it is not nothing, and a future contributor extending fluid handling
(e.g. flowing water) should know this gap exists rather than rediscover it.

## Known gap: the 3×3 ore driver's residual beyond its own neighbourhood

`FeatureOracle.java` and `crate::feature::OreInput`/`apply_ore_step_3x3` now
drive a real 3×3 chunk neighbourhood (the centre plus its 8 immediate
neighbours) for ore composition, each with its own origin and its own
`decorationSeed`, matching vanilla's real `blockStateWriteRadius(1)` spill —
this replaces the old "wrap every out-of-chunk probe into the centre"
approximation, which was wrong for effectively every edge case, not just a
rare one.

Vanilla's largest overworld blob ores (`size=64`:
`ore_andesite`/`ore_diorite`/`ore_granite`/`ore_tuff` — common features, not
rare ones) can, in boundary-adjacent cases, probe or write up to ~13 blocks
beyond the chunk they originate in (`spread_xy = size/8 = 8`,
`max_radius = ceil((size/16*2+1)/2) = 5`, probe half-width
`ceil(spread_xy) + max_radius = 13`). A source chunk at the edge of the
driven 3×3 neighbourhood can therefore reach up to ~13 blocks into a chunk
*two* away from the centre — outside the 3×3 footprint. **Which chunks get
to place ore features** stays exactly 3×3 regardless (that is a real,
enforced vanilla limit — `blockStateWriteRadius(1)` — not an approximation);
what this section is about is how far a *read* during one of those 9 chunks'
own placement (a heightmap probe, a block-state/adjacency check) can reach.

**The two sides handle this differently, and that asymmetry is deliberate.**
`FeatureOracle.java`'s `getChunk`/`getHeight` are **not** clamped: they
lazily generate (and memoise) whatever additional chunk a read actually
touches, so the oracle's own answer for the centre's post-feature state is
the true vanilla value, bounded only by the ore blob's own geometry, never by
an artificial cap. An earlier version of this method *did* clamp reads to
the 3×3 footprint, and that clamp is what caused the JVM deadlock described
in `FeatureOracle.java`'s own header comment: two different real chunk
coordinates that both clamp to the same edge alias onto the same memoised
chunk, and vanilla's own `BulkSectionAccess` (used by `OreFeature.doPlace`)
does not know about that aliasing — it can try to acquire the same
`LevelChunkSection`'s non-reentrant semaphore twice within one placement and
hang forever. Measured directly: `jstack` on the hung process showed the main
thread parked in `ThreadingDetector.checkAndLock`, called from
`OreFeature.doPlace`, after 10+ minutes at ~0% CPU.

Rust's `OreInput::region_local`, by contrast, **is** clamped — necessarily,
because the *fixture* `FeatureOracle.java` dumps (`inrun.`/`ofh.`) is itself
bounded to the 3×3 region (`REGION_MIN..REGION_MAX`, `-16..32`): an unbounded
terrain dump has no natural size limit, so there is nothing further out for
Rust to read even if it wanted to. This means the oracle's own answer can be
*more* correct than Rust's reconstruction of it in this specific residual —
a real, narrow, understood gap, not a shared approximation as an earlier
draft of this section claimed.

This has not been measured to produce any parity difference in the two named
fixture chunks (`(0,0)`, `(-120,-120)`) — the size-64 blob ores are common,
but landing exactly at the few blocks of a chunk edge where this residual
would fire is not — but it is a real, understood limitation rather than an
unknown one, and a future contributor chasing a stray single-digit ore
mismatch near a chunk boundary should check here before assuming a bug in the
placement engine itself.

## Known gap: `ComposedChunkOracle.java`'s `postfeatures` stage is single-source only

Unlike `FeatureOracle.java` (fixed single biome, so the real 3×3 driver above
is cheap: every one of the 9 source chunks shares one feature list),
`ComposedChunkOracle.java` runs the real `MultiNoiseBiomeSource`, so a
faithful 3×3 ore driver here would need 8 more *real, per-quart-biome* chunks
generated (their own `fillFromNoise`/`buildSurface`/17×17-carve passes) —
a significant expansion of an already-heavy single-chunk Docker run.
`postfeatures` therefore only runs the **centre chunk's own** ore step (its
own origin, its own seed, its own biome's feature list) against the real
composed `postcarve` terrain above it — no neighbour spill modelled here at
all, unlike `FeatureOracle.java`'s fixture. `FeatureOracle.java` remains the
authoritative check on the ore *engine* (RNG order, placement, and now real
3×3 spill); `postfeatures` exists to show what composing the centre's own ore
step looks like against real biome variety (useful for the count-band
predictions below), not as a fully vanilla-accurate edge band. Extending this
oracle to the real 3×3 model is a further increment, not attempted here.

## How to add a stage

1. In `ComposedChunkOracle.java`, dump the block field at the new pipeline
   point (follow the `postsurface`/`postcarve` pattern: a full `16×384×16`
   `canon(chunk.getBlockState(...))` sweep, labelled `<stage>.x,y,z`).
2. Add the stage to `parse_raw_dump` (a new `else if let Some(coords) =
   key.strip_prefix("<stage>.")` arm) and to `ChunkFixture` (a new
   `BlockField` member), then thread it through `encode_compact`/
   `parse_compact`'s `[("postsurface", ...), ("postcarve", ...), (...)]`
   stage list.
3. Regenerate (`cargo run -p lodestone-worldgen-parity --bin regen`), review
   the fixture diff, commit.
4. Add a comparison + measured ceiling/floor to `tests/chunk_parity.rs`,
   following `currently_composed_subset_matches_vanilla_postsurface`'s
   pattern (measure first, then assert "no worse than measured" — never
   assert a guessed number).

## How to add a chunk/seed

1. Add a `dumpChunk(provider, seed, cx, cz);` call in `ComposedChunkOracle
   .main` (or change `seed`, see [Seed determinism](#seed-determinism)).
2. Regenerate, which appends the new chunk's fixture block to
   `fixtures/composed_seed42.txt` (or writes a new `fixtures/composed_seed<N>
   .txt` if you changed the seed — `regen`'s `fixture_path()` would need a
   matching CLI flag; today it's hardcoded to the one committed fixture,
   deliberately, since only one seed is in use).
3. Add a `ceiling`/`floor` arm to `tests/chunk_parity.rs`'s two `match`
   statements — they panic (not silently skip) on an un-calibrated chunk.

## How to add a version

Only `crates/protocol/v770` implements `ServerProtocol`, and
`lodestone-worldgen`'s JSON-driven data (density functions, noises, surface
rules, the biome parameter table) is 26.2-specific — so nothing here
currently exercises a second version, and this harness does not build one.
What a second version would actually need, so a future contributor doesn't
have to re-derive it:

- **A second oracle classpath.** `scripts/worldgen-oracle/run.sh` mounts
  `.cache/mc/26.2` and its `server-26.2.jar`; a second version needs its own
  `.cache/mc/<version>` (or a `run.sh` argument selecting one) and, likely, a
  separately-named oracle class if registry/API shapes drifted enough that
  `ComposedChunkOracle.java` doesn't compile unchanged against the older jar.
- **A second embedded data bundle.** `lodestone_server::overworld_generator`
  is wired to exactly one `EmbeddedResolver` over
  `crates/lodestone-server/assets/worldgen/` (26.2 data, no version gate —
  already flagged as a pre-existing seam gap in `worldgen_data.rs`'s own doc
  comment and in the worldgen completion plan's §6, not something this
  harness introduces). A second version needs its own such bundle and a way
  to select which `OverworldGenerator` a fixture's `seed`/coordinates should
  be checked against.
- **What does *not* need to change**: the fixture format, `diff_field`,
  `DiffReport`, and the whole `crates/lodestone-worldgen-parity` crate shape.
  A second version is a second `fixtures/composed_seed<seed>_<version>.txt`
  plus a version-tagged oracle run — data, not a new code path — matching
  this repo's general rule that version-specific worldgen content is data a
  version-free engine consumes, not a fork of the engine.

## One-command entry points

```bash
# Hermetic report (no Docker) — reads the committed fixture, prints the
# per-chunk / per-stage diff against the *actually served* generator.
cargo run -p lodestone-worldgen-parity --bin compare

# The gate (what CI/health-check runs) — same comparisons, pass/fail.
cargo test -p lodestone-worldgen-parity --no-fail-fast

# Regenerate the fixture from a fresh vanilla JVM run (needs Docker; no
# local JDK required — see CLAUDE.md's data-sources section).
cargo run -p lodestone-worldgen-parity --bin regen

# Finer breakdown: which (expected, got) block-id pairs dominate the gap.
cargo run -p lodestone-worldgen-parity --example breakdown_by_block_pair
```
