# Compiled point density evaluation

## What it is

`lodestone_worldgen_core::engine::PointProgram` is the compiled evaluator for
density trees that must answer exact block-point queries, including the
preliminary surface scan. It keeps the immutable operation graph shareable while
`PointScratch` supplies a bounded memo for one aquifer request.

## How it works

Compilation emits an index-addressed operation table and interns identical
subtrees, noise values, and opaque spline-like leaves. `find_top_surface` keeps
its upper-bound query first, scans downward in the source cell order, and stops
at the first positive density. Arithmetic branches retain their original
operand order and zero short-circuit. Point-only cache wrappers consult the
request scratch only when the source tree carried a memo slot; a disabled memo
stays disabled.

`PointProgram::compute_batch` processes caller-order contexts in fixed groups of
eight lanes. The operator walk is shared across active lanes, while branch masks
and the downward scan keep each lane's short-circuit and candidate order. Values
are written to a caller-owned slice in the original order, and the same bounded
XZ memo is reused across groups. The production preliminary cache uses this
batch path for its construction-time grid and surface-corner admissions; scalar
misses retain the scalar path for small or irregular requests.

When `gen-counters` is enabled, the redundancy probe separately reports compiled
point scalar and batch visits. Its readiness-candidate count requires the same
graph identity, operation id, and full `(x, y, z)` within one scalar evaluation
or one eight-lane batch; it does not turn cross-request repetition into a
speculative cache hit rate.

`LODESTONE_DENSITY_PROBE_COLUMNS` narrows the ignored density-redundancy probe
from its default 100 contiguous interior targets to `1..=100` targets. Before
measuring, it prepares the two-chunk pre-ore halo while excluding the target
prefixes themselves. The selected `column` calls then form one production
stream: the first is cold, while an earlier closure can prepare a later target.
The probe reports stream totals normalized by submitted targets, not cold-call
averages. Field "own-sampler" counters are scoped to one `Scratch` lifetime,
not to an individual `Field::eval` call; treat them as a per-sampler demand
measure until a separate per-evaluation instrument exists.

When compilation sees `Clamp(Max(A, B), lo, hi)`, it may mark the clamp for a
right-first scalar path only when `A` is cache-free and both finite bounds are
valid. The evaluator then runs `B` first and returns `hi` when `B > hi`; if not,
it evaluates `A` and performs the normal max followed by clamp arithmetic. This
preserves `B`'s cache publication while avoiding `A` only when the clamp result
is already fixed. Tile and point evaluators retain their ordinary order.

Pure X/Z factor-and-offset products can also be admitted to a request-scoped
`XzProductLattice`. `XzProductManifest::from_routes` records the exact source
signatures and a seed-bound fingerprint; `Program::compile_with_xz_products`
and `PointProgram::compile_with_xz_products` only consume matching lattices.
The point producer evaluates each admitted quart-grid context in factor-before-
offset order. A field `flat_cache` hit then returns the stored bit pattern
directly, while a sparse miss or fingerprint mismatch follows the ordinary
recursive path. This is an array cache, not a global memo: its rectangle and
retention belong to one bounded request, so an omitted position remains an
explicit fallback rather than a fabricated default.

The focused lattice control measures the structural effect independently of
wall time: one fresh field query over two pure wrappers visits 5 graph nodes on
the ordinary route and 3 on a matching lattice (both wrappers still dispatch,
but their child walks disappear). It compares output bits at interior and
boundary coordinates and uses a deliberately mismatched fingerprint as a
negative control; the latter retains the 5-visit route. Production callers
must compile both routes with the same manifest before passing the lattice.

On macOS, the ignored release control also brackets 16,384 fresh samples with
`proc_pid_rusage`'s retired-instruction and cycle counters:
```text
cargo test -p lodestone-worldgen-core --lib --release \
  engine::xz_products::tests::field_lattice_instruction_cycle_control -- \
  --ignored --nocapture --test-threads=1
```

One run without `gen-counters` measured 41,849,746 versus 5,642,087
instructions (36,207,659 fewer, 86.518%) and 7,234,915 versus 732,243 cycles
(6,502,672 fewer, 89.879%) for ordinary versus lattice routes.
These are a local instrument sample, not a wall-time promise; the portable
visit-count and bitwise controls remain the acceptance gates.

The field program has two distinct eight-corner paths. Bundled 26.2
final-density graphs compile each cache-free root into a `TilePlan`: child
registers, parameters, noise references, product labels, and branch ranges are
decoded once at graph construction. The masked executor evaluates only selected
lanes and preserves zero-multiply short-circuit and selector order. Cache
writers and opaque terrain boundaries remain on the exact scalar cell fallback.
Each plan register stores its values, readiness mask, and branch selections in a
reusable lane-state record. The current executor uses eight lanes; the bounded
mask and const-generic storage keep a wider fused evaluator possible without
adding another allocation or changing the scalar fallback.
Graph construction records the complete final-density shape and product labels.
A field context performs a constant-time admission check against the shared
immutable product identity; matching product values bypass their child walks,
while sparse positions and mismatched identities use the exact snapped lookup.
This keeps the interpolation cell cache and shared slot lattice authoritative
rather than replacing them with an independent cross-cell cache.

Overworld cell filling uses `final_density_cell_or_positive` for the fast solid-cell proof. A
single field context handles the proof and the exact 128-value fallback, so inconclusive cells do
not rebuild the evaluator entry state. The fallback remains bitwise-identical to
`final_density_cell`; positive cells leave the caller's output buffer untouched.

`NoiseChunkRegionSampler::with_cell_column` scopes horizontal blended-noise operands to
one aligned 4×4 cell column. The graph records the single blended node only when the
stock terrain certificate matcher accepts its shape and octave descriptors. Each of
the four enclosing X/Z vertices is prepared at its first actual blended leaf miss;
slot hits, memo hits, and successful pre-corner certificates prepare nothing. The
fixed operand arrays contain wrapped horizontal fractions, smoothsteps, and permutation
prefixes, not sampled values. Y floors, smear arithmetic, octave accumulation and
blend gates remain in the ordinary evaluator's shared routine, including its existing
eight-gradient SIMD kernel. No Y samples are evaluated ahead of demand.

The cell-column facade borrows the region but acquires scratch separately for each
query. Its operands are discarded at the next cell column. Other graphs and
noncanonical geometry retain the generic route; existing public point and cell
queries are unchanged. The production consumer is `OverworldGenerator::fill_stage_cells`.

The same stock graph has a deep-terrain residual. Its selector is `S = A + B`,
with the exact non-blended summand `A` demanded first and certified `|B| <= 2.001`.
Finite `A` in `[4.345,65536]` proves both the out-of-range arm and an exact `+0.0`
for `clamp(1.5 + (-0.64 * S), +0.0, 0.5)`: the worst selector is `2.344`,
above `2.34375`, and the clamp entrance is at most `-0.00016`. The bounded finite
range keeps rounding far below that gap. Declined values compute `A + B` in the
original order and retain the original selector.

Compilation checks the complete stock terrain shape, constants by bit pattern,
noise descriptors, and every out-arm dependency, including every nested selector
branch. Opaque dependencies and any escape from the designated clamp decline.
The residual substitutes only its matched `+0.0` and clones the two adds, two
minimums, and one maximum on its ancestor spine. All other nodes, noises, and
slots remain shared. Adds with zero are retained, and the existing field walk
evaluates the residual lazily; no sampled output, artificial selector value,
additional slot, or per-Y operand is stored. Top/bottom zero-multiplier demand
gates and all later exact queries remain ordinary evaluator operations.

Only the admitted stock selector receives the internal `DeepTerrainRangeChoice`
tag during compilation. Ordinary selectors, including Nether and End graphs,
perform no deep-plan lookup. The tag retains the original selector children for
slot traversal and maps to the `range_choice` diagnostic bucket. It is explicitly
ineligible for tile execution; its scalar branch shares the ordinary selector
comparison and add continuation.

The canonical 4×8×4 output kernel processes eight consecutive Y lanes at a time.
An empty noodle-selection mask for a group keeps the terrain clamp, cubic squeeze,
and `min(64)` arithmetic, but skips the two ridge and thickness interpolations.
Groups with selected lanes retain their ordinary interpolation and per-lane
selection. The group shortcut consumes already prepared corners; it does not
change graph evaluation or interpolation-slot cache publication. Retain the
explicit `min(64)` for inactive lanes because a squeezed NaN selects that bound.
Native SIMD and the token-generic fallback use the same kernel.

For cells above the aquifer sampling cutoff, an empty structure-adaptation field can also use a
terrain-only nonpositive proof. It checks finite corner bounds with a rounding margin; a successful
proof lets fill emit the global fluid directly without evaluating the remaining density channels or
materializing 128 values. Nonfinite, near-zero, cutoff-crossing, nonempty-adaptation, and unsupported
geometry cases keep the ordinary path. `nonpositive_cell_skips` in the generation counters reports
how often this branch ran.

The pre-corner certificate recognizes the stock terrain slides and a selector
whose in-range minimum contains the **same canonical sloped node** as its selector. It evaluates
only the exact non-blended summand at the eight enclosing vertices. Values in `[-65536,-2.01]`,
a certified blended bound of `2.001`, and a finite entrance lower bound of `-32` prove negative
terrain for aligned cells starting between Y=-40 and Y=248. Both selector limits, operand order,
stock noise parameters, and finite noise descriptors are checked. Unknown shapes decline the
certificate. The upper slide and interpolation retain a negative gap much larger than roundoff.

`NoiseChunkSampler::final_density_cell_terrain_is_nonpositive` tries this certificate before the
exact-corner proof. Fill calls it only after the empty-beard and global-fluid gates. Successful
certificates leave terrain slots and cells absent; only genuinely evaluated flat/leaf values may
be published. Later density users compute missing terrain corners through the ordinary exact
path. The certificate adds no coordinate cache or retained sampler and has no runtime switch.

`NoiseChunkRegionSampler` keeps separate baseline and candidate scratch state only when the shadow
is enabled. The baseline calls the raw field evaluator's exact-corner proof, independently of the
sampler's production shortcut. The candidate skips
certified cells, while the baseline supplies all 128 exact densities for sign and resolved-block
comparison. Refused cells run the existing proof/fallback in both diagnostic samplers and compare
exact fallback bits. Exact flat-cache publications are reusable; no bound enters a density cache.

At region drop, `pre_corner_shadow` prints certified cells and the set difference of actual
blended cache misses below Y=256 over the complete traversal. Corners needed by a later mixed cell
are therefore not counted as avoided. It also reports added points and total executions, so cache
recomputation is visible. These diagnostic sets and two additional samplers die with the region.
The shadow adds substantial work and its instruction/counter totals are not a performance sample.

The focused bundled control compares every output bit with fresh scalar point
queries. With `gen-counters`, the same 4×2-cell sample reports 9,276 scalar
field visits versus 3,132 compiled-plan visits. The product-admitted route
reports 9,276 baseline visits versus 3,006 optimized visits, 72 product hits,
and the same digest. A Darwin release run measured 2,326,192 versus 1,313,302
retired instructions and 553,408 versus 323,339 cycles for the plain tile
route; the product-admitted route measured 1,664,727 versus 764,517
instructions and 633,066 versus 313,704 cycles. These are local instrument
samples, not a wall-time promise; the portable visit-count and bitwise controls
remain the acceptance gates:
```text
cargo test -p lodestone-worldgen-core --test field_production --release \
  bundled_final_density_tile_instruction_cycle_control -- --ignored --nocapture
cargo test -p lodestone-worldgen-core --test field_production --release \
  bundled_product_density_product_instruction_cycle_control -- --ignored --nocapture
```
A mismatched or sparse lattice is an exact scalar fallback.

Scalar field descent specializes its interpolation mode at compile time. Column
queries use the interpolating instance, while corner and point queries use the
non-interpolating instance; recursive calls no longer carry a runtime mode
branch through every operator.

When a column's root is `interpolated` or `squeeze(interpolated(...))`, the field
walks contiguous Y slices of each interpolation cell directly. A missing slice
uses the existing cell-column interpolation and publishes its full vertical
cache before copying the requested range; a cached slice only copies that range.
The squeeze runs on the copied output, after interpolation, with the scalar
clamp and cubic arithmetic in groups of eight SIMD lanes plus a scalar tail.
The cached interpolation values remain unsqueezed. Other roots keep the ordinary
per-Y evaluator, and shared corner demand and cache publication stay with the
existing cell-column path.

`compose::fill_column`, consumed by Nether and End filling, calls the aquifer's
column API once per X/Z column, then resolves each material in `(lz, lx, ly)`
order. One temporary density buffer holds `height` values and is reused across
all 256 columns; it is released with the fill request. The density-first beard
addition remains explicit, including `density + 0.0` for an empty beard. That
addition can change the sign of zero and matches `AquiferSystem::block_at_beard`.

For an aligned 128-block-high fill, the traversal requires 256 field contexts
instead of 32,768. Reusing four X interpolations per cell-column reduces cell
queries from 32,768 to 4,096 for 4×8 geometry and to 8,192 for 8×4 geometry;
the corresponding X interpolation counts are 16,384 and 32,768 instead of
131,072. These are derived traversal counts, not benchmark measurements.
Shared cell-corner fills and distinct corner evaluations remain unchanged.

The aquifer route control uses the same Darwin process counters to compare a
16-level recursive point tree with its reusable compiled program:
```text
cargo test -p lodestone-worldgen --lib --release \
  aquifer::tests::compiled_aquifer_route_instruction_cycle_control -- \
  --ignored --nocapture --test-threads=1
```
It prints both instruction and cycle deltas plus the output digest. The
portable aquifer unit control separately covers all four bundled route slots,
program reuse, exact output bits, and an opaque-leaf fallback.

## How to change it

Extend `PointGraph::compile_node`, `PointProgram::eval`, and
`PointProgram::eval_batch` together for a new density variant. Keep the
exhaustive kind mapping aligned with `Density::kind_index`, and add a
scalar-versus-compiled bit comparison that uses inputs where branch and scan
choices differ. Do not flatten below opaque spline-like leaves unless their
point semantics and side effects have first been made explicit. A cache change
must retain the source memo-presence decision and must have a bounded collision
control; collisions may recompute but never alter an answer. Keep the batch
width fixed unless its scratch budget and parity controls change with it.

When extending the product lattice, keep `XzProductManifest::kind_for` strict:
only a proven pure-XZ subtree wrapped in both source routes may be labelled.
Preserve the wrapper's snapped `(x, 0, z)` context and return an exact fallback
for a missing entry or fingerprint mismatch. Add a bitwise parity case plus a
counter-enabled visit-count control for every new product kind; a hit count by
itself is not evidence that the evaluator actually bypassed the child walk.

For a field operator, update `Graph::compute_tile_eligibility`,
`Graph::compile_tile_plan`, and the matching arm of
`eval_tile_plan_node` together. Keep the scalar evaluator as the authority for
unsupported nodes and preserve operand order, branch masks, and special
floating-point cases. Product labels must be derived from the complete
final-density shape plus a matching manifest; cache and opaque-leaf boundaries
remain explicit scalar fallbacks. Tile-plan registers are request-local and
reset at each cell; they are not a cross-column cache.
Add a real bundled graph digest and a synthetic negative control whenever a new
node becomes eligible.

Keep prepared blended operands limited to the stock matcher and four enclosing
vertices. Change `PreparedImprovedXZ`, `BlendedInputs`, and the cell-column facade
together if horizontal arithmetic changes. Do not combine scale operations or
replace zero-gradient multiplies with constants: bitwise rounding and signed zero
are part of the contract. The focused prepared-vertical controls compare independent
scalar arithmetic, execute a wrong-X/Z failure control, and compare stock density
bits, demanded coordinates, and ordered slot publications through proof/fallback
and warm-neighbor traversals. This is work elimination, not a second output cache.

Keep `Graph::compile_deep_terrain_residual` and its all-branch dependency walk
together when changing terrain data. The five-node residual spine is deliberately
narrow; reject new wrapper/context boundaries rather than cloning cache slots.
The `deep_saturation_` controls check the independent threshold arithmetic,
stock clone boundary, every nested selector dependency, exact output bits,
remaining kernel calls, demanded coordinates, ordered slot publications, and
later exact selector queries. The forced-invalid control executes the residual
with a non-blended input of `-4`; the ordinary path must produce `-11/24`, and
a real differing lane must trigger the detector. Run these controls before the
bounded production shadow and matched measurement. No performance gain follows
from merely counting admitted evaluations.

The noodle output shortcut relies on canonical groups starting at multiples of
eight: each group fits entirely within one word of the 128-lane selection mask.
If the lane layout or canonical geometry changes, update mask extraction and
the inactive/all-active/mixed arithmetic controls together. Noncanonical geometry
continues to use the scalar output path.

Keep column admission limited to the direct interpolation root and its one
squeeze wrapper. Moving squeeze into corner evaluation changes the resulting
field at fractional positions. Column controls cover both 4×8 and 8×4 geometry,
negative coordinates, partial cells, ordered slot publications, special floating
point values, and unsupported-root fallback. The fractional negative control
deliberately interpolates squeezed corners and must fail its equality assertion.
Material-fill controls compare against scalar aquifer calls with both empty and
nonempty structure adaptation. Retain the `+ 0.0` in the empty-adaptation arm.

The production handoff is `OverworldGenerator::new` → `AquiferTrees` →
`AquiferSystem::from_parts_with_preliminary_cache_and_point_programs`. The
preliminary route and compound aquifer
routes use `PointProgram`; a single `noise` leaf keeps its direct specialization.
`CompiledAquiferPointRoutes` is built once with the generator's four aquifer
trees and passed to each chunk-bound aquifer, so route compilation and graph
allocation do not recur per chunk. A route containing an opaque legacy leaf is
left on the exact recursive evaluator instead of wrapping a recursive leaf in
an otherwise compiled graph.
For route documents that advertise `/factor` and `/offset` references, the
generator builds those two trees once, admits one `XzProductManifest`, and
compiles both final and preliminary programs against that same fingerprint.
Keep immutable graphs in generator-owned trees and scratch in the per-chunk
aquifer. The focused aquifer controls prove both that this handoff is selected
and that a raw recursive control remains bit-identical.

## Configuration

With `gen-counters`, `LODESTONE_DEEP_SATURATION_SHADOW=1` takes precedence over
the pre-corner shadow for a supported stock 4×8 region. Both independent
diagnostic samplers keep identical certificate and fill-demand gates; only the
baseline disables the residual. Exact fallback values and classifications are
compared during the real fill traversal. Successful pre-corner cells are not
queried a second time, which would pollute a full-traversal demand census.
At drop, `deep_saturation_shadow` prints avoided/added actual blended miss
coordinates below Y=256 and baseline/candidate total executions. These are
whole-traversal differences, not a sum of nested stage counters. The diagnostic
adds work and must be off for performance measurements. Tests can call
`enable_deep_saturation_shadow` and `deep_saturation_shadow_counts` directly.

With `gen-counters`, set `LODESTONE_PRE_CORNER_SHADOW=1` for a bounded Overworld run. Automatic shadow
admission requires a supported stock plan and 4x8 geometry; other settings keep the exact path.
Without the feature or that exact variable value,
there are no diagnostic samplers or point sets. Tests can use `enable_pre_corner_shadow` explicitly
and inspect `pre_corner_shadow_counts` after the complete traversal. The focused controls are
`pre_corner_matcher_requires_the_identical_selector_in_the_minimum`,
`stock_bound_rejects_parameters_and_wrong_zero_bound_has_a_density_witness`, and
`pre_corner_shadow_removes_unique_blended_work_and_checks_exact_blocks`. Changing the matcher or
bounds requires all three controls, followed by the bounded real fill shadow. Zero certified cells
or zero avoided points is not evidence of a useful optimization.
The default-feature regression `pre_corner_skip_preserves_later_exact_density_bits` also checks
that a successful certificate publishes no terrain values and that later exact queries of the
skipped cell and its neighbors remain bitwise identical to a fresh scalar sampler.

There is no production runtime switch. `LODESTONE_DENSITY_PROBE_COLUMNS` affects
only the ignored, counter-enabled redundancy measurement. `PointScratch::with_capacity` selects the bounded
request-local table size; `PointScratch::new` uses the default 4096 entries.
Batch evaluation uses a fixed width of eight and lazily reserves one `f64` per
compiled plan register and lane. Memo participation comes from each source
`XzMemoId`, not from a second purity decision in the compiler. Plan storage is
bounded by the admitted root and cleared at each cell; it is not a
cross-column cache.

Column geometry and temporary buffer length come from the owning dimension's
noise settings. The squeeze output uses eight lanes without extending the
existing interpolation storage or adding a retained cache.
Prepared horizontal operands have no runtime switch or allocation. Their fixed budget
is four vertices, each with eight main and two sixteen-octave limit stacks; admission
requires the complete stock reverse-octave descriptors.

## Dependencies

The evaluator depends on `Density`, `Context`, `NormalNoise`, and the existing
counter/redundancy hooks in `lodestone-worldgen-core`. The production consumer
is the worldgen aquifer and its preliminary-surface cache; Nether and End fill
consume the aquifer's column sampler through `compose::fill_column`. Tests use
the checked worldgen density fixtures and do not require a process-wide
evaluator cache.
