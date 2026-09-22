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

There is no runtime switch. `PointScratch::with_capacity` selects the bounded
request-local table size; `PointScratch::new` uses the default 4096 entries.
Batch evaluation uses a fixed width of eight and lazily reserves one `f64` per
compiled plan register and lane. Memo participation comes from each source
`XzMemoId`, not from a second purity decision in the compiler. Plan storage is
bounded by the admitted root and cleared at each cell; it is not a
cross-column cache.

## Dependencies

The evaluator depends on `Density`, `Context`, `NormalNoise`, and the existing
counter/redundancy hooks in `lodestone-worldgen-core`. The production consumer
is the worldgen aquifer and its preliminary-surface cache; tests use the checked
worldgen density fixtures and do not require a process-wide evaluator cache.
