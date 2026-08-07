# Worldgen density engine: the block field, and the interpolation order it depends on

## What it is

How `crates/lodestone-worldgen-core`'s density stack turns vanilla's data-driven
density-function graph into the per-block field that `fillFromNoise` writes, and
the one bit-significant decision inside it that is easy to get wrong in the
direction that looks *more* faithful: which of vanilla's **two** trilinear
interpolation orders the block field actually uses. Written while executing U4 of
[`plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md); it records a measured
correction to that plan's U4 row plus the engine shape the correction implies.

## The two layers

Two evaluators exist, and the difference between them is the whole subject:

| evaluator | vanilla analogue | entry point |
|---|---|---|
| point interpreter | `DensityFunction.compute(SinglePointContext)` | `Density::compute(Context)` (`density/mod.rs`) |
| block field | `NoiseChunk.getInterpolatedDensity()` | `NoiseChunkSampler::final_density(x, y, z)` (`density/chunk.rs`) |

The router parity suites (`density_parity`, `region_parity`) prove the **point**
interpreter. `chunk_parity` proves the **field**: 16×16×384 = 98,304 blocks of
chunk (0,0) at seed 42, bit-for-bit against `DensityChunkOracle.java` driving the
real 26.2 `NoiseChunk`. Both currently read 100%.

Only three node kinds behave differently between the two layers:

- `interpolated` — transparent at a point; 4×8×4 corner sampling + trilinear
  interpolation in the field.
- `flat_cache` — transparent at a point; XZ snapped to the quart grid and
  `y` forced to `0` in the field.
- `cache_2d` — a real last-`(x, z)` memo at a point; transparent in the field.

`cache_once`, `cache_all_in_cell` and `blend_density` are value-transparent in
both. `spline`, `old_blended_noise` and `find_top_surface` are **leaves** to the
field evaluator: it does not recurse into them, it calls the point interpreter,
so everything beneath one of those is evaluated with point semantics (no quart
snapping, no interpolation). That is a real semantic, not an optimisation, and
any flattening has to reproduce it.

## The interpolation order — the finding

`NoiseChunk.NoiseInterpolator` has two value paths over the same eight corners:

| vanilla path | expression | nesting |
|---|---|---|
| `fillingCell == true` | `Mth.lerp3` — `lerp2` is `lerp(dy, lerp(dx, x00, x10), lerp(dx, x01, x11))` | **X inner**, then Y, then Z |
| `fillingCell == false` | incremental `updateForY` → `updateForX` → `updateForZ` | **Y inner**, then X, then Z |

Bilinear interpolation is order-independent algebraically and **is not**
order-independent in IEEE 754, so these are two different worlds.

Reading `NoiseChunk`'s driver loop suggests the second: `selectCellYZ` →
`updateForY` → `updateForX` → `updateForZ` → read, which is literally what
`DensityChunkOracle.java` calls. The plan's U4 row says "vanilla's incremental
cell walk (`advanceCellX`/`updateForY`)" for the same reason.

**The block field uses the first one.** The resolution is two levels removed
from the interpolator — `NoiseChunk`'s constructor (`NoiseChunk.java:157-160`)
never reads the router's `final_density` directly:

```java
DensityFunction fullNoiseValue = DensityFunctions.cacheAllInCell(
        DensityFunctions.add(wrappedRouter.finalDensity(), BeardifierMarker.INSTANCE))
    .mapAll(this::wrap);
this.fullNoiseDensity = fullNoiseValue;
```

That `cache_all_in_cell` is applied **in code, not in data**. `grep` finds no
`minecraft:cache_all_in_cell` anywhere in 26.2's worldgen JSON, so a census of
the `noise_settings` documents — the obvious way to enumerate which markers the
engine must implement — cannot see it at all. Its cell array is pre-filled inside
`selectCellYZ` (`NoiseChunk.java:295-311`), which brackets the fill with
`fillingCell = true` / `fillingCell = false`. So every value
`getInterpolatedDensity()` ever returns for `final_density` was produced while
`fillingCell == true`, i.e. by `Mth.lerp3`. The incremental chain is machinery
the loop maintains and `final_density` never reads.

### Measured, because a 1-ULP error does not look like an error

Two measurements, both in the tree:

- Swapping `density/chunk.rs`'s `lerp3` helper to the incremental chain takes
  `chunk_parity` from **98304/98304** to **90563/98304** — 7,741 diverged
  blocks, every one a last-place difference. Nothing else fails; the terrain
  still looks like terrain.
- The two nestings are bit-distinguishable on real router data at a rate that
  makes the coincidence explanation untenable: **60,300 of 393,216** blocks
  across four chunk/seed cases, worst absolute difference `1.78e-15`.

`crates/lodestone-worldgen/tests/interpolation_order.rs` is the standing guard.
It harvests the real corner lattice through the public sampler — at a cell corner
every lerp factor is exactly `0.0` and `lerp(0.0, a, b) == a` exactly, so
`final_density` at a corner *is* the corner value, needing no private access and
no second implementation of corner evaluation to get wrong — then recomputes the
field both ways.

Three things about that test are worth copying rather than just reading:

- **Its control fired, on its first run.** The first version rooted the sampler
  at `noise_router.final_density` and reported 178,815 / 393,216 blocks
  unexplained. `final_density` is `min(squeeze(interpolated(...)), ...)`: the
  marker is nested two levels down and the enclosing `squeeze`/`min` vary *within*
  a cell, so the corner-harvest premise only holds for a root that **is** the
  marker. Without the control that would have been a confidently wrong result.
- **Its guard is inverted.** It asserts the orders *differ*, and fails loudly if
  they ever stop differing, because at that point it can no longer distinguish a
  correct port from a wrong one and the reader needs to be told rather than
  inherit a vacuous pass.
- **Its magnitude bound is absolute, not in ULPs.** An interpolated density
  passes through zero, and two values straddling zero sit thousands of ULPs apart
  while being `1e-15` apart. A ULP bound reports 2048 for the *healthy* case. The
  thing worth catching is one of the two helpers being miswritten, which would
  differ by O(1).

## What this means for the engine rewrite (U4)

The correction is not "keep the point-query sampler". Vanilla really does hoist
the corner work; it just hoists it with `Mth.lerp3`. So:

> The correct cell walk **pre-fills a 4 × 8 × 4 = 128-value cell array using
> `Mth.lerp3` from eight corner values held once per cell** — exactly what
> `CacheAllInCell` does. That is the *same arithmetic* the current per-block path
> already performs, hoisted so the eight corner reads become register loads per
> cell instead of hash-or-array probes per block. A walk built on
> `updateForY/X/Z` is a different world, and 7,741 blocks per chunk is the price.

The available win is therefore real and is a *lookup* win, not an arithmetic one.
Counter shapes, derived from the geometry (`cell_width = 4`, `cell_height = 8`,
16×16×384 chunk, so `cellCountXZ = 4`, `cellCountY = 48`):

| quantity | value | derivation |
|---|---|---|
| corner lattice per interpolated slot per chunk | **1,225** | 5 × 49 × 5 = (4+1)² × (48+1) |
| corner *lookups* today | **786,432** | 98,304 blocks × 8 |
| `block_at` calls per chunk | **98,304** | 16 × 16 × 384 |

1,225 agrees with the plan's figure, and also with vanilla's own slice
accounting: `fillSlice` fills (cellCountXZ+1) × (cellCountY+1) = 5 × 49 = 245 per
X-plane, over `initializeForFirstCellX` plus four `advanceCellX` calls = 5
planes. The win the hoist deletes is `786,432 → 1,225` evaluations plus the
elimination of every corner *lookup*; it does not change the number of
multiply-adds in the lerp itself.

### Semantics a flattened graph must preserve

Each of these is a place a flattening can silently differ, and each needs its own
per-wrapper fixture rather than only an end-to-end gate:

1. **`Mul`'s `v1 == 0.0` short-circuit.** The second operand is not evaluated.
   This forbids a bottom-up topological sweep that evaluates every node — a
   flattened graph must still be walked by recursive descent over indices.
2. **`interpolated`-inside-corner transparency.** While evaluating a corner, a
   nested `interpolated` is transparent (`interpolate = false`), so the flag has
   to thread through the descent.
3. **`flat_cache`'s quart snap and forced `y = 0`.** Keyed on `(qx, 0, qz)`; the
   inner is evaluated with `interpolate = false`.
4. **`cache_2d` / `cache_once` scoping.** `cache_2d` caches in the point
   interpreter and is transparent in the field; `cache_once` and
   `cache_all_in_cell` are transparent in *both* of our evaluators, and the
   `cache_all_in_cell` above `final_density` is not in the data at all (above) —
   its effect is already baked into the choice of `Mth.lerp3`.

And the float-order rule: no `mul_add` or any FMA-introducing operation anywhere
in ported numerics, and no reassociation of an accumulation chain (octave sums in
Perlin/blended noise have a fixed vanilla evaluation order). Batching across
independent lattice positions is safe by construction; batching across an
accumulation chain is not.

## How to change it

- **Do not "fix" `lerp3` to the incremental chain.** Read
  `## Which interpolation order` in `density/chunk.rs` first;
  `interpolation_order.rs` will fail, and so will `chunk_parity`, but the second
  one fails at 92% which reads like a tolerance problem rather than a wrong
  algorithm.
- **Do not derive the marker set from the `noise_settings` JSON alone.** One
  load-bearing marker is applied in code. Census the data *and* read
  `NoiseChunk`'s constructor.
- Changing the corner cache's *storage* (hash vs dense array) is value-invariant
  and needs no new fixture — `chunk_parity` already runs both, via
  `NoiseChunkSampler::new` and `new_bounded`. Changing the corner *key shape*,
  the traversal's `interpolate` flag, or which node kinds the field evaluator
  recurses into is not value-invariant and does.
- `new_bounded`'s contract is unchecked in release: every query must fall inside
  the declared inclusive bounds or it silently aliases another cell. `erosion`
  and `depth` deliberately stay on the unbounded `new` because
  `is_deep_dark_region` queries them outside the chunk.

## Configuration

None. No feature flag or env var selects any of this. The `gen-counters` feature
turns the `corner_lookups` / `density_evals` / `slot_hit`/`slot_miss` counters
from inert to live; it must be forwarded as
`gen-counters = ["lodestone-worldgen-core/gen-counters"]` from
`lodestone-worldgen` or every counter silently reads zero
(`tests/gen_counters_forward.rs` is that gate).

## Dependencies

`density/chunk.rs` depends only on `density/mod.rs` (the point interpreter and
the `Density` graph) and `counters`. Its consumers are
`crates/lodestone-worldgen/src/aquifer/mod.rs` (three samplers: `final_density`
bounded, `erosion` and `depth` unbounded) and `tests/chunk_parity.rs`. Evidence
comes from `scripts/worldgen-oracle/DensityChunkOracle.java` and the decompiled
`NoiseChunk.java` under `.cache/mc/26.2/src`.
