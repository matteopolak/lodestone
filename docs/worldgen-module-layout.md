# Worldgen module layout and per-unit file ownership

## What it is

The file layout of `crates/lodestone-worldgen`'s generation engine after Unit 16 (the
decomposition unit) of [`plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md), plus the map of
which rewrite unit owns which file. Two files that six units all had to edit —
`src/overworld.rs` (1,873 lines) and `src/feature/vegetation.rs` (3,661) — became two directories,
by pure move, so the rewrite's engine middle can run at width 2 and then 4 instead of width 1.

## Why a file layout is worth a doc

Because in this repo the layout **is** the schedule. One shared checkout with no per-agent
worktrees means two agents editing one file serialize on it, so U4, U6, U7, U9 and U11 — five
independent pieces of work — were queued behind `overworld.rs` for no reason other than that they
lived in the same file. The plan's own scheduling note says it plainly: until U16 lands,
`overworld.rs` allows **at most one in-flight owner at a time**.

The acceptance criterion for U16 is therefore not a line count. It is that
**U4, U6, U7 and U9 own different files afterwards.**

## The layout

### `src/overworld/` — the composed driver (was `overworld.rs`)

| file | holds | owned next by |
|---|---|---|
| `mod.rs` | `OverworldGenerator` and its fields, `new`, the `column`/`column_timed` orchestration, `PreOreCache`/`PostOreCache` and their two memoising wrappers (`pre_ore_stage`, `post_ore_world`) | **U6** (the staged sharded store replaces both caches) |
| `fill.rs` | stages 1–4: `AquiferTrees`, `build_aquifer`, `fill_stage`, `heights_from_field`, `surface_stage`, `materialize_world`, `carve_stage`, and `pre_ore_stage_uncached` | **U4** (flattened density engine), then **U15** (ore veins) |
| `biome.rs` | `DynamicBiome`, `biome_stage` (per-quart, surface height), `biome_for_carver_source` (the separate `y = 0` answer) | **U9** (memoised biome + RTree), then **U11** (3-D quart cells) |
| `decorate.rs` | stages 5–7: `ore_stage`, `vegetation_stage`, `top_layer_stage`, and the two region stitches `stitch_region`/`stitch_veg_region` | **U7** (in-place region view), then **U12** (lakes, springs, geodes…) |
| `output.rs` | `GeneratedColumn`, `StageTimes`, and `intern_from_dense` | read-mostly; changes brokered |

The seam this split used was already load-bearing rather than hypothetical: `column_timed` calls
the *same* private stage functions `column` does, so the stage boundary was an existing, tested
interface before it became a file boundary.

### `src/feature/vegetation/` — vegetal decoration (was `feature/vegetation.rs`)

| file | holds | owned next by |
|---|---|---|
| `mod.rs` | the `VEGETAL_DECORATION` step drivers (`apply_vegetal_decoration_step`, `…_3x3_per_source`) and the dispatch pair `place_placed_feature`/`place_configured_feature` | **U7** (driver/seeding only) |
| `config.rs` | the data layer every feature parses into: `HeightmapKind`, `BlockPredicate`, `BlockStateProvider`, `VegTags`, `VegPlacement`, `Decorator`, `FeatureSizeCfg`, `TreeConfig`, `BlockColumnConfig`, `ConfiguredFeature`/`PlacedRef` resolution | **U8**, then **U12** |
| `tree.rs` | trunk placers (straight / forking / dark-oak 2×2), foliage placers, leaf-`distance` propagation | **U8** |
| `place.rs` | the per-feature placement bodies: `place_simple_block`, `place_block_column`, `place_tree`, `place_beehive_decorator` | **U8** |
| `grid.rs` | `VegGrid` (decoration's read/write surface) and the `census` counters | **U7** |

Splitting this here rather than as U8's first step is deliberate: U8 depends on U7, so "U8's first
step" would have kept the split serialized behind U7's landing. Done in U16 it costs nothing extra
— same pure-move discipline, same gates — and U8's interior can fan out across agents the moment
U7's seam lands.

**Every public path is unchanged.** `crate::feature::vegetation::VegGrid`,
`::census`, `::build_veg_tags` and the rest still resolve: the submodules are private and
glob-re-exported from `mod.rs`. Private items that left `mod.rs` carry `pub(super)`, which is
exactly the scope they already had when the module was one file — visibility is *preserved*, not
widened.

## Reading the older docs

Roughly a dozen docs predate this split and name `crates/lodestone-worldgen/src/overworld.rs` or
`.../src/feature/vegetation.rs`. **Read those as the directory of the same name.** They were not
rewritten in bulk on purpose: they are owned by other in-flight units, and a sweep across sixteen
shared files to change one path each is exactly the kind of collateral edit
[`CLAUDE.md`](../CLAUDE.md) forbids in a shared checkout. Every claim in them about *behaviour*
still holds — U16 moved text and changed nothing else.

## How to change it

**The rule that makes this layout worth anything: a split never shares a commit with a logic
change.** A "pure move" that accidentally reorders an RNG draw changes the generated world, and
landed mixed with a rewrite the parity failure gets attributed to the wrong change. So if you need
both, the move lands first, green, alone.

To move code between these files:

1. Move it. Do not reformat it, do not reorder it, and do not run `cargo fmt` — a formatter run
   across a moved file makes the move undiffable and destroys the only review mechanism a pure move
   has.
2. Bump visibility to `pub(super)` if the new home is a sibling module. Nothing else about the
   signature may change.
3. Prove conservation, do not eyeball it. `git diff --color-moved=dimmed-zebra` shows *ordering*;
   a line-multiset comparison (old file vs the concatenated new files, allowing only `//!`/`use`/
   `mod`/`pub use`/`impl` plumbing to appear and only `use` lines to be redistributed) shows
   *conservation*, which is the property that matters. U16 used the second, with a control that
   corrupted one RNG line and confirmed the comparison caught it — an absence claim needs evidence
   the detector fires.
4. Run the gates **in the same commit**: the byte-identity control
   (`lodestone_server::worldgen_data::tests::column_is_byte_identical_across_two_independently_constructed_generators`),
   all 13 worldgen `*_parity` binaries, the composed fixture gate, the two production-seam
   vegetation gates in `crates/lodestone-server/src/worldgen_data.rs`, plus `just health`.
   Old-engine equality alone is insufficient — the JVM fixtures run too.

### Gotchas

- **A method that moves to a sibling module needs `pub(super)`, and the compiler is the only thing
  that tells you.** Private items in a *parent* module (`overworld/mod.rs`) are visible to
  children automatically; the reverse is not true. This is why `mod.rs` needed no bumps and every
  other file did.
- **Struct fields are the same story and are easier to miss**, because the struct itself being
  `pub(super)` is not enough. `DynamicBiome` and `AquiferTrees` are constructed in
  `overworld/mod.rs`'s `new` but defined in sibling modules, so every field needed the bump too.
- **The `#[cfg(test)] mod tests` block in `feature/vegetation/mod.rs` uses `use super::*` and
  therefore sees `pub(super)` items through `mod.rs`'s glob imports.** That is what let ~600 lines
  of fixtures move with zero edits. If you split those tests out later, they lose that and will
  need explicit paths.
- **`census` stayed nested inside `grid.rs` rather than becoming `census.rs`** for one reason: a
  file of its own would have required de-indenting all 149 lines by four spaces, which is 149
  changed lines in a diff that is supposed to have none. `mod.rs` re-exports it so the public path
  is unchanged.

## The leaf sub-crate, and a correction to the plan

U16's third phase was to extract `{hash, math, rng, noise, density}` as a
`lodestone-worldgen-core` leaf crate. **It was not done, and the plan's stated premise for it
is wrong.** Both halves of that matter, so here is the measurement.

The plan records a `use crate::` scan concluding "a closed set — `math` imports nothing
crate-internal, `rng` only `hash`, `noise` only `math`/`rng`, `density` only
`math`/`noise`/`rng`". Re-run distinguishing **code** lines from **doc-comment** lines:

| `crate::` target outside the set | code call sites | doc-comment mentions |
|---|---|---|
| `counters` | **8** | 2 |
| `feature` | 0 | 6 |
| `overworld` | 0 | 2 |
| `biome` | 0 | 2 |
| `aquifer` | 0 | 1 |
| `compose` | 0 | 1 |

So the plan is right about five of six and silent about the one that matters. `density` and
`rng` really do call `crate::counters` (`density/chunk.rs`'s `bump_density_eval`,
`bump_corner_lookup`, `bump_slot_hit`; `rng`'s `next_bits` hook), and **`counters` in turn
depends on `density::Density::KIND_COUNT`** for two array sizes and a loop bound
(`counters.rs:151`, `:162`, `:238`, `:303`). That is a cycle, and a cycle cannot be cut by a
crate boundary — extracting the plan's five modules as written would put 8 call sites in the
new crate pointing back at the old one.

The set that *is* closed is the same five **plus `counters`** — measured zero code edges out,
~4,372 lines. That is the extraction to do, and it is mechanical.

Two consequences worth carrying forward:

- **`KIND_COUNT` is not the blocker it was reported to be, and does not need to move.** Making
  it local to `counters` is not a clean move in either direction: it cannot leave `density`,
  because `KIND_NAMES`'s array length and two of `density`'s own tests are written against it,
  so the only available shape is a *duplicate* constant plus a drift guard — and that guard,
  living in `counters`, re-creates the very `counters -> density` edge it was meant to remove,
  as a dev-dependency. With `counters` inside the core crate instead, the edge is intra-crate
  and there is nothing to fix.
- **A crate split needs three gates a file split structurally cannot fail**: `just check-seam`,
  `cargo xtask check-isolation` (wasm confinement) and `cargo xtask check-connected`
  (the plugins→engine dependency direction). It also needs a new workspace member in the root
  `Cargo.toml` and the `gen-counters` feature forwarded from `lodestone-worldgen` to the new
  crate — both **outside** `crates/lodestone-worldgen/**`. U16 was scoped as a pure move inside
  that directory, so this is a hand-off rather than a widening.

U4 has a stake in this specifically: the plan wants U4's new `engine/` born inside the leaf
crate rather than moved there later, so U4 should not inherit the five-module figure.

## Configuration

None. This is a source layout; no feature flag, env var or build setting selects it.

## Dependencies

Internal only, and the split did not add one. `overworld/` depends on the crate's `aquifer`,
`biome`, `carver`, `compose`, `counters`, `dense_grid`, `density`, `feature`, `interner`, `rng` and
`surface` modules exactly as `overworld.rs` did; `feature/vegetation/` on `density::Resolver`,
`feature::{BlockPos, IntProvider}`, `interner` and `rng`.
