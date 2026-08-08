# The `MOTION_BLOCKING` heightmap

Issue [#516](https://github.com/matteopolak/lodestone/issues/516) (partial — the
generator and encoder halves for `MOTION_BLOCKING`; the other three sent maps
remain, which is why the issue stays open).

## What it is

Every served chunk used to carry a well-framed, **zero-entry** heightmap NBT —
`encode_column_body` wrote `Heightmaps::new().encode(&mut w)`, an empty
`Vec<(u32, Heightmap)>`. `lodestone-worldgen` computes the real `MOTION_BLOCKING`
map per column and exposes it on `GeneratedColumn`; `ChunkColumn` now carries it
across the generator/server seam and `encode_column_body` packs it into the chunk
packet, so a client receives a real map instead of an empty one.

## How it works

`OverworldGenerator::intern_from_dense`
(`crates/lodestone-worldgen/src/overworld/output.rs`) computes it as the last
thing it does, and both `column` and `column_timed` route through that one
function — so the pipeline grew no new stage and no new call site.

* **The predicate** is `input.blocksMotion() || !input.getFluidState().isEmpty()`,
  read off the record definition itself —
  `MOTION_BLOCKING(4, "MOTION_BLOCKING", Heightmap.Usage.CLIENT, …)`,
  `.cache/mc/26.2/src/net/minecraft/world/level/levelgen/Heightmap.java:151` — and
  already ported as `feature::top_layer::SnowSupport::motion_blocking` over two
  jar-dumped per-state columns. Nothing new is guessed. The fluid half is the one
  a solids-only port drops, and it is what makes an ocean column read 63 rather
  than the seabed.
* **The stored value** is `topMatchingY + 1 - min_y`.
  `Heightmap.primeHeightmaps` scans each column downward and calls
  `setHeight(x, z, m + 1)` at the first matching block (`Heightmap.java:60-64`);
  `setHeight` stores `y - chunk.getMinY()` and `getFirstAvailable` adds it back
  (`Heightmap.java:70-78`). A column with **no** matching block never gets a
  `setHeight` call, so its slot stays `0`, i.e. `min_y` — which is why an all-air
  column here is `0` and not a sentinel.
* **The index** is `Heightmap.getIndex(x, z) = x + z * 16`, matching
  `lodestone_world::heightmap::Heightmap::index`, so a consumer can `set` each
  column straight across with no re-ordering.
* **It is integer-only.** `top_layer::motion_blocking_first_free` tests the
  predicate against a *string* per block, which is right for it (it runs inside a
  stage holding a `DenseBlockGrid`). Here the palette is already built, so the
  predicate is evaluated once per **palette entry** — a few dozen times — and the
  256-column scan is `u16` compares against a `bool` slice. The string form would
  cost ~200k hash lookups per column for an identical answer.

### Why a fresh scan is equivalent to vanilla's incremental maintenance

Vanilla primes the heightmaps at the start of the `features` status and maintains
them through `Heightmap.update` per placed block, which is an incremental form of
exactly this scan. This runs after **every** stage including
`TOP_LAYER_MODIFICATION`, so nothing is left to place and a top-down scan of the
finished field lands on the same answer (the same argument
`feature/top_layer.rs`'s `motion_blocking_first_free` already makes). #516's scope
asks for incremental maintenance through the region view; that is a **cost**
refinement (worldgen-rewrite candidate 3), not a correctness one, and doing it
would not change a single stored height.

### `None`, not zeros

`GeneratedColumn::motion_blocking_heightmap()` returns `None` when the resolver
supplied no `block_freeze_facts` — every fixture `Resolver` in this workspace,
which is why this unit changed no parity fixture. A zeroed array would be
indistinguishable from "every column is air" and would encode a **wrong**
heightmap. That direction matters: the save-parity work found vanilla *adds* a
heightmap for any type we omit but **trusts** one we send, so a wrong heightmap is
worse than none.

## The encoder, and the two places it crosses a seam

Landed. Three links, and each one exists because the previous one could not reach
further. The registry id is never retyped: it is
`lodestone_worldgen::overworld::MOTION_BLOCKING_HEIGHTMAP_TYPE_ID` (`= 4`),
re-exported from `lodestone-server` because `lodestone-worldgen` is only a
*dev*-dependency of `lodestone-v770` and the encoder cannot name it at its source.

1. **`ChunkColumn`** (`crates/lodestone-server/src/chunk.rs`) carries an
   `Option<Box<[u16; 256]>>`, filled in `from_generated` from
   `column.motion_blocking_heightmap()` and read back through the
   `ChunkColumn::motion_blocking()` accessor. It deliberately does **not** ride
   `into_raw` — that tuple's own doc forbids widening it, and
   `biome_cells`/`block_entities` already established the opt-in accessor pattern.
2. **`encode_column_body`**, replacing `Heightmaps::new().encode(&mut w)`:

   ```rust
   let mut maps = Heightmaps::new();
   if let Some(stored) = column.motion_blocking() {
       let mut map = Heightmap::new(height as u32);
       for lz in 0..16 {
           for lx in 0..16 {
               map.set(lx, lz, u32::from(stored[lx + lz * 16]));
           }
       }
       maps.insert(MOTION_BLOCKING_HEIGHTMAP_TYPE_ID, map);
   }
   maps.encode(&mut w);
   ```

   `Heightmap::new(world_height)` sizes itself with `height_bits(world_height)` =
   9 bits for the overworld's 384, the same `ceillog2(getHeight() + 1)` vanilla's
   own `BitStorage` uses — so no width is chosen here either.
3. The **empty** case is unchanged (a zero-entry NBT), so a column from anywhere
   but the generator — `ChunkColumn::new`, a region-file load, a generator with no
   `block_freeze_facts` — regresses nothing.

**The field is a snapshot, not a maintained map.** `ChunkColumn::set_block` does
not move it, so a player edit leaves it stale, and that is deliberate: `chunk_nbt`
omits heightmaps from the Anvil write and relies on vanilla's
`Heightmap.primeHeightmaps` to re-derive on load, so nothing persists a stale
value. Only the first send after generation carries it — which is exactly the send
a client has no other way to derive one for. Maintaining it incrementally is
scope 1 of #516, and the prerequisite for the vegetation cost-per-draw work.

**Not in scope, deliberately:** the other three sent maps
(`WORLD_SURFACE`, `OCEAN_FLOOR`, `MOTION_BLOCKING_NO_LEAVES`).
`MOTION_BLOCKING_NO_LEAVES` in particular is *aliased onto* `WORLD_SURFACE` at
`feature/vegetation/config.rs:41` with no leaf/log exclusion, so sending it today
would send a knowingly wrong map — the one thing worse than sending none. #516
stays open for those.

## How to change it, and the gotchas

* **The `+1` is the whole trap.** It is two cancelling offsets away from being
  invisible: `Heightmap` stores `topY + 1`, `getHighestTaken` subtracts one, and
  `WorldGenRegion.getHeight` adds it back. `output.rs`'s unit test pins the `+1`,
  the `lx + lz * 16` index (with a transposition check), the all-air `0`, and the
  fluid half — all against `Heightmap.java`'s own lines, not against this code.
* **`snow[layers=1]` does not raise the map**, and that is correct rather than a
  bug: `SnowLayerBlock`'s collision shape for one layer is empty, so
  `blocksMotion()` is false. A column with a fresh snow layer on grass reports the
  grass's own first-free Y.
* **Adding a second map** means reading its id off its **own** line in
  `Heightmap.Types` — the id is the enum's first constructor argument, and reading
  it as an ordinal position happens to work here and is not a rule.

## Configuration

None. The map is computed unconditionally whenever the generator has
`block_freeze_facts`; there is no feature gate and no env var. Its cost lands in
`StageTimes::intern` rather than a field of its own (that field's doc says so).

## Dependencies

`feature::top_layer::SnowSupport` for the predicate (which needs
`lodestone_data::snow_support`'s dump, delivered through
`density::Resolver::block_freeze_facts`). The consumer side needs
`lodestone_world::heightmap::{Heightmap, Heightmaps}`, which already exists and
already round-trips both wire framings.
