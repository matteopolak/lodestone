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
non-degenerate columns:

| chunk | stage | match | real mismatches | property-only mismatches |
|---|---|---|---|---|
| (0, 0) | postsurface (composed-today subset) | 93160/98304 (94.77%) | 316 | 4828 |
| (0, 0) | postcarve (full, minus features/structures) | 90100/98304 (91.65%) | 3376 | 4828 |
| (-120, -120) | postsurface | 93394/98304 (95.01%) | 4910 | 0 |
| (-120, -120) | postcarve | 93053/98304 (94.66%) | 5251 | 0 |

"Real" = the block *id* differs (a genuine composition/algorithm gap).
"Property-only" = same block id, different block-state properties —
see [Known representation gap](#known-representation-gap-fluid-level) below;
counted, never silently discarded, but not the same kind of finding.

Run `cargo run -p lodestone-worldgen-parity --bin compare` for the live
report (bounding box + per-16-tall-section breakdown), or
`cargo run -p lodestone-worldgen-parity --example breakdown_by_block_pair`
for a `(expected, got)` frequency table. What that breakdown shows, per
chunk:

- **(0, 0), postcarve vs. today**: dominated by `water → stone` (2780
  positions) and `air → stone` (218). This is the honest #295 number: vanilla
  carved flooded caves and tunnels through this column; today's
  `OverworldGenerator` has no carvers composed at all
  (`crates/lodestone-worldgen/src/overworld.rs:29-34`), so it's still solid
  rock there.
- **(-120, -120), both stages**: dominated by `terracotta`/`orange_terracotta`
  /`yellow_terracotta`/`white_terracotta`/`red_terracotta`/
  `light_gray_terracotta` → `stone`/`dirt`/`grass_block` (nearly all 4910–5251
  mismatches). This chunk's real vanilla biome is `badlands`/
  `eroded_badlands` — and `crates/lodestone-worldgen/src/biome.rs`'s
  `usable_overworld_table` **deliberately excludes** badlands/eroded_badlands
  /wooded_badlands from the searchable biome table (vanilla's `Bandlands`
  surface rule delegates to an unported `SurfaceSystem.getBand` subsystem that
  would panic if reached), so Rust falls back to the nearest *supported*
  biome and its surface rule never produces terracotta banding. This chunk is
  a live, measured demonstration of that already-documented Phase 1 gap, not
  a new finding — but it's a useful fixture precisely because it makes the
  gap concrete instead of theoretical.

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

- **Ore/vegetation features**: not composed into `ComposedChunkOracle.java`.
  `FeatureOracle.java` already isolates the ore-feature engine (placement
  modifiers, RNG order) against a fixed biome via a `WorldGenLevel` built from
  a JDK dynamic proxy — extending it to run inside this harness's real
  per-column biome, then folding its output into a third `postfeatures`
  fixture stage, is the natural next increment. Not attempted here to keep
  one Docker run's scope bounded, and because per-biome feature-list
  resolution needs the same kind of biome-source rewiring
  `applyCarvers` needed here, freshly built rather than reused.
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
