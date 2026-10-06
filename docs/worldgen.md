# Worldgen engine overview

## What it is

World generation is split across four crates. `lodestone-worldgen-core` holds the numeric leaf
(RNG, hashing, noise, the density interpreter, counters) and the 26.3 engine
(`engine::release26_3`: noise router, biomes, aquifer, surface rules, carvers).
`lodestone-worldgen-feature-26-3` is the 26.3 placed-feature decorator.
`lodestone-worldgen` composes them into `terrain263::Terrain263`, the generator every dimension and
noise-based world type is served from, and owns the structure engine, the flat and debug world
generators and the bundled-data resolver. This doc covers the crate layout and the numeric rules
every stage depends on; the generator itself is in [26.3 world source](worldgen-world-263.md).

## How it works

### Crate layout

| module | holds |
|---|---|
| `terrain263` | `Terrain263`: shaped chunks, decoration, structure starts and placement for one dimension and seed |
| `structure` | structure sets, start search, jigsaw/coded/template pieces, per-chunk placement (`structure::chunk`) |
| `feature` (private) | the feature placers a jigsaw feature-pool element invokes, over `VegGrid` |
| `block_entities` | the block entities generation produces (beehives, dungeon chests and spawners) |
| `flat`, `debug` | the seed-free flat and debug-world generators |
| `dense_grid`, `generated_storage` | dense block fields and compact column storage |
| `table_resolver` | `TableResolver`, lookup over the bundled JSON and structure templates |
| `spawners`, `spawn_stage` | biome mob-spawn tables and the generation-time spawn pass |
| `generator` | the `ChunkGenerator` seam plugins implement |

`lodestone-worldgen` re-exports the core's `counters`, `density`, `engine`, `hash`, `math`, `noise`
and `rng` under the same paths, so `lodestone_worldgen::density::Resolver` resolves from either crate.
Add numeric and kernel code to the core and composition code here.

### Bundled server data

`lodestone-server/build.rs` walks its worldgen, loot, structure, recipe and item-tag asset trees and
writes sorted lookup tables into Cargo's `$OUT_DIR`. Each generated include keeps only the path
relative to its asset tree and resolves it through the compiling package's `CARGO_MANIFEST_DIR`.
This matters when Cargo reuses a target directory after a temporary checkout or worktree has gone
away: generated source must not retain the build script's old absolute path. Add or refresh bundled
files under the relevant `crates/lodestone-server/assets/` directory; the build script tracks those
directories and regenerates the corresponding table automatically.

The bundled resolver's freeze and survival fact factories enumerate the selected
release's wire-to-canonical state map, including its own default-state choices.
The process-wide canonical registry also contains appended identities from newer
releases; its total size is not the generation bundle's supported state domain.
Changing a generation bundle requires changing these fact bindings alongside its
assets, rather than extending an older motion predicate to unsupported states.

### The density/noise engine

A density function graph (`Density`, compiled by `engine::graph::Program`) is vanilla's
`DensityFunction` tree, interpreted two ways:

- **Point interpreter** (`Density::compute`) — evaluates one `(x, y, z)` at a time; used by leaves
  (`spline`, `old_blended_noise`, `find_top_surface`, `end_islands`).
- **Block field** (`engine::field`, driven through `NoiseChunkSampler`) — fills a whole chunk,
  pre-computing each cell's eight corners once and trilinearly interpolating the rest. The cell
  width and height come from the settings document (`size_horizontal` and `size_vertical`, each
  multiplied by four): the usual pair is 4×8, while the End uses 8×4. Aquifer and optional vein
  samplers carry that same pair instead of assuming one world-wide lattice.

**The interpolation order is bit-significant.** Vanilla's own noise-chunk sampler pre-fills its cell array with
its own plain-trilinear lerp (X-inner nesting) via an in-code `cache_all_in_cell` marker that never appears in any
`noise_settings` JSON — a census of the data alone will not find it. The alternative, Y-inner
incremental nesting vanilla's driver loop *looks* like it uses, differs at the last ULP and is
wrong; `interpolation_order` tests assert the two orders still disagree, inverted so a future change
that makes them agree fails loudly.

Two cache layers exist in the field evaluator (a per-cell lookup cache and a per-corner-slot
evaluation cache) plus a one-slot last-value memo for point-evaluated leaves reached from inside the
field walk, and a full `(node, x, z)` map for the point interpreter's own `flat_cache`/`cache_2d`
subtrees (the one-slot form vanilla's own `Cache2D` uses cannot survive the field walk's alternating
corner-fetch order). A `Density` node is 14× the size of a compiled `Op`, which is why the graph is
compiled into a flat, `Arc`-shared, lock-free `Program` rather than walked as boxed `Density` trees
per chunk — one compiled graph backs unlimited concurrent chunk generation with zero clones on the
hot path.

Compilation also folds side-effect-free constant arithmetic and routes selectors
whose input is constant, eliminating their unreachable child graph before the
field walk. Cache-writing wrappers remain explicit: even a numerically constant
interpolation must retain its slot write and corner order. Graph controls cover
IEEE signed zero, selector branch choice, and this cache-write boundary.

Everything the graph evaluates must preserve vanilla's IEEE-754 evaluation order exactly: `Mul`
short-circuits on an exact `0.0` first operand without evaluating the second (so the field walk must
stay recursive descent, never a bottom-up sweep), no `mul_add`/FMA anywhere, no reassociation of an
octave accumulation chain, and no folding a multiply until its first operand is proven constant
and the exact short-circuit result is known (`-0.0` vs `0.0` diverge downstream).
SIMD lanes evaluate independent gradient corners in `noise/improved.rs` and
independent field-cell positions in `engine/field.rs`; neither vectorizes an
accumulation chain. Improved noise uses nightly portable SIMD, while canonical
field cells use `fearless_simd`'s token-generic kernels and scalar fallback.

The geode distance field follows the same rule: each point's inverse-square-root term is added to
the sampled noise offset first, then that rounded contribution is added to the running shell or
crack sum. Keep those two additions explicit when changing the geode loop; combining them changes
the threshold comparison for some IEEE-754 inputs.

### RNG

`lodestone-javarandom` (see its own doc) is the workspace's one `java.util.Random` port; worldgen
additionally needs `LegacyRandomSource`/`XoroshiroRandomSource` (`rng::Algorithm`,
`AnyRandomSource`/`AnyPositionalFactory`), because `noise_settings`'s `legacy_random_source` flag
switches vanilla's **entire** noise stack between the two families per dimension. `AnyPositionalFactory`
is `Copy` because both variants are, which is what keeps a dimension-aware `Builder::with_algorithm`
to a type-name change in four files rather than a generic parameter through every stage.

`WorldgenRandom<R>` is a bit-random wrapper, not a delegation wrapper: its ordinary `double` and
Gaussian draws are constructed from its own `next_bits` funnel, so each accepted Gaussian pair uses
four wrapped bit draws. The cached mate belongs to the wrapper rather than `R`; it must not call
`R::next_gaussian`, whose cache and double-draw shape can differ. Its forwarding reseed deliberately
leaves that wrapper cache alive, while a positional fork delegates to `R` and retains the selected
family. `RngOracle` records two fixed-seed Gaussian pairs, the following `long`, and a reseed-between-
pair control; keep all three when changing this seam. The following `long` makes a cache or draw-count
mistake observable even where platform logarithm rounding leaves the Gaussian's last bit variable.

### World-type selection

The server builds every noise-based world type (`worldgen_data::WorldType`
`Overworld`/`Amplified`/`LargeBiomes`, and the single-biome world) from the 26.3 engine, not from this
crate's generators; see [26.3 world source](worldgen-world-263.md). `flat`/`flat_all_dimensions` and
`debug_all_block_states` are structurally different, seed-free generators (`lodestone_worldgen::flat`,
`::debug`) with their own `ChunkSource` wrappers (`worldgen_data::flat_chunk_source`,
`debug_chunk_source`).

### Chunk generation, end to end

`lodestone_server::Terrain263ChunkSource` asks `Terrain263` for a chunk's shaped terrain (noise fill,
surface rules, carvers), then decorates the nine source chunks around it in
`terrain263::DECORATION_SOURCE_OFFSETS` order, each step running structure placement before its
features. The details, and what each oracle checks, are in [26.3 world source](worldgen-world-263.md).

Terrain adaptation does not build the complete 17×17 structure-reference product that placement and
persistence need. `StructureRegistry::origin_candidates_in` inverts random-spread cells and consults
the same biome-relocated concentric-ring list as the complete start walk, then the beardifier
evaluates only those sparse origins. Each surviving origin still resolves through the ordinary
memoised start slot; there is no second start computation and no separate RNG path.

### Structure-pool tree leaf-distance post-processing

After a structure-pool tree ([structure pool features](worldgen-structure-pool-features.md)) writes
its trunk, foliage and decorators, `place_tree` runs the leaf-distance pass
over the bounding box of those writes. The pass seeds bucket zero from the trunk positions and walks
only log and distance-carrying leaf states. Its per-distance worklists preserve source position-set
hashing, resize and iteration order. Positions are marked filled when popped, so stale entries remain
observable and can rewrite a leaf after an earlier, smaller distance; this is intentionally not a
shortest-path queue with a visited-on-enqueue set.

When changing this pass, start with the focused external-value control in `feature/vegetation/tree.rs`. Keep the `VegTags` membership checks and `StateId` rewrite
path on the hot loop; block-state strings are only used when constructing test fixtures. The `bbox`
must continue to include every write from the one tree, while only trunk positions seed propagation.

## How to change it

- **Never share a commit between a pure file move and a logic change.** A "just relocating this"
  commit that also reorders an RNG draw changes the generated world, and a parity failure gets
  attributed to the wrong thing. Land the move alone, green, first.
- **Adding a `Density` variant touches at least five places**, only three of which are compile
  errors: `graph.rs`'s `compile_node`, `field.rs`'s `eval`, `OpKind`'s discriminant (must equal
  `Density::kind_index()`; only a dedicated test catches a mismatch), `Density::write_signature`
  (for node-sharing; floats go in as `to_bits()`, never compared values), and `graph.rs`'s
  `walk_interpolating`, which silently drops a node from `interpolating_slots` if you forget its
  arm. Append new variants; never insert in the middle, since a saved counter table is indexed by
  position.
- **Do not "fix" the interpolation order to the incremental chain.** Read the density-engine
  module's own doc on interpolation order first.
- **Keep embedded data lookup in `TableResolver`.** An embedding crate supplies its sorted
  JSON/template tables and any version-specific block-state census; it must not recreate `Resolver`
  methods. Production embedders may attach the resolver's fingerprint-checked `JsonCache` to retain
  parsed documents across seed changes; custom or mutable resolvers should leave it detached.
- **Keep structure setup seed-independent.** `StructureRegistry` caches a fingerprinted blueprint
  (including decoded templates and reachable pools) and shares the resulting ring-position list for
  the same seed and sampler identity. A resolver that can change its assets must return no
  fingerprint so it always takes the fallback construction path.
- **Measure before adding or removing a cache.** Sizing anything here means running the counters
  (`gen-counters` feature), not reasoning from the shape of the code.

## Configuration

- `gen-counters` (crate feature, default **off**) turns density, cache and RNG-draw counters from
  compiled-out constants into live atomics. It must be forwarded explicitly
  (`gen-counters = ["lodestone-worldgen-core/gen-counters"]`) from any crate that re-exports the core,
  or every counter silently reads zero; `tests/gen_counters_forward.rs` gates that.
- `#![feature(portable_simd)]` (nightly, pinned in `rust-toolchain.toml`) is the noise kernel's only
  vectorised path; there is deliberately no scalar fallback, so as not to run two different
  implementations from one seed.
- `fearless_simd` and `fearless_simd_macros`: canonical field cells select a SIMD token once per cell
  and use one arithmetic body across native and scalar backends.

## Dependencies

`lodestone-worldgen-core` ← `lodestone-worldgen-feature-26-3` ← `lodestone-worldgen` ←
`lodestone-server` (the chunk sources and the bundled structure-template tables under
`assets/worldgen/`). `lodestone-worldgen-data-26-3` supplies the bundled 26.3 tables.
`lodestone-javarandom` is the shared `java.util.Random` port.

Verification is against a real vanilla server, never against this engine's own output: oracles under
`scripts/` drive the running server's own methods and dump results as committed fixtures under each
crate's `tests/support/`. See [26.3 world source](worldgen-world-263.md),
[structures](worldgen-structures.md) and [oracles and benchmarks](oracles-and-benchmarks.md).
