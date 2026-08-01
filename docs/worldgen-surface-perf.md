# Surface-stage generation performance

## What it is

Two profile-driven optimisations to `lodestone-worldgen`'s surface stage
(`crates/lodestone-worldgen/src/surface/mod.rs`, consumed by
`crates/lodestone-worldgen/src/overworld.rs`), landed together because the
`generation` bench (`docs/benchmark-harness.md`) showed the surface stage was
the majority of per-chunk cost. Both are pure memoisation/plumbing changes —
same values computed, fewer times — verified bit-identical against the
existing JVM-oracle parity tests (`tests/surface_parity.rs`,
`tests/overworld_gen.rs`), not a new algorithm.

## How it works

### 1. `preliminary_surface_level` was recomputed 256x more than it needed to be

`SurfaceSystem::build_surface` scans one 16×16 chunk. For every one of its 256
`(x, z)` columns it used to call `min_surface_level(block_x, block_z, ...)`,
which itself calls `preliminary_surface_level` at **4 corner positions**
(`corner_cell_x = block_x >> 4`, etc.) and bilinearly interpolates between
them. But `block_x >> 4` is the *same* value for all 16 `x` in a chunk (chunk
width is exactly 16, and callers always pass a chunk-aligned origin) — so all
256 columns were asking for the same 4 corner values, 256 times over, instead
of once.

`preliminary_surface_level` is not cheap: it is a `find_top_surface` density
node (`crates/lodestone-worldgen/src/density/mod.rs`), which linearly scans
Y in `cell_height`-sized steps evaluating a nontrivial density subgraph at
each step. Doing that 1024 times per chunk (4 corners × 256 columns) instead
of 4 was the single largest cost in the surface stage.

**The fix**: `build_surface` now computes the chunk's 4 corner values once,
before the per-column loop, and every column reuses them via
`SurfaceSystem::interpolate_min_surface_level` — the same lerp math
`min_surface_level` already did, just fed precomputed corners instead of
recomputing them. `min_surface_level` itself is untouched and still used by
`top_material` (the carver-facing single-position API), which only ever
queries one arbitrary position at a time and has no chunk-batch structure to
exploit.

### 2. `build_surface` no longer materialises the whole column up front

`build_surface` used to open with a loop over all `16×16×gen_depth`
positions (98,304 of them at `gen_depth = 384`), inserting
`pre(x, y, z).to_string()` into a `HashMap<(i32,i32,i32), String>` for every
single one — before the real surface-rule scan even started. The scan then
selectively overwrote a much smaller subset (wherever a rule actually
rewrote a block). The up-front fill was pure overhead: ~98K `String` clones,
hashes (`SipHash`, the default `HashMap` hasher) and `HashMap` inserts
(including rehashing, since capacity was never reserved), the overwhelming
majority of which were immediately either overwritten or never read again.

**The fix**: `build_surface` now returns a **sparse diff** — only the
positions a surface rule actually rewrote — documented as such on the
function. A position absent from the map means "unchanged from `pre`", not
"missing". Callers that need the full column reconstruct it from `pre`
(or, for `overworld.rs`'s composed pipeline, from the already-in-hand
`solid` mask) overlaid with this diff, instead of the diff itself being
built as an exhaustive copy.

This changed one call site's contract, not the algorithm: which positions
get rewritten to what is byte-for-byte identical (that's exactly what the
JVM-oracle parity tests re-verify — see below). Only the bookkeeping around
"what does the caller do with a position the rules left alone" changed.

## How to change it

- **`build_surface`'s sparse-diff contract lives in its own doc comment**
  (`src/surface/mod.rs`) — read it before touching either call site
  (`overworld.rs::intern_stage`, `tests/surface_parity.rs`). If you add a
  new caller, it must treat "absent" as "equal to `pre`", not "air" or
  "error".
- **`OverworldGenerator::intern_stage`** (`src/overworld.rs`) now takes
  `solid: &[bool]` in addition to the diff, and seeds the dense `blocks`
  array from it (mirroring `surface_stage`'s own `pre` closure: solid ->
  `default_block`, else below sea level -> `default_fluid`, else air) before
  overlaying the diff. If you change what "pre-surface" means in
  `surface_stage`'s `pre` closure, mirror the change here — the two must stay
  in sync by construction, since `intern_stage` is reconstructing exactly
  what `pre` would have returned for the positions the diff doesn't cover.
- **The corner-cell hoist assumes chunk-aligned input.** `build_surface`'s
  `min_block_x`/`min_block_z` must be `(chunk_x * 16, chunk_z * 16)` for the
  hoist to be valid (`block_x >> 4` constant across all 16 columns requires
  it). This was already an implicit contract (the doc comment already called
  `min_block_x`/`min_block_z` "the chunk's world-space origin"); the hoist
  just makes it load-bearing for performance, not only for correctness of the
  interpolation math itself. `top_material`'s single-position path is
  unaffected — it doesn't take this shortcut.
- **Follow-up landed**: `Density::compute` now caches `cache_2d` (real
  `Cache2DSlot`, keyed on exact `(x, z)`, ignoring `y`) — the node kind that
  sits directly over `find_top_surface`'s per-`y` scan in
  `preliminarySurfaceLevel`'s own tree (`NoiseRouterData.java:489-490`'s
  `cache2d(offset)`/`cache2d(factor)`). `flat_cache` was tried the same way
  first and **reverted**: it regressed `column()`'s median by +11–13% across
  every bench function, because in this crate's actual call graph
  `flat_cache` nodes (`continents`/`erosion`/`ridges`) are reached almost
  entirely as `spline` `coordinate` inputs, which `NoiseChunkSampler`
  (`chunk.rs`) treats as a corner-deduplicated *leaf* — so each raw `compute`
  call already lands on a distinct `(x, z)`, and a last-value cache there pays
  a `Mutex` lock for (almost) no hits. `interpolated` and the other three
  markers (`cache_once`, `cache_all_in_cell`, `blend_density`) stay
  transparent, matching vanilla's own behaviour off the cell-filling loop —
  see `crates/lodestone-worldgen/src/density/mod.rs`'s `## Caching` section
  for the full per-node-kind reasoning and jar citations. Net effect, measured
  with a same-session criterion `--save-baseline`/`--baseline` pair on a quiet
  machine: **−4.4% (95% CI −6.0%..−2.7%, p < 0.05)** on `column()`'s median —
  real but modest, because `preliminary_surface_level` was already a minority
  of the surface stage's own cost even before this change (the corner-cell
  hoist above ate the larger, 256×-per-chunk redundancy; this catches the
  smaller, per-`y`-step one it couldn't touch). All parity tests
  (`surface_parity.rs`, `chunk_parity.rs`, `aquifer_parity.rs`,
  `density_parity.rs`, `region_parity.rs`) stayed 100% bit-exact throughout.
- **The shape stage's own memoisation (`NoiseChunkSampler`'s `FxHash`-keyed
  corner cache, `chunk.rs`) was assessed as the next lever and not attempted.**
  A `samply` profile (`threadCPUDelta`-weighted, symbolicated via a local
  `samply load` + Firefox Profiler session against the DWARF `debug = 2`
  build) found `HashMap::get` alone at ~10% of total profiled self-time
  inside `NoiseChunkSampler::slot_get` — a materially larger target than
  `preliminary_surface_level` was. Replacing it with vanilla's real
  incremental, array-indexed `NoiseChunk` update (`initializeForFirstCellX` /
  `advanceCellX` / `selectCellYZ` / `updateForY` / `updateForX` / `updateForZ`,
  `NoiseChunk.java:250-336`) is **not a like-for-like swap**: vanilla's
  algorithm is a stateful, strictly-ordered walk (X-cell outer, then
  Y/Z-cell, precomputing 8 corner values once per cell into double-buffered
  `slice0`/`slice1` arrays and reusing partial Y-then-X-then-Z lerps across
  every block in that cell) with no analogue in this crate's current
  per-block `final_density(x, y, z)` point-query API. Adopting it would mean
  restructuring `NoiseChunkSampler`'s public API *and* every caller's
  iteration order (`OverworldGenerator::shape_stage`'s `for lz { for lx { for
  ly } } }` loop, currently Z/X/Y, would have to become vanilla's cell-major
  order) — a rewrite of exactly the code `chunk_parity.rs`'s
  `interpolated_final_density_matches_jvm_over_whole_chunk` test exists to
  hold bit-exact, where an axis-order or lerp-sequencing slip would be far
  easier to introduce than in the two memoisation-only changes made here.
  Given the modest, already-measured return on the lower-risk lever, this was
  costed and left as a follow-up rather than attempted in the same pass.

## Configuration

None — no flags, no data-driven behaviour change. Both changes are internal
to `lodestone-worldgen` and invisible to `OverworldGenerator::column`'s
public output.

## Dependencies

- `crates/lodestone-worldgen/src/density/mod.rs` — `find_top_surface`,
  `Density::compute`, referenced above for the next-lever note.
- `crates/lodestone-worldgen/src/density/chunk.rs` — `NoiseChunkSampler`,
  the shape stage's own (already-memoised) evaluator, contrasted with
  `Density::compute` above. Not modified.
- `crates/lodestone-worldgen/benches/generation.rs` — the bench that surfaced
  this (`docs/benchmark-harness.md`) and the one used to measure it.

## Evidence

Correctness: `cargo test -p lodestone-worldgen --no-fail-fast` — 13 suites,
0 failures, both before and after each change individually. The
load-bearing tests are the two whole-chunk JVM-oracle comparisons in
`tests/surface_parity.rs` (`surface_rules_match_jvm_ocean_chunk`,
`surface_rules_match_jvm_land_chunk` — 98,304 blocks each, block-for-block
against a real 26.2 server dump, one oceanic and one land column so both
water/stone-depth banding and grass/dirt banding are exercised, not a flat
or empty scene) plus `tests/overworld_gen.rs`'s composed-pipeline tests. All
four passed unchanged after both changes. A same-tree before/after self-diff
was considered and skipped: per `CLAUDE.md`'s evidence standard, a
self-comparison is *weaker* evidence than the JVM-oracle comparison already
being re-run, and producing one safely would have meant briefly reverting
shared files in this repo's single shared checkout — not worth it for
strictly weaker evidence.

Performance, measured with `cargo bench -p lodestone-worldgen --bench
generation` (release profile, `lto = "thin"`, `codegen-units = 1`), same
machine, seed 42, before/after each change in sequence:

| point | column median (25-chunk patch) | shape | fluid+heightmap | surface | intern |
|---|---|---|---|---|---|
| baseline (this session, before either change) | ~24.1–25.0 ms | 36–38% | 0.1% | 55–57% | 6–8% |
| + corner-cell hoist | 17.4 ms | 56.6% | 0.1% | 35.1% | 8.2% |
| + sparse diff | **12.5–12.8 ms** | 73.8% | 0.2% | 23.6% | 2.3% |

Net: roughly **24.5 ms -> 12.6 ms**, a ~1.94x speedup, entirely from the
surface stage (its absolute cost fell by more than half again on top of the
first change; shape and fluid+heightmap were untouched and their *share*
only grew because the denominator shrank). Linearity (4x chunks -> ~4.1–4.3x
time) held throughout, both before and after.

The task brief that motivated this cited an independently-measured 31.8 ms/chunk
baseline. This document's own "before" figure (~24.1–25.0 ms) is a
different measurement — same machine, same day, but a separate run — and is
reported as such rather than presented as a reproduction; see
`docs/benchmark-harness.md`'s own note about the harness's baseline/seed-3
figures for the same caveat applied consistently.

With surface's share now down to ~24%, shape (`NoiseChunkSampler`, the
per-block density-field sampler) is the new majority cost at ~74%. It
already does its own memoisation (an `FxHash`-keyed corner cache per
interpolation slot — see `src/density/chunk.rs`), unlike the generic
`Density::compute` path discussed above; a further win there would mean
replacing that per-call hash-map-lookup memoisation with vanilla's real
incremental (array-indexed, no hashing) `NoiseChunk` update pattern, which
is a materially larger rewrite of code that `chunk_parity.rs` already proves
bit-exact — left as a follow-up, not attempted here.

## 3. `NoiseChunkSampler`'s hash map, replaced by a dense grid (`ada197f`)

The follow-up above **was** taken, but not by the route it describes. Vanilla's
stateful cell-ordered walk would have forced `NoiseChunkSampler`'s public API
*and* `shape_stage`'s iteration order (Z/X/Y → cell-major) to change together —
an order-dependent rewrite where `chunk_parity.rs` is the only thing that would
catch a slip.

`DenseSlot`/`DenseShape` get the array-indexing win **without** that: the
point-query API is unchanged and there is no iteration-order change at all. The
insight is that both key families are regular, so one grid addresses both
exactly — `interpolated` corners land on multiples of `cell_width`/`cell_height`,
`flat_cache` keys on multiples of 4 (hardcoded in vanilla, *not*
`cell_width`-parameterised), so stepping X/Z by `gcd(cell_width, 4)` divides both.
Every real key lands on an exact grid point rather than being rounded onto one.

### The measurement, and why the obvious number is the wrong one

**−13.2%: 12 639.6 µs → 10 972.7 µs** (`column_median_us`, seed 42, 5×5 patch,
release, this machine).

That compares two **quiet** readings: `d0cd8d6` before the change against
`4d34681` after. It is *not* a paired same-run A/B, which would require reverting
the change — stated plainly rather than implied, because the distinction matters
for how much weight the figure carries.

**Do not quote criterion's own `change:` line for this commit.** It reported up to
**−32%**, and that is an artifact: its saved baseline came from a run taken while
six agents were building concurrently. The same commit's recorded history shows
that load directly — `fe7238d` produced 13 471, 13 537, 13 753, 14 751 and
14 827 µs on five consecutive runs, a spread of over 10% on *identical* code.
Comparing a quiet run against a loaded baseline manufactures a win.

This run itself was at load 2.2–2.95, not zero. Better than the 53 the first pass
saw, but the honest reading is "−13.2% with a couple of points of noise", not a
precise figure.

Cumulative across all three passes on this pipeline: **~24–25 ms → ~11.0 ms.**
