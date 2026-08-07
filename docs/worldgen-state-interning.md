# Worldgen block-state interning

## What it is

Numeric `u16` handles (`StateId`) for block-state strings inside
`lodestone-worldgen`'s generation engine, so a block moving between two internal grids costs a
`u16` move instead of a heap-allocated `String`. Unit 3 of
[`docs/plans/worldgen-rewrite.md`](plans/worldgen-rewrite.md); it took a steady-state warm column
from **905,459 heap allocations to 20,686**, a 97.7% reduction, with every parity gate byte-identical.

## Why it existed to be fixed

Every grid in this engine already converged on a dense palette-indexed representation. The strings
survived only at the *edges* — `get`/`set` spoke `&str` — so each hop between two dense grids
round-tripped through the heap. One line dominated everything:

```rust
// overworld.rs, stitch_veg_region — before
let state = world.get(base_x + lx, y, base_z + lz);
grid.seed(base_x + lx, y, base_z + lz, state.to_string());
```

`48 × 384 × 48` cells × 9 source chunks = **884,736 allocations per warm column**, 97.7% of the
serve path's entire heap traffic, from that one unconditional `to_string()`.

## How it works

Three pieces:

- **`src/interner.rs`** — `StateId(u16)` plus `StateInterner`, an `RwLock`-guarded
  string↔id table. `StateId::AIR` is guaranteed to be id 0. `base_of(id)` returns the id of the
  state's base name (`minecraft:oak_log[axis=y]` → `minecraft:oak_log`), which is the table that
  replaces the crate's five separate `split('[')` helpers — a base name cannot be recovered from a
  `u16` without it.
- **`OverworldGenerator` owns one `Arc<StateInterner>`**, shared by *every* grid it builds. That
  sharing is the whole point: an id read out of one grid is meaningful in another.
- **Both storage layers carry ids.** `DenseBlockGrid`'s local palette became `Vec<StateId>` +
  `HashMap<StateId, u16>`; `VegGrid`'s backing store became
  `HashMap<(i32,i32,i32), StateId>`. Each gained `get_id`/`set_id`-style accessors, with the
  original `&str` forms kept as shims.
- **`DenseBlockGrid` also keeps `palette_names: Vec<&'static str>`**, appended in lock-step with
  `palette`, so `get` stays a plain array read. Without it `get` would resolve through the interner's
  `RwLock` *per cell* — and one `OverworldGenerator` is shared by every concurrent generation call, so
  the carve path's per-cell `CarveGrid::get` would put ~289 threads on one cache line, which is the
  shape `4307b59` was reverted for. Resolving once per new palette entry costs one `name_of` per
  entry (**76 per chunk**, from `palette_intern_new` 10,958 over a 144-chunk sweep) against ~98,304
  cell reads. `palette_names_stays_in_lock_step_with_the_id_palette` guards the pairing, because a
  desynchronised shadow returns the wrong block and no type check can see it.

### Ids are deliberately not world-visible

The obvious worry is that id-assignment order leaks into the served palette and becomes
world-visible — `overworld.rs`'s module doc carries a post-mortem on a `RandomState`
iteration-order bug that already shipped here once. **It cannot, by construction:** a grid's local
palette still holds entries in *first-write order*, exactly as it did when those entries were
`String`s. Interning changed what a palette entry *is*, not the order it is appended in, so
`into_palette_and_blocks` emits a byte-identical `Vec<String>` and `blocks` is untouched.

So the interner may assign ids in any order at all, including one that varies between runs, without
changing a byte of output. `column_is_byte_identical_across_two_independently_constructed_generators`
is the gate that holds this, and it would fail if any id reached the wire.

### Growable, not frozen

Interning a *new* state allocates once, to own the string. A frozen table built exhaustively at
construction would avoid even that, but it would need every state the engine can **synthesise** at
generation time (`[snowy=true]`, leaf `distance=N`, `waterlogged=`), and a missed one has no correct
fallback.

Growable is simpler and sufficient, because **the interner outlives every column the generator
serves.** The allocation budget is written against a *steady-state* column, by which point every
state the data can produce is already interned and `id_of` is a pure lookup. Measured: 65 interner
allocations for a cold column on embedded data, 0 in steady state.

## How to change it, and the gotchas

- **Never store interner ids directly in a grid's `blocks` array.** It is tempting (it would delete
  the palette probe entirely) but it puts id-assignment order on the wire. Re-derive the argument in
  "Ids are deliberately not world-visible" before trying.
- **Do not call `name_of` or `id_of` from a per-block loop.** Both take an `RwLock` guard. That is far
  cheaper than the `String` it replaces, but it is a *shared cache line*, and this repo has a
  measured scar for exactly that shape (`4307b59` reverts a change over cache contention across 289
  concurrent generator calls). The ported hot loops traffic purely in ids.
  `counters::state_name_lookups` exists so a regression into per-block resolution shows up as a
  counter delta instead of an unexplained slowdown.
- **Ids from two different interners are not comparable.** The string-taking constructors
  (`DenseBlockGrid::new`, `VegGrid::new`/`with_footprint`) build a *private* interner, which is right
  for a self-contained parity fixture and wrong for anything exchanging ids. Production must use
  `with_interner` / `with_footprint_interned`. The seams where two id-carrying grids meet carry a
  `debug_assert!` on `StateInterner::instance_id`.
- **Interned names are leaked** (`Box::leak`), because that is what buys `name_of` a `&'static str`
  it can hand out after dropping the read guard — a `Vec<Box<str>>` could not without `unsafe`, which
  is denied workspace-wide. Bounded by distinct block states per generator (a few hundred), not by
  columns served.

### The measurement trap: most stages do not run on a warm column

The per-stage attribution below is the important thing to understand before aiming the next unit:

| stage | steady-state allocs | share |
|---|---|---|
| vegetation | 20,625 | **99.7%** |
| intern | 41 | 0.2% |
| other | 19 | 0.1% |
| *everything else* | **0** | — |

Fill, surface, carve and ore all read **0**, because on a warm column they are cache hits — they do
not execute. So **porting `carver/`, `feature/mod.rs`'s ore path or `surface/` off strings cannot move
`steady_state_heap_allocs_per_column` at all**; those only affect `C_cold`. Anyone continuing Unit 3's
file cluster on the strength of the steady-state counter will measure nothing and conclude wrongly.

Two further caveats on that table:

- The two allocations between 20,684 and 20,686 are `palette_names`' own `vec![default_name]`, one
  per grid constructed — the O(1)-per-grid cost of removing ~98,304 lock acquisitions per carve pass.
- It is **scene-dependent**. `apply_freeze_top_layer` early-returns for biomes that do not list
  `freeze_top_layer`, so seed 42's interior never exercises `top_layer.rs`'s string sites
  (`with_snowy_true`'s `format!`, the `to_owned()` per column). A cold-biome scene would show them.
  This is the "world" species of vacuous measurement from `CLAUDE.md` — the flaw would be in the
  input data, not the instrument.
- The 99.7% in `vegetation` is the **placement engine**, not the store: `get_state` returning
  `Option<String>`, the leaf `distance=N` read-and-rewrite, the `waterlogged` fix-up. That is Unit 8's
  scope, which is why the counter cannot reach 0 until Unit 8 lands.

## Configuration

- **`gen-counters`** (crate feature, default off) — enables `state_intern_new` and
  `state_name_lookups`, and is required for the bench's per-stage allocation attribution, because
  `counters::current_stage()` compiles to a constant `Stage::Other` without it.
  **Verified allocation-neutral**: `steady_state_heap_allocs_per_column` reads the same with the
  feature on *and* off (20,684 measured both ways before the `palette_names` shadow added its two
  per-grid `Vec` allocations; 20,686 after). That control matters — without it the attribution would describe a different
  program from the one the ratchet measures.
- **`LODESTONE_CARVE_HASHMAP_DEBUG`** — the pre-existing debug path that round-trips the world through
  `HashMap<_, String>`; it re-interns through the shared table and stays correct, but it allocates by
  design and is not on the normal path.

## Dependencies

- `src/interner.rs` depends on exactly one thing inside the crate: `crate::counters`. Nothing else —
  no `density`, no `rng`, no `serde_json`. (`counters` itself depends on
  `density::Density::KIND_COUNT`, so that single edge is what would drag `density` into any
  leaf-crate split — see the decomposition note in `docs/plans/worldgen-rewrite.md`.)
- Consumers inside the crate: `dense_grid`, `feature::vegetation`, `overworld`.
- **No external crate is affected.** `GeneratedColumn::block_state`/`into_raw` still return `&str`
  and `Vec<String>`, so all ~200 call sites in `lodestone-server`, `lodestone-shell`,
  `protocol/v770` and `lodestone-testsupport` are untouched. The serve boundary was deliberately not
  crossed: every one of the 885k allocations was internal, so changing `ChunkColumn` or the wire
  encode would have added risk for zero counter movement.

## Gates

- `column_is_byte_identical_across_two_independently_constructed_generators` (`lodestone-server`) —
  the determinism gate, and the one that would catch an id reaching the wire.
- All 13 `lodestone-worldgen` parity binaries (aquifer, carver, chunk, density, feature, mth, noise,
  overworld_gen, region, rng, surface, vegetation) — bit-exact against the committed JVM fixtures.
- `vegetation_reaches_real_blocks_over_a_production_sweep` and
  `plains_grass_patch_attempt_count_matches_the_placement_json`, plus the latter's control
  (`grass_patch_attempt_count_control_fires_when_the_count_modifier_is_removed`).
- `benches/generation.rs`'s calibration, which now asserts the **post**-U3 magnitude with both
  hypotheses named: interner warmup only (measured 65, ceiling 1,000) versus the pre-U3
  `>= 884,736`. It is a ceiling rather than an equality because the exact count is a property of the
  worldgen data, not of this code.
