# Worldgen rewrite plan

## What it is

The standing rules for the worldgen engine rewrite: bit-exact parity with the reference game defines correct, and speed is pursued only where it cannot move an RNG draw. The engine itself is in [worldgen](../worldgen.md); this doc keeps the rules and goals that still bind new worldgen work.

## Rules

- **Parity is bit-exact.** Placement preserves the reference depth-first feature traversal and RNG-consumption order. The per-stage `*_parity.rs` suites and the JVM oracles (`FeatureOracle`, `VegetationOracle`, `ComposedChunkOracle`) define correct. A unit touching placement says whether it can move an RNG draw.
- **Cutover gate.** Replacing a stage needs byte-identical output against the old path across several seeds and the JVM fixtures, in the same commit that deletes the old path (two live paths are two worlds from one seed).
- **Rollback rule.** A mismatch at any gate seed blocks landing. Localise to one stage with per-wrapper fixtures, then let the JVM oracle break ties; never widen a tolerance, drop a seed or relabel a mismatch "expected" without a JVM fixture.
- **Float dust is an algorithm bug.** A wrong interpolation order scored 90,563/98,304 on the chunk gate, every miss a last-place float difference that looked like an epsilon problem.
- Wire-facing encoding needs evidence from outside our encoder (captured bytes or a real client), never a round trip.

## SIMD policy

The workspace is nightly for `portable_simd`, with one implementation and no scalar fallback. Lanes go across independent positions only. Never vectorise an accumulation chain (octave sums have a fixed order; a horizontal-add tree is a different world). No `mul_add` or other FMA in ported numerics. Fix allocation, interning and copying before SIMD.

## Performance goal

Steady-state serial cost `C_ss`: median `column(cx, cz)` over the 100 interior chunks of a 12x12 sweep, one thread, release, seed 42, each stage run exactly once per chunk (counter-asserted). Target `C_ss` at most 1.0 ms and cold-region cost at most 8 ms. It is a goal, not a gate: a miss is a recorded number plus a named next lever, never a reason to weaken parity. Draw count is spec-bound, so headroom is in vegetation predicate cost. Acceptance is counters and gates, not a bare duration; timings taken while other agents build are samples.

## Parallelism and allocation

- **Wavefront.** Fill, surface and carve depend only on the seed; ore reads a 3x3 of pre-ore; vegetation reads a 3x3 of post-ore; top-layer depends on its own vegetation.
- **One writer per chunk.** Decoration folds nine sources into the centre chunk's grid and drops out-of-bounds writes, so neighbours are read-only `Arc` snapshots and there is no cross-chunk write lock. Gate: `parallel_generation_is_deterministic_and_matches_serial`.
- The store map is sharded by chunk hash with locks held only for an `Arc` lookup or insert. Steady-state serving of a column makes no hot-path allocations beyond the returned buffers; scratch comes from per-thread pools.

## Traps

- A bench whose resolver supplies no data measures a pipeline with stages missing; assert by counter that stages ran.
- Palette and iteration order must be deterministic at the serve boundary; a clamped-key cache once aliased two chunks (use exact keys, view-scoped eviction); an absolute-versus-local coordinate bug once gave zero vegetation with green unit tests (keep a boundary-write control and production-seam gates).
- Sampling heights are per-consumer by design (carvers and ore at y=0, vegetation at the surface); do not unify.
- Biome search must equal brute force, tie-breaks included. Depth-first vegetation recursion must not be reordered, and draw-count-changing swaps (trapezoid versus uniform ints) desync the stream.

## Reference provenance

The community Rust server Pumpkin (GPL-3.0) was read at commit `caf954d17043e1f618f4afe254cbcd479492d80b` for engineering shapes only (flattened density component stack, eagerly filled caches with batched fill, thread-local scratch pools, flat numeric chunk storage, a staged dependency-graph scheduler, a biome tree in the game data's node order). No source is reproduced. Its placement, coral, tree-decorator and surface-rule behaviour diverges from the reference, so it is never an oracle; everything from surface rules outward is checked against our JVM oracles only.

## Open items

- Savanna vegetation oracle residuals (11/185 and 1/116) have no known mechanism; new work must add none.
- The set of still-skipped decoration steps should be derived from the jar's registry loading.

## Dependencies

JVM oracles under `oracle-java/` ([oracles-and-benchmarks](../oracles-and-benchmarks.md)); `lodestone-worldgen-core` (noise, RNG, hash, kernels) and `lodestone-worldgen` (composition).
