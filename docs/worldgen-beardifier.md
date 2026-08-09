# The beardifier — terrain adaptation under structures (worldgen phase S3)

## What it is

The density term that reshapes terrain under an adaptation-bearing structure: it
grows a flat foundation ("beard") up to a village house's floor, and swallows a
stronghold or a trial chamber whole ("bury"/"encapsulate"). Phase **S3** of issue
#514, and the only structures phase that changes *terrain* rather than adding
blocks on top of it. Vanilla's
[`Beardifier`](../.cache/mc/26.2/src/net/minecraft/world/level/levelgen/Beardifier.java);
ours is [`crates/lodestone-worldgen/src/structure/beardifier.rs`](../crates/lodestone-worldgen/src/structure/beardifier.rs).

## How it works

```text
structure_starts_stage (0a)      S1: where structures go
  ↓
structure_refs_stage   (0b)      the 17×17 walk; StructureRefs::adaptation_bearing
  ↓
beardifier_for(cx, cz) (0c)      Beardifier::for_chunk — rigid list + junctions
  ↓
fill_stage                       per block: block_at_beard(x, y, z, beard.compute(..))
  ↓
AquiferSystem::block_at_beard    final_density(x, y, z) + beard   ← the whole seam
```

Per block, `compute` walks its rigid list and adds one term per piece, then one
per junction:

| `terrain_adaptation` | `dy` | contribution | structures |
|---|---|---|---|
| `none` | 0 | `0.0` | 23 of the 34 bundled |
| `bury` | `y - groundY` | `bury(dx, dy/2, dz)` | `stronghold`, `trail_ruins` |
| `beard_thin` | `y - groundY` | `beard(dx, dy, dz, dyToGround) * 0.8` | 5 villages, `pillager_outpost`, `nether_fossil` |
| `beard_box` | `max(0, max(groundY - y, y - maxY))` | same as `beard_thin` | `ancient_city` |
| `encapsulate` | `max(0, max(minY - y, y - maxY))` | `bury(dx/2, dy/2, dz/2) * 0.8` | `trial_chambers` |
| a junction | `y - sourceGroundY` | `beard(dx, dy, dz, dy) * 0.4` | every jigsaw piece |

`bury` is a linear falloff — `clampedMap(|d|, 0, 6, 1, 0)`. `beard` is a Gaussian
kernel sample scaled by `-(dyToGround + 0.5) * fastInvSqrt(|d|²/2) / 2`, so its
**sign flips at the piece's ground level**: below it the term is positive (fill in
a foundation), at and above it negative (shave the hillside off). The foundation
therefore stops exactly one block below the piece's floor, which is right — the
piece places its own floor block.

## How to change it, and the gotchas

**`beardifier` appears nowhere in 26.2's worldgen JSON, and `Density::Beardifier`
evaluating to `0.0` is correct.** This is the single most confusing thing about
this phase. `grep -rn beardifier .cache/mc/26.2` finds exactly one hit, in
`registries.json`'s type list — the marker is only reachable from data a pack
author wrote. The overworld's own adaptation comes from *code*:
`NoiseChunk`'s constructor (`NoiseChunk.java:155-160`) wraps the router as
`cacheAllInCell(add(finalDensity, BeardifierMarker.INSTANCE))` and `NoiseChunk.wrap`
substitutes the real `Beardifier` for the marker. So anyone who "fixes"
`OpKind::Beardifier => 0.0` in `engine/field.rs` is fixing the wrong thing.

**The term is added at the `final_density` call site, not inside the density
graph**, and that is exact rather than approximate: `add` is the top operand and
`Ap2(ADD)` is `argument1.compute(ctx) + argument2.compute(ctx)`, so
`self.final_density.final_density(x, y, z) + beard` is the same floating-point
expression in the same order. **Operand order is the specification** — flipping it
is a different `f64`. Keeping the beard out of the graph is also what lets the
compiled `Graph` stay shared across threads behind an `Arc` while the beard is
per-chunk mutable input.

**`block_at` has no beard, deliberately.** Vanilla passes
`BeardifierMarker.INSTANCE` (a constant `0.0`) at both of
`NoiseBasedChunkGenerator`'s non-fill call sites (`:145`, `:230` — `getBaseColumn`
and `getBaseHeight`), which is why a structure's own height probe does not see the
terrain its own beard is about to create. `StartSampler::first_occupied_height`
goes through `block_at`, so this is already right; adding a beard there would make
placement depend on placement.

**The empty branch in `fill_stage` is a correctness property, not a micro-optimisation.**
For a chunk with no adaptation-bearing start in reach the fill runs the *pre-S3
loop verbatim* — no addition at all. `x + 0.0` is the identity for every finite
`f64` *except* `-0.0`, where it flips the sign bit, and the branch means that
question never has to be answered about the rest of the pipeline.

**`fast_inv_sqrt` is Newton's method off a magic integer, not `1.0 / sqrt(x)`.**
Its relative error is ~1.7e-3 — three orders of magnitude larger than a rounding
difference — so a "cleaner" rewrite changes the beard shape everywhere.
`fast_inv_sqrt_is_newton_not_exact` pins eight exact `f64` values *and* asserts
the error stays in `[1e-4, 2e-3]`, precisely so the rewrite cannot pass.

**The kernel is indexed `[zi][xi][yi]` — Y innermost.** `zi * 576 + xi * 24 + yi`.
A transposed index is symmetric in x and z and would pass any value check; the
guard is that the `+0.5` y-offset makes `(0, +1, 0)` and `(0, -1, 0)` differ while
`(+1, 0, 0)` and `(0, 0, +1)` agree.

**Widening the reference walk widens the halo the join scheduler must lead by.**
`StructureRefs` keeps a start whose *adjusted* (12-inflated) box comes within
`BEARD_REACH` of the chunk, which is wider than vanilla's `createReferences` test.
That is fine and intentional: the piece-level `isCloseToChunk(chunk, 12)` filter
in `for_chunk` re-narrows it to exactly vanilla's set, because a piece within 12
blocks of the chunk necessarily puts the start's 12-inflated box across the chunk
box. One product, two consumers, no second chance to disagree about reach.

## What actually adapts today

**Villages, `pillager_outpost` and `ancient_city` — since S4 landed jigsaw
assembly.** Until then this section read "nothing, in a generated world", which was
a phase-order fact rather than a defect; it is now stale in the direction that
matters, so here is the current table.

| structure | adaptation | adapts? |
|---|---|---|
| `village_{plains,desert,savanna,snowy,taiga}` | `beard_thin` | **yes** (S4) |
| `pillager_outpost` | `beard_thin` | **yes** (S4) |
| `ancient_city` | `beard_box` | **yes** (S4) |
| `trail_ruins` | `bury` | no — jigsaw, but its `capped` processor is ledgered |
| `trial_chambers` | `encapsulate` | no — jigsaw, but `pool_aliases` is ledgered |
| `stronghold` | `bury` | no — coded pieces (S5) |
| `nether_fossil` | `beard_thin` | **yes** (S7) — a *template* piece, not coded as this row used to claim, and Nether-only. The first non-jigsaw structure whose beard is observable; `tests/nether_structures.rs`'s `the_nether_beard_is_live_at_a_fossil_and_empty_away_from_one` is that measurement |

So `StructureRefs::adaptation_bearing` is **no longer empty**: a chunk within 12
blocks of any village piece yields a real rigid list plus junctions, and
`fill_stage` takes its per-block branch there. Consequently **the generated world is
no longer byte-identical to pre-S3 around a village**, which is exactly what S3 was
built for — the 45-column/5-seed byte-identity dump that was S3's negative control
is only a valid *control* for a column with no adaptation-bearing start in reach.

`PieceBeard` is the seam S4 fills, and it now really is filled by
`jigsaw::Placer::into_pieces`: `rigid` (a `terrain_matching` element contributes no
rigid box, only junctions — bearding one would flatten a village's roads),
`ground_level_delta`, and `junctions`. A coded piece leaves it `None`, which is
vanilla's own `else` branch: rigid box, `groundLevelDelta` 0, no junctions.

**One correction to what this doc used to say about `ground_level_delta`:** it is
*not* read from a template marker. In 26.2 `StructurePoolElement.getGroundLevelDelta`
returns a **constant 1** and nothing overrides it; older versions read a `bottom`
data marker, and the S3→S4 handoff note repeated that. The value a piece carries is
`1` for the centre and for every `terrain_matching` child, and
`sourceGroundLevelDelta - deltaY` for a rigid child — computed by
`JigsawPlacement`, not read from NBT.

## Evidence

**The negative control is the load-bearing half**, because S3 is the phase that can
silently perturb the entire world.

`tests/u15_column_dump.rs` run in two arms — an isolated `git worktree --detach`
at the pre-S3 sha (`a722265a`) and the same worktree with only the S3 patch
applied, same `CARGO_TARGET_DIR`, dumper md5 verified identical on both:

```text
45 columns, 5 seeds, 8,902,157 bytes, 74 distinct block states, 1,414,423 non-air
before == after: True    differing bytes: 0
```

Compared with a Python read of both files, not a shell pipeline. Two controls on
that control:

- **The detector fires.** Forcing the per-block branch *and* adding `+0.557` (a
  real `beard_thin` magnitude at a piece floor) at every block changes the dump —
  different length, so not a subtle drift.
- **Forcing the branch alone changes nothing** (byte-identical), which is the
  measurement behind the claim that an empty `Beardifier::compute` really returns
  `0.0` and not `-0.0`.

The production generator has structure data, so all 45 columns really do run
`beardifier_for`'s 17×17 walk and really do get an empty answer — the new code
path is exercised, not skipped.

`tests/structure_beardifier.rs` is the positive half, three arms:

| arm | asserts |
|---|---|
| `a_beard_raises_terrain_under_the_piece` | the solid top under a `beard_thin` piece rises to **exactly** `floor - 1` — a predicted value, derived from the sign of `-(dyToGround + 0.5)`, not a direction |
| `the_beard_is_local_to_its_affected_box` | inside the affected box something changed; **outside it, zero cells differ**, with >10,000 cells outside so the claim is not vacuous. Failure prints the bounding box of the differing cells |
| `no_start_means_no_beard_and_no_change` | over a 5×5 patch of the oracle world, `beardifier()` is empty for every chunk and the field is identical to an explicitly empty beard |

The start in the positive arms is **synthetic**, which the phase order forces: no
real 26.2 start can carry a beard until S4 or S5 lands. The arithmetic's expected
values come from `Beardifier.java` hand-expanded in `beardifier.rs`'s own unit
tests (the `bury` falloff at 0/3/6 blocks, the exact kernel product at the floor,
`fast_inv_sqrt`'s eight bit-exact values, the strict junction window, and
`beard_box`'s `dy` clamp), and the seed and chunk come from the vanilla-authored
oracle world so the terrain being reshaped is a real world's.

## Configuration

None. No env var, no feature flag. `BEARD_KERNEL_RADIUS` (12), the kernel size
(24) and the affected-box inflation (24) are vanilla constants, not tunables —
changing any of them changes every adapted structure in the world.

## Dependencies

`BoundingBox`/`TerrainAdjustment`/`StructureStart` from
[`crate::structure`](../crates/lodestone-worldgen/src/structure/mod.rs),
`crate::math::clamped_map`, and
[`StructureRefs`](../crates/lodestone-worldgen/src/overworld/structures.rs) for its
input. No noise, no RNG, no resolver — every value is a pure function of piece
geometry, which is what makes it unit-testable against the record definition.

## See also

- [Structure placement (S1)](./worldgen-structure-placement.md) — where structures
  go, and why `structure_starts` runs before `NOISE`.
- [Structure templates and processors (S2)](./worldgen-structure-templates.md) —
  how a start becomes blocks.
- [The density engine](./worldgen-density-engine.md) — the `Graph`/`Field` the
  beard is added on top of, and why it is not added inside.
