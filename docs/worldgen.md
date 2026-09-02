# Worldgen engine overview

## What it is

`crates/lodestone-worldgen` (engine) and `crates/lodestone-worldgen-core` (numeric leaf crate) are
a version-free port of vanilla Minecraft 26.2's world generator: a density-function/noise-router
interpreter, biome search, surface rules, carvers, aquifer, ore/vegetation placement and structures,
all driven by the JSON data Mojang ships rather than by hardcoded logic. This doc covers the module
layout, the density engine that turns that data into per-block terrain, the RNG and parity
discipline every stage depends on, and how a chunk is produced end to end. Biomes, structures,
decoration and the Nether/End have their own docs (see Dependencies).

## How it works

### Module layout

`OverworldGenerator` (the composed driver) lives under `src/overworld/`, split by stage rather than
kept as one file:

| file | holds |
|---|---|
| `overworld/mod.rs` | `OverworldGenerator`, `column`/`column_timed`, the staged-store wiring |
| `overworld/fill.rs` | shape, aquifer, surface, carve (stages 1–4) |
| `overworld/biome.rs` | biome cell sampling |
| `overworld/decorate.rs` | ore, vegetation, top-layer decoration (stages 5–7) and the 3×3 region stitches |
| `overworld/output.rs` | `GeneratedColumn`, per-stage timings, interning to the served format |
| `overworld/structures.rs` | structure starts/refs/placement (see `worldgen-structures.md`) |

`feature/vegetation/` (config parsing, trunk/foliage placers, grid, placement) is split the same
way. Every public path (`crate::feature::vegetation::VegGrid`, etc.) is unchanged by the split —
submodules are private and re-exported.

The numeric core (`rng`, `hash`, `math`, `noise`, `density`, `counters`) lives in the leaf crate
`lodestone-worldgen-core`, which depends on nothing outside itself but `std` and `serde_json` (for
`density`'s JSON-parsed graph). `lodestone-worldgen` re-exports those modules, so callers inside or
outside the crate see no path change.

### The density/noise engine

A density function graph (`Density`, compiled by `engine::graph::Program`) is vanilla's
`DensityFunction` tree, interpreted two ways:

- **Point interpreter** (`Density::compute`) — evaluates one `(x, y, z)` at a time; used by leaves
  (`spline`, `old_blended_noise`, `find_top_surface`, `end_islands`) and by aquifer/surface.
- **Block field** (`engine::field`, driven through `NoiseChunkSampler`) — fills a whole chunk,
  pre-computing a 4×8×4 cell's eight corners once and trilinearly interpolating the rest.

**The interpolation order is bit-significant.** Vanilla's `NoiseChunk` pre-fills its cell array with
`Mth.lerp3` (X-inner nesting) via an in-code `cache_all_in_cell` marker that never appears in any
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

Everything the graph evaluates must preserve vanilla's IEEE-754 evaluation order exactly: `Mul`
short-circuits on an exact `0.0` first operand without evaluating the second (so the field walk must
stay recursive descent, never a bottom-up sweep), no `mul_add`/FMA anywhere, no reassociation of an
octave accumulation chain, and no folding a `0.0 *` multiply that could carry a sign (`-0.0` vs
`0.0` diverge downstream). SIMD vectorization (`lodestone-worldgen-core`'s `noise/improved.rs`,
nightly `#![feature(portable_simd)]`) lanes only independent lattice positions, never across an
accumulation chain, for the same reason — it is the one place lanes are safe.

### RNG

`lodestone-javarandom` (see its own doc) is the workspace's one `java.util.Random` port; worldgen
additionally needs `LegacyRandomSource`/`XoroshiroRandomSource` (`rng::Algorithm`,
`AnyRandomSource`/`AnyPositionalFactory`), because `noise_settings`'s `legacy_random_source` flag
switches vanilla's **entire** noise stack between the two families per dimension. `AnyPositionalFactory`
is `Copy` because both variants are, which is what keeps a dimension-aware `Builder::with_algorithm`
to a type-name change in four files rather than a generic parameter through every stage.

### World-type selection

`worldgen_data::WorldType` (`Overworld`/`Amplified`/`LargeBiomes`) all share one
`OverworldGenerator::new(seed, settings, resolver, …)`, parameterised entirely by which
`noise_settings` document it is handed — no new engine code. `single_biome_generator` reuses the
same generator through its pre-existing fixed-biome fallback path (an empty `Resolver::biome_parameters`
disables climate sampling). `flat`/`flat_all_dimensions` and `debug_all_block_states` are structurally
different, seed-free generators (`lodestone_worldgen::flat`, `::debug`) with their own `ChunkSource`
wrappers. All seven bundled `world_preset/*.json` documents have a working generator; only three
(`normal`/`amplified`/`large_biomes`) are wired into the world-creation screen today — the other four
need their entry points re-exported from `lib.rs` plus UI affordances.

### Chunk generation, end to end

`OverworldGenerator::column` runs, in order: structure starts/refs → beardifier lookup → shape (density
graph) → aquifer → biome cells → surface rules → carve → ore → vegetation → top-layer (snow/ice) →
structure placement → intern to the served format. Everything above stage 4 that touches a
neighbourhood (carve's 17×17, ore/vegetation's 3×3) is served through the staged store below, and every
stage after decoration operates on the same per-chunk `DenseBlockGrid`, addressed by interned `StateId`s
rather than block-state strings.

### Memoisation: the staged store

`overworld/store.rs`'s `StagedStore` gives every `(chunk, stage)` pair a `OnceLock`-backed slot inside
one of 64 independent shard locks, so a hit costs one atomic load and a miss runs its computation
exactly once — structurally, not by convention — even when hundreds of concurrent generation calls
request overlapping 3×3-of-3×3 neighbourhoods. Eviction is scoped to the in-flight view (never a
capacity-FIFO), because a FIFO cache that evicts a still-needed neighbour turns into silent recompute;
the retention ceiling and pin radius are derived from the widest driver's closure (currently radius 10,
1,369-chunk worst case) and must be re-derived whenever a stage's neighbourhood widens.

### Cost attribution

Interning block-state strings into `u16` `StateId`s (below) and moving the surface stage off
per-probe `String` allocation removed essentially all of worldgen's heap traffic; what remains is
CPU. A steady-state warm column spends roughly a quarter of its time in the density engine itself
(aquifer + shape), with ore and vegetation placement the largest remaining shares — these numbers
shift with scene and biome, so re-measure locally (`benches/generation.rs`) rather than trusting a
recorded split.

## How to change it

- **Never share a commit between a pure file move and a logic change.** A "just relocating this"
  commit that also reorders an RNG draw changes the generated world, and a parity failure gets
  attributed to the wrong thing. Land the move alone, green, first.
- **A method moved to a sibling module needs `pub(super)`**, checked by the compiler; a *field* on a
  struct constructed in the parent but defined in a sibling needs it too, and nothing but a build
  failure catches a missed one.
- **Adding a `Density` variant touches at least five places**, only three of which are compile
  errors: `graph.rs`'s `compile_node`, `field.rs`'s `eval`, `OpKind`'s discriminant (must equal
  `Density::kind_index()` — only a dedicated test catches a mismatch), `Density::write_signature`
  (for node-sharing/hash-consing — floats go in as `to_bits()`, never compared values), and
  `graph.rs`'s `walk_interpolating`, which silently drops a node from `interpolating_slots` if you
  forget its arm. Append new variants; never insert in the middle, since a saved counter table is
  indexed by position.
- **Do not "fix" the interpolation order to the incremental chain** — read the density-engine
  module's own doc on interpolation order first.
- **When you add a caller that resolves per-chunk state, route it through the existing memo/store
  rather than adding a second cache.** A `Mutex`-guarded FIFO cache under concurrent load has already
  cost this repo a reverted change once; sharded once-only slots or a thread-local direct-mapped memo
  (biome search's per-source-chunk lookup is the latter) are the two patterns in use.
- **Measure before adding or removing a cache.** A memo that helped at one call site measured a
  0.12% hit rate at another and was actively worse than not caching; sizing anything here means
  running the counters (`gen-counters` feature), not reasoning from the shape of the code.
- **A cross-chunk decoration driver (ore, vegetation) must make each source chunk's pass a pure
  function of `(seed, chunk)` — never of which chunk is the recompute's centre.** Vanilla decorates a
  chunk once and persists spill into neighbours; this engine recomputes per centre, so a read that
  depends on centre-relative distance (not absolute chunk identity) silently disagrees between two
  chunks that both compute the same seam. Widening a *read* neighbourhood is usually free (the extra
  chunks are often already memoised for another reason); isolating *writes* between sources that share
  one overlay is a real behavioural change and needs its own re-baselined parity numbers if you take it.

## Configuration

- `gen-counters` (crate feature, default **off**) — turns density/cache/RNG-draw counters from
  compiled-out constants into live atomics. Must be forwarded explicitly
  (`gen-counters = ["lodestone-worldgen-core/gen-counters"]`) from any crate that re-exports the core,
  or every counter silently reads zero.
- `#![feature(portable_simd)]` (nightly, pinned in `rust-toolchain.toml`) — the noise kernel's only
  vectorised path; there is deliberately no scalar fallback, so as not to run two different
  implementations from one seed.
- No other env vars or flags select engine behaviour; everything else is data through `Resolver`.

## Dependencies

`lodestone-worldgen-core` (numeric leaf: rng/hash/math/noise/density/counters) ← `lodestone-worldgen`
(engine: overworld/biome/surface/aquifer/carver/feature/structure) ← `lodestone-server`
(`worldgen_data::EmbeddedResolver`, the bundled 26.2 JSON under `assets/worldgen/`) ← `lodestone-shell`
(the singleplayer path, same generator). `lodestone-javarandom` for the shared `java.util.Random` port.

Verification is against a real vanilla 26.2 server, never against this engine's own output:
`scripts/worldgen-oracle/*.java` (run via `scripts/worldgen-oracle/run.sh` under Apple `container`, no
host JDK needed) drives the running server's own methods and dumps results as committed fixtures under
each crate's `tests/support/`; `crates/lodestone-worldgen-parity` is the shared chunk-for-chunk
comparison harness (`cargo run -p lodestone-worldgen-parity --bin compare`/`regen`). A second,
independent oracle is a vanilla-authored save (`.cache/mc/survival/world`, seed −195764831) read
directly off disk with no dependency on this repo's own encoder. See `docs/worldgen-biomes.md`,
`docs/worldgen-structures.md`, `docs/worldgen-decoration.md` and `docs/worldgen-dimensions.md` for the
subsystems built on this engine, and `docs/oracle-runtimes.md` for the oracle runtime itself.
