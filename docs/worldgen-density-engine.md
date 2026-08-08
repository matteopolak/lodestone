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
- `cache_2d` — **transparent in both**, since §12.132. It was a real last-`(x, z)`
  memo at a point until that section measured its hit rate at 0.12%; see
  *`cache_2d` was a memo and is not any more* below.

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

## The flattened engine (U4), as built

`crates/lodestone-worldgen-core/src/engine/` is the engine the correction below
implies. Three files, split by mutability, and the split *is* the design:

| file | holds | lifetime |
|---|---|---|
| `graph.rs` | `Program` — the `Op` table plus side tables | immutable, `Sync`, `Arc`-shared |
| `scratch.rs` | `Scratch` — the corner and cell caches | per-chunk, per-thread, pooled |
| `field.rs` | the `NoiseChunk`-semantics evaluator | borrows both |

`density/chunk.rs`'s `NoiseChunkSampler` is now a façade that pairs a `Program`
with a `Scratch`; its public API is unchanged, so `aquifer/`, `chunk_parity` and
`interpolation_order` needed no edits across the cutover. The recursive walk over
`Box`-linked `Density` nodes it used to contain is deleted.

Because **no** cache lives in the graph, one graph backs concurrent chunk
generation on any number of threads with no lock and no copy. That is what makes
a `Program` clone a refcount bump, which is diagnostic D3's per-chunk deep clone.

### Why flatten at all: the node is 14× too wide

`Density` inlines a `BlendedNoise` in one variant — three `PerlinNoise` stacks,
each two `Vec`s plus two `f64`s — so **every** node, including a bare
`Const(f64)`, occupies that width, and every child is a separate heap allocation
of it. Measured: **`size_of::<Density>()` = 232 bytes** against
**`size_of::<Op>()` = 16**, a 14× ratio, with the wide payloads moved to side
tables. `graph::tests::density_node_is_much_wider_than_an_op` re-measures and
prints both on every run and fails below 8×, so the figure quoted here has a
source rather than being a number someone once got by adding up struct fields.

### The route the plan expected was not needed

Issue #490 records that a compiled-graph cache had "nowhere to live" without
either patching `overworld/mod.rs` or making `Builder::build`'s returned
`Density` carry the compiled form, and judged the second viable. **Neither was
needed.** Two of that analysis's premises had moved: `AquiferTrees` is in
`overworld/fill.rs`, not `overworld/mod.rs`, and `overworld/mod.rs` came into
U4's ownership. So the compiled form is held directly and `Builder::build` still
returns a plain `Density` — which matters, because `biome/mod.rs` (U9's file)
consumes `Builder::build` and a changed return type would have reached across an
ownership boundary for no benefit.

All 11 `Density::` construction sites outside `density/` are
`Density::YClampedGradient`, which has no boxed children, and `Spline` has zero
external users — so the blast radius the issue measured was real, it just did not
have to be spent.

### D3: the eight per-chunk clones, deleted and measured

`build_aquifer` runs once per chunk and used to `.clone()` eight `Density` trees.
`AquiferTrees` now holds the three sampler routes as `Program` and the five
point-evaluated routes as `Arc<Density>`, so every one of those clones is a
refcount bump.

Measured, both arms in one process with a thread-local counting allocator
(`tests/engine_clone_allocs.rs`, its own binary because a `#[global_allocator]` is
per-binary):

| arm | allocations per chunk |
|---|---|
| deep-cloning the eight trees (pre-U4) | **19,356** |
| cloning the compiled/shared form | **exactly 0** |

The control is measured *first* and required to exceed 1,000, so the zero cannot
be explained by a dead instrument. The sinks are pre-allocated to capacity outside
the measurement window and the clone targets are fixed-size **arrays**, not
`Vec`s — the first version used `Vec`s and had to permit 16 allocations of
container overhead, which is exactly the kind of allowance that later gets
widened instead of removed.

One consequence worth knowing before sharing a graph more widely: see
*`cache_2d` was a memo and is not any more* below. The slot is gone entirely, so
there is nothing left in a shared `Program` for two threads to contend on.

### What it cost and what it bought, measured

Old-vs-new equality, the U6 bar: **786,432 blocks over 8 chunks at 4 seeds**
(42, 1234, 987654321, −99, two chunks each including negative and far-from-origin
coordinates) dumped from two worktrees whose harness was md5-verified identical,
`cmp`-clean and md5-identical (`3366dd68c4a7ddc4c8923abe47889c03`), 0 differing
lines of 786,432. Detector control: a single flipped hex digit 15 MB into one file
is reported by both `cmp` (`char 15000033, line 469116`) and the line counter
(exactly 1). Plus `chunk_parity` bit-exact against the JVM dump on both the hashed
and dense paths.

**No speedup is claimed.** The two arms are different builds of the same symbol,
so they cannot be interleaved in one process, and this repo's two-arm rule exists
precisely because non-interleaved worldgen timings have been attributed to the
wrong cause before. For context only, explicitly not a claim: the density-field
sweep above took 1.31 s on the old engine and 0.35 s on the new one, in separate
processes, on a loaded machine.

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
| cells per chunk per interpolated slot | **768** | 4 × 48 × 4 |
| corner *lookups* per slot, pre-hoist | **786,432** | 98,304 blocks × 8 |
| corner *lookups* per slot, post-hoist | **6,144** | 768 cells × 8 |
| `block_at` calls per chunk | **98,304** | 16 × 16 × 384 |

1,225 agrees with the plan's figure, and also with vanilla's own slice
accounting: `fillSlice` fills (cellCountXZ+1) × (cellCountY+1) = 5 × 49 = 245 per
X-plane, over `initializeForFirstCellX` plus four `advanceCellX` calls = 5
planes. The hoist deletes corner *lookups*; it does not change the number of
corner *evaluations* and does not change the number of multiply-adds in the lerp.

### Measured, and the premise that was wrong first

`tests/engine_counters.rs` is the gate, in its own binary because the counters are
process-global. Its first version asserted the overworld `final_density` contained
**one** `interpolated` node and derived `768 × 8 = 6,144` from that. **It contains
five**, and only **two** are entered per block — that premise check fired on its
first run. Without it the file would have asserted a wrong literal and then been
"fixed" by relaxing it.

So the per-slot figures above are the per-slot figures; the whole-router numbers,
measured on chunk (0,0) at seed 42, are:

| quantity | measured | derivation |
|---|---|---|
| `interpolated` evaluations | 196,608 | 2 nodes entered × 98,304 blocks |
| cell fills | 1,536 | 2 × 768 |
| corner lookups | **12,288** | 8 × 1,536 |
| corner lookups, no-hoist hypothesis | 1,572,864 | 8 × 196,608 |
| corner evaluations | **2,450** | 2 × 1,225, unchanged by the hoist |

The two lookup hypotheses sit exactly 128× apart — the blocks in a `4 × 8 × 4`
cell — so the gate is a prediction rather than a tolerance. Two failure modes get
named diagnoses in its message: measuring 1,572,864 means the cell cache is not
consulted, and measuring 196,608 means a *single* last-cell memo rather than a
per-cell cache, which the fill loop's Y-innermost order evicts 12,288 times per
node per chunk.

Three of the five `interpolated` nodes are never entered. That is `mul`
short-circuiting and `range_choice` branching, **not** transparency: the
structural walk `Program::interpolating_slots` — which applies the same
transparency rule the evaluator does — finds all five reachable in an
interpolating context.

### Two cache layers, and why neither can go

| layer | keyed by | population per chunk | deletes |
|---|---|---|---|
| cell | cell triple, per `interpolated` slot | 768 octets | the *lookup* |
| slot | corner block position, per slot | 1,225 values | the *evaluation* |

Dropping the cell layer restores 8 lookups per block. Dropping the slot layer
leaves the winning lookup count while **quintupling** evaluations, because
adjacent cells share corners (768 × 8 = 6,144 fetches over a lattice of 1,225).
The counters are one per layer for exactly this reason, and the gate asserts both.

### The pooled scratch's one hazard

A recycled `Scratch` keeps its `values` buffers, so `reconfigure` clearing every
presence flag is the only thing standing between the pool and a stale value from
the previous chunk — a failure that is silent, position-dependent, and produces
plausible terrain. `scratch::tests::reuse_clears_presence_flags` and
`pool_round_trip_is_clean` are the gates, each with a control proving a store
would have been visible.

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
4. **`cache_2d` / `cache_once` scoping.** All of `cache_2d`, `cache_once` and
   `cache_all_in_cell` are transparent in *both* of our evaluators (`cache_2d`
   since §12.132 — it used to memoise at a point), and the
   `cache_all_in_cell` above `final_density` is not in the data at all (above) —
   its effect is already baked into the choice of `Mth.lerp3`.
5. **`cache_all_in_cell` selecting the interpolation order.** Value-transparent,
   but it is what picks `Mth.lerp3` over the incremental chain. This is the one
   that actually bit.

`crates/lodestone-worldgen/tests/engine_semantics.rs` is the per-wrapper suite,
one test per rule, each carrying a control that shows the assertion could have
failed. Three things in it are worth knowing before writing another such test:

- **At `cell_width = 4`, semantics 2 and 5 are value-unobservable.** `flat_cache`
  snaps XZ to the **quart** grid — a hardcoded `>> 2 << 2`, *not*
  `cell_width` — so when `cell_width` is also 4, every quart-snapped position and
  every corner position is a cell corner. At a cell corner all three lerp factors
  are exactly `0.0` and `lerp(0.0, a, b) == a` exactly, which makes a nested
  `interpolated` the identity whether or not it is transparent, and makes the
  X-inner and Y-inner nestings agree bit-for-bit. The suite therefore uses
  `cell_width = 8` for those two, which puts the quart grid mid-cell. **A version
  of these tests written at `cell_width = 4` passes while measuring nothing**, and
  `chunk_parity` — which is the overworld, at 4 — says nothing about either rule.
- **The real router does not exercise semantic 2 at all.** None of the five
  `interpolated` nodes in the compiled `final_density` is nested inside another.
- **A fixture of `const`/`y_clamped_gradient` cannot expose an interpolation-order
  difference**, because a function of `y` alone has x/z-invariant corners and every
  nesting then agrees exactly. The suite instantiates a real `NormalNoise` through
  an in-memory `Resolver` so its fixtures vary in all three axes, and the
  interpolation-order test asserts that at least one sampled position is
  bit-distinguishable between the two nestings (currently 2 of 6) rather than
  assuming it.

Semantic 1's fixture is worth copying: `mul(0.0, invert(0.0))` is `0.0` with the
short-circuit and **`NaN`** without it, because `1/0` is infinity and `0.0 * inf`
is `NaN`. A value difference, so it holds with the counters compiled out; and the
control asserts `mul(1.0, invert(0.0))` really is infinite, without which the test
would pass just as happily against a harmless second operand.

And the float-order rule: no `mul_add` or any FMA-introducing operation anywhere
in ported numerics, and no reassociation of an accumulation chain (octave sums in
Perlin/blended noise have a fixed vanilla evaluation order). Batching across
independent lattice positions is safe by construction; batching across an
accumulation chain is not.

## The point interpreter's `(node, x, z)` memo — vanilla's `FlatCache`, finally

The two cache layers above belong to the **block-field** evaluator. The **point**
interpreter (`Density::compute`, which is what every leaf — `spline`,
`old_blended_noise`, `find_top_surface`, `end_islands` — is evaluated by) had no
memo of any kind, and that is where the 4.87× noise redundancy §12.134 measured
turned out to live. `density/xz_memo.rs` is the fix, and three things about it are
worth reading before touching it.

**Three memo shapes were measured at once, and only one of them works here.**
`tests/density_redundancy_probe.rs` counts, per node kind, how many visits each of
three hypothetical memos would have answered, over the 100 interior columns of the
12×12 sweep. In the point interpreter:

| memo | hit rate |
|---|---|
| one slot per node, last `(x, z)` — vanilla's `Cache2D` | **2.1%** |
| full `(node, x, z)` map | **78.2%** |
| full `(node, x, y, z)` map | 0.9% |

So §12.132's decision to delete a one-slot `cache_2d` memo at a 0.12% hit rate was
a correct measurement **of the wrong structure**. The reason the one-slot form
cannot work is the corner fetch order: `Field::interpolate` fills a cell by
fetching `(x0,z0) (x1,z0) (x0,z0) (x1,z0) (x0,z1) (x1,z1) (x0,z1) (x1,z1)`, so
consecutive visits to one node alternate between four `(x, z)` pairs and a single
slot is evicted before it is ever read. **Vanilla does not have this problem
because vanilla's `FlatCache` is a chunk-wide `double[]` over the quart grid, not
a slot** — the one-slot structure is `Cache2D`'s, and vanilla reaches these
subtrees through `FlatCache`. The map is the faithful shape, not an invention.

**Value invariance is structural, not a property of 26.2's data.** A
`flat_cache`/`cache_2d` node carries a memo id only if `Density::is_xz_pure` proves
its *whole subtree* cannot read `ctx.y`. Three arms of that analysis are places a
plausible simplification is wrong: `shift_a`/`shift_b` pass a literal `0.0` where
`y` would go and are pure while `shift` is not; `shifted_noise` qualifies only with
`y_scale == 0.0` **and** a constant `shift_y` that is not `-0.0`, because
`f64::from(y) * 0.0` is `-0.0` for negative `y` and `-0.0 + -0.0` is `-0.0` while
`+0.0 + -0.0` is `+0.0`; and plain `noise` is excluded even at `y_scale == 0.0`
because nothing absorbs that `±0.0` and the answer would depend on
`ImprovedNoise`'s internals rather than on IEEE addition. Anything unproved is
simply not memoised, so a datapack cannot defeat it.
`engine_semantics.rs`'s `cache_2d_is_transparent_in_both_evaluators` — whose
fixture is *deliberately* `y`-dependent — is the negative control that this
analysis rejects what it should: that gate passes unchanged.

**Ids come from a process-wide monotonic counter and are never reused**, which is
why the table needs no clearing, no epoch and no lifetime coupling to a `Graph`. A
pointer-keyed version would have had to reason about a tree being dropped and a new
node landing on the same address; there is no cheap way to be sure of that, and the
failure would be a wrong value, not a missed hit.

Measured on the 12×12 sweep, before/after alternated ten times in one shell
invocation: **I_ss 485.08 M → 430.65 M instructions per column, −11.22%**, against
a within-arm spread of 0.14%/0.25%. Point-interpreter node visits per column
**172,888 → 56,877**, and the `NormalNoise` evaluations inside leaves
(`shifted_noise` + `shift_a` + `shift_b`) **40,902 → 8,311**. The memo itself reads
23,619 lookups per column at a **54.7%** hit rate. The 45-column/5-seed U15 dump is
byte-identical across the change (8,902,157 bytes, md5
`c0ef05ac09ba3f90175a14b0f9a69d50`, one-flipped-byte control fires).

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
- **The field walk must stay a recursive descent.** `Mul` does not evaluate its
  second operand when the first is exactly `0.0`, and a skipped subtree can
  contain a cache-slot write, so a bottom-up sweep over the `Op` table would
  change what *later* queries return, not merely the cost. `range_choice`,
  `interval_select` and `interpolated`'s two regimes branch as well, so the set of
  nodes one evaluation touches is position-dependent.
- **Adding a `Density` variant is four edits and only three are compile errors.**
  `graph.rs`'s `compile_node` and `field.rs`'s `eval` are both exhaustive matches
  and will fail to build. But `OpKind`'s discriminant must *also* equal the new
  variant's `Density::kind_index()`, and only
  `graph::tests::op_kind_discriminants_match_density_kind_index` catches that.
  Get it wrong and the node still evaluates — as the wrong operator, with the
  per-kind counter filed under the wrong name. Append to both tables; never insert
  in the middle, because a recorded counter table from an earlier run is indexed by
  these numbers. `graph.rs`'s `walk_interpolating` is a **fifth** place, and it is
  not exhaustive-checked in a useful way: an operator missing a case there silently
  stops contributing its subtree to `interpolating_slots`. The third compile error is
  `Density::write_signature` — see the node-sharing entry below for what to write in
  it, which is the part a compile error cannot tell you.

  `end_islands` is the worked example: `Density::EndIslands(Arc<EndIslandNoise>)`
  appended at index 31, `OpKind::EndIslands = 31` in `graph.rs` *and* an arm in
  `field.rs`, a `compute` arm passing **only** `(x, z)`, a signature arm, and a case
  added to `op_kind_discriminants_match_density_kind_index` — a case left out of that
  list is a case the gate does not cover. `Builder` holds a `OnceCell` for it because
  construction burns 17,292 discarded `nextInt`s plus a 256-step shuffle and the type
  appears twice in 26.2's data (`noise_settings/end.json`'s `erosion`, and
  `end/sloped_cheese.json`); vanilla substitutes one object into both.
- **Do not flatten beneath `spline` / `old_blended_noise` / `find_top_surface` /
  `end_islands`.** They are leaves to the field evaluator by vanilla's own
  semantics, so they hold an untouched `Density` subtree and are evaluated with
  `Density::compute`.
- **Compilation is a node-sharing pass, and a new node kind has to be classified.**
  `Program::compile` hash-conses the `Op` table (`graph.rs`'s `Interner`), so an
  identical subtree is compiled once however many times `Builder`'s reference
  expansion duplicated it. Two rules for anyone adding a kind:
  1. **Add a `Density::write_signature` arm.** The match is exhaustive so this is a
     compile error, but *what* you write matters: floats go in as `to_bits()`, never
     as compared values (`0.0 == -0.0` is true and they are different values under
     `1.0 / x`; `NaN != NaN` would stop a node matching itself), and lengths precede
     their contents.
  2. **Decide whether the kind is a pure function of position.** Everything in
     26.2's data is — `Graph` holds no mutable state and every evaluator entry takes
     `&self` — so today's exclusion list is empty. A kind that advanced an RNG at
     *evaluation* time could not be collapsed, and the arm should then push a
     per-instance unique word instead of a structural one.

  The `slot` of `interpolated`/`flat_cache` is deliberately **excluded** from the
  signature: it is an index into `Scratch`'s memo, not part of the function, so two
  copies collapse onto the first one's slot and the second parent then reads the
  memo instead of re-evaluating. Freed slots are simply unused; `Builder::slot_count`
  still sizes the scratch.
- **`Program::cache_2d_under_leaves` measures duplication, and 708 → 236 is only
  part of the story.** It read **708** on the real overworld `final_density` before
  the sharing pass and reads **236** after — exactly 708/3, because the leaf *table*
  held three copies of each `cache_2d`-bearing subtree. The 236 that remain are
  duplication *inside* one leaf, which an `Op`-level pass structurally cannot reach:
  a leaf is an untouched `Density` subtree interned whole or not at all.
  `preliminary_surface_level` is the extreme — **one** op, **one** leaf, **416**
  `cache_2d` nodes inside it — and `depth` carries another 106.

  **And node sharing is not evaluation sharing.** The field evaluator is a recursive
  descent with no per-node memo, so a node with two parents is still evaluated
  twice. Measured: sharing removed 51% of `final_density`'s nodes and **0.26%** of
  instructions retired per column. The redundancy is real (4.87× per column, §12.134)
  and removing it needs per-position memoisation, which §12.132 closed for `cache_2d`
  on hit-rate grounds — grounds this pass changes, since the reason that memo could
  never hit was the duplication it now removes.

  There is nothing to contend on any more. See the next entry.
- **`cache_2d` was a memo and is not any more, and the reversal is the interesting
  part.** It carried a `Mutex<Option<(i32, i32, f64)>>` single-slot last-value cache
  in the point interpreter from `d68e0a5` until §12.132. Both the decision and its
  reversal were measurements, and the second caught the first going stale:

  | when | evidence |
  |---|---|
  | added | criterion paired comparison, **−4.4%** on `column()`'s median (95% CI −6.0%..−2.7%) — it sat above `find_top_surface`'s per-`y` scan |
  | §12.132 | every lookup's outcome counted over a 289-column burst: **24,843 hits against 19,899,205 misses**, a **0.12%** hit rate, 86 hits per column |

  U4 is what changed underneath it: `find_top_surface` became a *leaf*, and the
  `Scratch` slot layer memoises its result one level up, so the repeat visits this
  cache existed to catch stop reaching it. Removing it was worth **3.5% of serial
  instructions retired** — the memo had become net-negative even single-threaded.

  Two independent arguments make the removal value-invariant, which is why the
  45-column/5-seed dump came out byte-identical (md5 `c0ef05ac…`, 8,902,157 bytes):
  vanilla's own unwrapped `DensityFunctions.Marker.compute` is
  `return this.wrapped.compute(context);` with no memo at all, and every `cache_2d`
  in 26.2's shipped data wraps an xz-only subtree (`shift_a`, `blend_offset`,
  `blend_alpha`, a spline over `continents`), so a `y`-keyed difference cannot arise.
  `engine_semantics.rs`'s `cache_2d_is_transparent_in_both_evaluators` is the gate,
  and it keeps the deliberately `y`-dependent fixture — a real `cache_2d` can never
  show a memo, so only an invalid one can detect it.

  **It was not the parallelism defect**, which is worth recording because it looked
  exactly like one: §12.102 flagged the 708 shared slots as a contention hazard, and
  removing them changed the 289-column burst's cycle ratio from 4.19× to 4.35× —
  i.e. not at all. The window was the defect (§12.132).
- **`xz_memo`'s `LOG2_LEN` is a locality trade — re-measure, do not reason.** The
  table is direct-mapped, so a bigger one conflict-misses less and evicts more of
  everything else. Measured I_ss: 1,024 entries −10.38%, **4,096 −11.22%**, 16,384
  −11.88%. 12 is shipped because the last 0.66% costs 4× the per-worker footprint
  (98 KB → 393 KB) and §12.132 measured per-worker cache footprint, not locking, as
  what caps generation parallelism at 2.6×. Raise it only with the 289-column join
  burst measured, not on the serial number alone. The table is **thread-local with
  no atomics on the hot path**, which is the one thing the deleted `Mutex`-backed
  memo got wrong.
- **Do not add `y` to the memo key.** Measured at 0.9% in this evaluator: it would
  pay the lookup and return nothing.
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

`density/chunk.rs` is now a façade over `engine/` and depends on it plus
`density/mod.rs` (the point interpreter and the `Density` graph it compiles from);
`engine/` depends on `density`, `noise`, `math` and `counters`, and on nothing
outside this crate. Its consumers are
`crates/lodestone-worldgen/src/aquifer/mod.rs` (three samplers: `final_density`
bounded, `erosion` and `depth` unbounded) and `tests/chunk_parity.rs`. Evidence
comes from `scripts/worldgen-oracle/DensityChunkOracle.java` and the decompiled
`NoiseChunk.java` under `.cache/mc/26.2/src`.

One caveat on that oracle, since it is the authority for `chunk_parity`:
`DensityChunkOracle.java` itself calls the **incremental** methods
(`initializeForFirstCellX` / `advanceCellX` / `updateForY` …), because that is
vanilla's driver loop. It is still authoritative, and the reason is the same
`cacheAllInCell` that this document is about — the values it reads back through
`getInterpolatedDensity()` come out of the pre-filled cell array, produced while
`fillingCell == true`, i.e. by `Mth.lerp3`. So the oracle drives the Y-inner API
and reports X-inner values. Reading its *method calls* as evidence for the
nesting is precisely the mistake that made the plan's U4 row wrong; the fixture it
produces is what settles the question, and 98304/98304 against `Mth.lerp3` is that
answer.
