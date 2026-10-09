# Worldgen engine overview

## What it is

World generation spans four crates. `lodestone-worldgen-core` holds the numeric leaf (RNG, hashing, the `f64` noise vegetation samples, counters) and the 26.3 engine (`engine::release26_3`: noise router, biomes, aquifer, surface rules, carvers); `lodestone-worldgen-feature-26-3` is the placed-feature decorator; `lodestone-worldgen` composes them into `terrain263::Terrain263`, the generator every dimension and noise-based world type is served from, and owns the structure engine, the flat and debug generators and the bundled-data resolver. This doc covers the layout and the numeric rules every stage depends on; the generator itself is in [26.3 world source](worldgen-world-263.md).

## How it works

### Crate layout

| module | holds |
|---|---|
| `terrain263` | `Terrain263`: shaped chunks, decoration, structure starts and placement for one dimension and seed |
| `structure` | structure sets, start search, jigsaw/coded/template pieces, per-chunk placement (`structure::chunk`) |
| `feature` (private) | placers a jigsaw feature-pool element invokes, over `VegGrid` |
| `block_entities` | block entities generation produces (beehives, dungeon chests and spawners) |
| `flat`, `debug` | seed-free flat and debug-world generators |
| `dense_grid`, `generated_storage` | dense block fields and compact column storage |
| `table_resolver` | `TableResolver`, lookup over bundled JSON and structure templates |
| `spawners`, `spawn_stage` | biome mob-spawn tables and the generation-time spawn pass |
| `generator` | the `ChunkGenerator` seam plugins implement |

`lodestone-worldgen` re-exports the core's `counters`, `engine`, `hash`, `math`, `noise`, `rng` under the same paths; the `Resolver` trait lives in `resolver::Resolver`. Numeric and kernel code goes in the core, composition here.

### Bundled server data

`lodestone-server/build.rs` walks its worldgen, loot, structure, recipe and item-tag asset trees and writes sorted lookup tables into `$OUT_DIR`. Each include keeps only the path relative to its asset tree and resolves it through `CARGO_MANIFEST_DIR`, because a reused target directory outlives temporary checkouts or worktrees (never retain the build script's absolute path). Add files under the relevant `crates/lodestone-server/assets/` directory; the script tracks them. The resolver's freeze and survival fact factories enumerate the selected release's wire-to-canonical state map including its default-state choices; the process-wide canonical registry also holds newer-release identities, so its size is not the bundle's state domain. Changing a bundle means changing these fact bindings with its assets, not extending an older motion predicate to unsupported states.

### Numeric rules

Everything evaluated must preserve the reference's IEEE-754 evaluation order: no `mul_add`/FMA, no reassociation of an octave accumulation chain, no constant folding whose short-circuit result is unproven (`-0.0` vs `0.0` diverge downstream). SIMD lanes evaluate only independent gradient corners in `noise/improved.rs` ([noise kernels](worldgen-noise-kernels.md)); nothing vectorises an accumulation chain ([26.3 engine](worldgen-engine-26-3.md)). The geode distance field follows the same rule: each point's inverse-square-root term is added to the sampled noise offset first, then that rounded contribution to the running shell or crack sum; keep both additions explicit, as combining them changes the threshold comparison for some inputs.

### RNG

`lodestone-javarandom` is the workspace's one `java.util.Random` port. Worldgen also needs the legacy linear-congruential and the Xoroshiro random sources (`rng::Algorithm`, `AnyRandomSource`/`AnyPositionalFactory`), since `noise_settings`' `legacy_random_source` flag switches the **entire** noise stack between the two families per dimension; `AnyPositionalFactory` is `Copy`, so the family can be chosen at run time without a generic through every stage.

`WorldgenRandom<R>` is a bit-random wrapper, not a delegation wrapper: its `double` and Gaussian draws are built from its own `next_bits` funnel, so each accepted Gaussian pair uses four wrapped bit draws. The cached mate belongs to the wrapper and must not call `R::next_gaussian` (its cache and double-draw shape can differ). Its forwarding reseed leaves the wrapper cache alive; a positional fork delegates to `R` and keeps the family. `RngOracle` records two fixed-seed Gaussian pairs, the following `long` and a reseed-between-pair control; keep all three (the `long` exposes a cache or draw-count mistake even where platform logarithm rounding leaves the Gaussian's last bit variable).

### World types and chunk generation

Every noise-based world type (`worldgen_data::WorldType` `Overworld`/`Amplified`/`LargeBiomes`, and the single-biome world) is served from the 26.3 engine. `flat`/`flat_all_dimensions` and `debug_all_block_states` are structurally different seed-free generators with their own `ChunkSource` wrappers (`worldgen_data::flat_chunk_source`, `debug_chunk_source`). `lodestone_server::Terrain263ChunkSource` asks `Terrain263` for shaped terrain (noise fill, surface rules, carvers), then decorates the nine surrounding source chunks in `terrain263::DECORATION_SOURCE_OFFSETS` order, each step running structure placement before its features.

Terrain adaptation does not build the full 17x17 structure-reference product: `StructureRegistry::origin_candidates_in` inverts random-spread cells and consults the same biome-relocated concentric-ring list as the complete start walk, and the beardifier evaluates only those sparse origins. Each surviving origin resolves through the ordinary memoised start slot (no second start computation or RNG path).

### Structure-pool tree leaf distance

After a structure-pool tree ([structure pool features](worldgen-structure-pool-features.md)) writes trunk, foliage and decorators, `place_tree` runs a leaf-distance pass over the bounding box of those writes. It seeds bucket zero from the trunk positions and walks only log and distance-carrying leaf states, with per-distance worklists preserving the reference's position-set hashing, resize and iteration order. Positions are marked filled when popped, so stale entries stay observable and can rewrite a leaf after an earlier smaller distance; this is deliberately not a shortest-path queue with visited-on-enqueue. Start with the external-value control in `feature/vegetation/tree.rs` when changing it; keep `VegTags` membership checks and the `StateId` rewrite on the hot loop, `bbox` covering every write from the one tree, and only trunk positions seeding.

## How to change it

- Never share a commit between a pure file move and a logic change (a move that reorders an RNG draw changes the world and misattributes a parity failure).
- Keep embedded data lookup in `TableResolver`: embedders supply sorted JSON/template tables and any version-specific census without recreating `Resolver` methods. Production embedders may attach the resolver's fingerprint-checked `JsonCache` to retain parsed documents across seeds; custom or mutable resolvers leave it detached.
- Keep structure setup seed-independent: `StructureRegistry` caches a fingerprinted blueprint (decoded templates, reachable pools) and shares the ring-position list per seed and sampler identity. A resolver whose assets can change must return no fingerprint, taking the fallback construction path.
- Measure before adding or removing a cache, with the `gen-counters` feature rather than reasoning from code shape.

## Configuration

- `gen-counters` (crate feature, default off) turns noise, column, structure and RNG-draw counters from compiled-out constants into live atomics. Forward it explicitly (`gen-counters = ["lodestone-worldgen-core/gen-counters"]`) from any crate re-exporting the core or every counter silently reads zero; `tests/gen_counters_forward.rs` gates it.
- `#![feature(portable_simd)]` (nightly, pinned in `rust-toolchain.toml`) is the noise kernel's only vectorised path, with deliberately no scalar fallback so one seed never runs two implementations.

## Dependencies

`lodestone-worldgen-core` <- `lodestone-worldgen-feature-26-3` <- `lodestone-worldgen` <- `lodestone-server` (chunk sources, bundled structure-template tables under `assets/worldgen/`); `lodestone-worldgen-data-26-3` supplies the bundled 26.3 tables; `lodestone-javarandom` is the shared RNG port. Verification is against a real vanilla server, never this engine's own output: oracles under `scripts/` dump fixtures under each crate's `tests/support/` ([26.3 world source](worldgen-world-263.md), [structures](worldgen-structures.md), [oracles and benchmarks](oracles-and-benchmarks.md)).
