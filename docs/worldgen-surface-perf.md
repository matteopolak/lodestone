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
- **If `find_top_surface`'s own internal scan turns out to also be a
  bottleneck**, note that `Density::compute` (the generic evaluator used for
  `preliminary_surface_level`, as opposed to `NoiseChunkSampler`'s
  memoised `eval`/`slot_get` used for the shape stage) treats `FlatCache` /
  `Interpolated` / `Marker` density nodes as **pure pass-throughs with no
  caching** — correct values, but every nested noise sample inside one of
  those wrappers is recomputed from scratch on every `compute()` call, even
  when (as `cache_2d`-wrapped `overworld/offset`/`overworld/factor` are) the
  value only depends on `(x, z)` and is being asked for repeatedly at
  different `y` within one `find_top_surface` scan. This was not touched
  here — the corner-cell hoist above already eliminates the 256x outer
  redundancy that made this worth investigating — but a real per-node cache
  in `Density::compute` (matching what `NoiseChunkSampler` already does for
  the shape stage) is the next lever if `preliminary_surface_level` shows up
  hot again.

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
