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

The production handoff is `OverworldGenerator::new` → `AquiferTrees` →
`AquiferSystem::from_parts_with_preliminary_cache`. Keep the compiled graph in
the generator-owned trees and the scratch in the per-chunk aquifer. The focused
aquifer controls must prove both that this handoff is selected and that a raw
recursive control remains bit-identical.

## Configuration

There is no runtime switch. `PointScratch::with_capacity` selects the bounded
request-local table size; `PointScratch::new` uses the default 4096 entries.
Batch evaluation uses a fixed width of eight and lazily reserves one `f64` per
compiled node and lane. Memo participation comes from each source `XzMemoId`,
not from a second purity decision in the compiler.

## Dependencies

The evaluator depends on `Density`, `Context`, `NormalNoise`, and the existing
counter/redundancy hooks in `lodestone-worldgen-core`. The production consumer
is the worldgen aquifer and its preliminary-surface cache; tests use the checked
worldgen density fixtures and do not require a process-wide evaluator cache.
