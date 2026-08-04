# Overworld biome assignment (multi-noise climate)

Issue [#405](https://github.com/matteopolak/lodestone/issues/405), epic
[#404](https://github.com/matteopolak/lodestone/issues/404) (Phase 1). See
[`worldgen-plan.md`](./worldgen-plan.md) for the surrounding phased plan.

## What it is

Before this, `OverworldGenerator` ran the whole world under one hardcoded biome
(`minecraft:plains`) — every column looked the same and surface rules never varied. This is
vanilla's real **multi-noise biome source**: each column samples six climate values (temperature,
humidity, continentalness, erosion, depth, weirdness) and finds the nearest match, by squared
distance, in a table of ~7.6k `(climate range, biome)` entries. Different climate → different biome
→ different surface material (sand in deserts, snow in taiga, terracotta-adjacent in savanna, etc.)
and, once a render-layer consumer exists, different grass/foliage tint.

## How it works

**The engine** (version-free, `crates/lodestone-worldgen/src/biome.rs`):

- `ClimateSampler` evaluates the same six `noise_router` density functions
  (`temperature`/`vegetation`/`continents`/`erosion`/`depth`/`ridges`) the shape stage already
  proves bit-exact against a JVM (`region_parity`'s "34048/34048" whole-region check covers these
  exact five channels plus depth) — so the only genuinely new code here is the search, not the
  climate math.
- `nearest_biome` is vanilla's own `Climate.ParameterList.findValueBruteForce` (the un-optimized
  reference next to vanilla's RTree, `Climate.java:182`) — a linear scan, safe to skip the RTree
  entirely since a few thousand squared-distance comparisons per quart is already fast.
- `usable_overworld_table` excludes `minecraft:badlands`/`eroded_badlands`/`wooded_badlands` — see
  "Gotchas" below.

**The data.** `crates/lodestone-server/assets/worldgen/biome_parameters/overworld.json` — **7594
rows** (the scoping plan estimated ~700 from reading `OverworldBiomeBuilder.java`'s Java control
flow; the real count is 10x that, measured via the oracle dump, not assumed). Dumped once via
`scripts/worldgen-oracle/BiomeOracle.java`'s `table` mode — no bootstrap or registry access needed,
`MultiNoiseBiomeSourceParameterList.knownPresets()` is a public, self-contained call. Each row is 13
raw quantized `long`s (6 axes × `[min, max]`) plus an `offset`, plus a biome id string — the exact
representation `Climate.Parameter` carries internally, so parsing never re-derives a float from
decimal text. `overworld_temperature.json` is a much smaller companion (55 entries, biome id →
declared `temperature`), read directly from vanilla's own `data/minecraft/worldgen/biome/*.json`
files (no oracle needed for this one), used to approximate `cold_enough_to_snow` per sampled biome.

**Resolution: one climate sample per quart, not per column, not per block.** Vanilla's real biome
grid is 3-D (`4×4×4`-block cells, full height). This phase samples once per **horizontal** quart
(16 per chunk, at that quart's own corner — see the y = 0 trap below) and broadcasts the answer
vertically. That is deliberately narrower than vanilla: caves aren't composed into
`OverworldGenerator` yet (issue #295 / epic #404 Phase 2), so there is no vertical volume for a
cave biome to describe. Revisiting this (real 3-D sampling) is the natural first step once caves
land.

**Wiring.** `OverworldGenerator::biome_stage` runs between the fluid-fill and surface stages, needs
`heights[]` (the fluid stage's own output) for the y = 0 fix below. `surface/mod.rs`'s `Cond::BiomeIs`
and `Cond::Temperature` (issue #405 made both **runtime** checks against a per-column `Ctx`, not
build-time constants — `SurfaceSystem` used to be built for exactly one fixed biome for its whole
life). The served column carries biome as a small `[String; 16]` quart grid
(`GeneratedColumn`/`ChunkColumn::biome_state`), and `crates/protocol/v770/src/server_protocol.rs`'s
`build_world_column` resolves each quart's name to a wire id (`resolve_biome_id`/`BIOME_NAMES`) and
writes it into the real `ChunkSection` biome container (`ChunkSection::set_biome`, 4×4×4, already
existed — nothing new needed on the wire-format side).

## How to change it, and gotchas

- **The y = 0 trap.** An early version sampled climate at a fixed `y = 0` for every column. That
  reads as "deep underground" almost everywhere: `depth`'s density function
  (`overworld/depth.json`) is `y_clamped_gradient(-64→320, 1.5→-1.5) + offset`, so at `y = 0` the
  gradient term alone is already ≈ +1.0 (climate-space "cave"), and the probe came back almost
  entirely `lush_caves`/`dripstone_caves`/`deep_ocean`. **Sample at the column's own generated
  surface height instead** (`biome_stage` reads `heights[]`, the fluid-fill stage's own output).
- **Both X/Z *and* Y need quart-rounding, and it is easy to fix one and miss the other.** Vanilla's
  `Climate.Sampler.sample(quartX, quartY, quartZ)` floors *every* axis to `QuartPos.toBlock`
  (`coord << 2`) before evaluating — sampling at a quart's **center** (`qx*4 + 2`) instead of its
  **corner** (`qx*4`) flipped a real `dark_forest`/`river` boundary at world `(0, 0)`, and rounding
  X/Z but not the surface-height Y produced a `depth` value 156 quantized units off (the gradient's
  slope times the 2-block error) at the same point — both caught only by comparing against
  `BiomeOracle sample`'s raw quantized target, not by "a biome came back so it must be right." See
  `OverworldGenerator::biome_stage`'s doc comment for the exact before/after numbers.
- **Three biomes can't reach the surface here yet.** `minecraft:badlands`/`eroded_badlands`/
  `wooded_badlands` all hit `SurfaceRules.Bandlands` (confirmed by walking the JSON: both occurrences
  sit under a `condition{biome_is:[…those three…]}` guard, nothing else), which delegates to
  `SurfaceSystem.getBand` — a genuinely unported subsystem (its own noises, plus a banded-terracotta
  column generator). Before this issue, that rule was unreachable dead code (fixed biome = plains);
  now that biomes vary for real, reaching it would **crash chunk generation** the moment a player's
  world contains badlands. `biome::usable_overworld_table` filters these three out of the searchable
  table so the nearest-neighbour search falls back to the next-closest *supported* biome instead —
  a deliberate, documented gap, not a silent wrong answer. Porting `getBand` lifts this restriction;
  it hasn't been attempted here.
- **`cold_enough_to_snow` ignores the per-block height adjustment and `temperature_modifier`.**
  `biome::cold_enough_to_snow` is `declared temperature < 0.15`, matching vanilla's
  `warmEnoughToRain` threshold but not `Biome.getHeightAdjustedTemperature`'s noise + `(y - seaLevel
  - 17) * 0.05/40` correction above `seaLevel + 17`, nor the `frozen` modifier a few ocean biomes
  declare. This isn't a new simplification introduced here — before this issue,
  `cold_enough_to_snow` was already one fixed bool for the whole world
  (`worldgen_data::DEFAULT_BIOME_SNOWS`); this computes that same kind of approximate answer per
  selected biome instead of once globally. Worth revisiting if a snow-line seam near `sea_level + 17`
  ever needs it.
- **The wire biome-id space is provisional.** `crates/protocol/v770/src/server_protocol.rs`'s
  `BIOME_NAMES` is alphabetical-by-name, **not** vanilla's real registration order — because this
  server sends **no `minecraft:worldgen/biome` registry-data sync at all** yet
  (`V770ServerProtocol::begin_configuration`'s own doc comment already flags this: "registry data …
  are all real vanilla packets this join sequence does not yet need to send"), so there is no
  existing id space to agree with. Any stable, reproducible convention is safe until that sync
  exists — regenerate the list with `awk '/^row\./{print $2}' scripts/worldgen-oracle/biome_java.txt
  | sort -u`. Replace this table with the real synced order once registry-data sync lands; don't
  treat it as a substitute for that.
- **Render-side consumer now exists — see [`biome-tint.md`](./biome-tint.md).** This bullet used to
  say zero implementors of `BiomeTint`; that is no longer true. `crates/lodestone-shell/src/
  mesher.rs`'s `SnapshotModelView`/`SnapshotFluidView` (the real views `MeshScheduler` meshes
  through) now implement `ModelSectionView::biome_tint_at`/`FluidSectionView::water_tint_at`,
  resolving each grass/foliage/dry-foliage/water quad's *real*, vanilla-box-blended colour via a new
  `lodestone_assets::tint::BiomeEffects` table (jar-derived, 66 biomes) and
  `crates/lodestone-render/src/biome_tint.rs`'s `NamedBiomeTint`/`resolve_blended_tint`. Proven live
  against a real `client.jar` in `crates/lodestone-shell/tests/biome_tint_live_mesh.rs`: two grass
  blocks in the same section, one in desert one in swamp biome, render `[191, 183, 85]` and
  `[0x6A, 0x70, 0x39]` respectively (the second value predicted exactly from the jar source
  independent of this code, since `GrassColorModifier::Swamp` ignores the colormap entirely).
  **Follow-up now closed**: the id→name mapping this consumer uses
  (`mesher.rs`'s `biome_name_at`) used to read *only* a provisional local mirror of
  `crates/protocol/v770/src/server_protocol.rs`'s `BIOME_NAMES` (`FALLBACK_BIOME_NAMES`) —
  correct against this codebase's own server (the only one it can host), not necessarily against a
  real vanilla server. The real registry-sync order this client already decoded correctly
  (`ClientRegistries::entry_names(BIOME)`) now threads all the way from the v770 adapter through
  `net.rs`'s `BiomeNameCell` and `Sim::refresh_mesh_policy` into the mesher's worker threads (baked
  onto each `SectionSnapshot`, the same way `SkyDefault` already crosses that boundary);
  `FALLBACK_BIOME_NAMES` is now consulted only when no live registry has arrived (no connection
  yet, or a version/server that sends none). See `biome-tint.md`'s "Gotchas" for the full wiring
  and the live gate that proves it against a fixture registry order which deliberately disagrees
  with the fallback table. **`server_protocol.rs`'s `BIOME_NAMES` itself is untouched by this
  follow-up** — it is the server's own id→name *assignment*, a separate and still-provisional gap
  from the client's id→name *resolution* this bullet is about; see the bullet below. The
  swamp/mangrove-swamp two-tone noise term (`Biome.BIOME_INFO_NOISE`) also stays unported — 64 of
  66 biomes are unaffected by that gap.
- **`chunks_biomes` (protocol issue #26) is decoded and reaches the world**, independently of tint:
  `World::merge_biomes` (`crates/lodestone-world/src/world.rs`) applies a live biome edit (vanilla's
  `/fillbiome`) to an already-loaded column without touching its block state, and
  `crates/protocol/v770/src/adapter.rs`'s `CHUNKS_BIOMES` arm reuses `ClientEvent::ChunkLoaded` as the
  remesh signal — the same dirty-region event `light_update` already uses for a non-block-changing
  update. See `docs/clientbound-packet-coverage.md`'s now-`Landed` row and
  `crates/protocol/v770/tests/chunks_biomes.rs`.

## Configuration

No env vars or flags. `LODESTONE_REGEN`-style reproducibility for the data:

```bash
# Full parameter table (writes scripts/worldgen-oracle/biome_java.txt's row.N lines)
bash scripts/worldgen-oracle/run.sh BiomeOracle table

# Ground-truth spot-check: seed, then (x y z) triples (y should be the
# column's own quart-aligned generated surface height, not 0 — see the y = 0
# trap above)
bash scripts/worldgen-oracle/run.sh BiomeOracle "sample 42 0 62 0 8 62 8"
```
The committed `crates/lodestone-server/assets/worldgen/biome_parameters/overworld.json` and
`overworld_temperature.json` are the derived, checked-in tables the embedded resolver actually
loads; regenerate them from a fresh `BiomeOracle table` dump (13 numbers + biome name per row) and
the vanilla `biome/*.json` files respectively if the underlying game data ever changes.

## Dependencies

- `crates/lodestone-worldgen` — the version-free engine (`biome.rs`, `surface/mod.rs`'s dynamic
  `Cond::BiomeIs`/`Cond::Temperature`, `overworld.rs`'s `biome_stage`).
- `crates/lodestone-server` — the embedded 26.2 data (`worldgen_data.rs`'s `EmbeddedResolver`
  overriding `Resolver::biome_parameters`/`biome_temperatures`) and the served column
  (`chunk.rs`'s `ChunkColumn::biome_state`).
- `crates/protocol/v770` — the wire encoder (`server_protocol.rs`'s `build_world_column`,
  `resolve_biome_id`, `BIOME_NAMES`) and `lodestone-world`'s pre-existing `ChunkSection::set_biome`
  (4×4×4 biome container — no format change needed, only a real writer).
- `scripts/worldgen-oracle/BiomeOracle.java` — the JVM oracle, same `run.sh`/
  `docker run eclipse-temurin:25-jdk` pattern as every other oracle in that directory.
