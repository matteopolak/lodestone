# Benchmark harness

## What it is

The criterion-based benchmark harness for epic [#78](https://github.com/matteopolak/lodestone/issues/78),
implemented for five crates so far: `lodestone-worldgen` (chunk generation,
sub-issues #84/#85), `lodestone-v770` (protocol decode throughput,
sub-issues #137/#142/#146/#88), `lodestone-world` (client-side chunk loading —
store insertion, heightmap decode, light propagation, light *application*,
memory footprint), `lodestone-entity` (mob simulation and pathfinding) and
`lodestone-physics` (movement integration, collision sweep, pose fit gate,
crowd push — sub-issues #115/#120/#124/#102). It is the concrete
implementation of the design recorded in
[`docs/roadmap/benchmarks.md`](./roadmap/benchmarks.md) — that doc is the
*argument* for the shape; this one is *how it actually works* and how to
extend it.

Sixteen bench binaries exist today:

| crate | bench | what it measures |
|---|---|---|
| `lodestone-worldgen` | `generation` | real-generator column throughput, per-stage cost split, linearity, thread-count sweep + RNG-determinism parity |
| `lodestone-v770` | `chunk_light_decode` | `level_chunk_with_light` decode throughput |
| `lodestone-v770` | `nbt_decode` | `read_network_nbt` throughput, two realistic payload shapes |
| `lodestone-v770` | `registry_decode` | block-state id → name/properties resolution (zero-heap and `BlockStateTable`) |
| `lodestone-v770` | `palette_expansion` | `PalettedContainer::iter` — local index → raw block-state id resolution, the middle stage between wire decode and `World::load` |
| `lodestone-world` | `chunk_load` | `World::load` insertion throughput — the real MP per-chunk consumer |
| `lodestone-world` | `heightmap_decode` | `Heightmaps::decode` — the real per-chunk heightmap consumer |
| `lodestone-world` | `light_propagation` | `compute_column_light`/`_with_neighbours` — the real SP per-chunk consumer |
| `lodestone-world` | `light_application` | `World::merge_light` — the real MP light-update consumer, single-column and render-distance-batch |
| `lodestone-world` | `memory_footprint` | tracked-baseline layer over `tests/memory.rs`'s five heap-byte fixtures |
| `lodestone-entity` | `pathfinding_search` | `PathFinder::find_path` over five scenes (open/detour/maze/sealed/real-collision-census stair gap) |
| `lodestone-entity` | `mob_tick` | `NavigatingMob::tick`, split into search-triggering vs. steady-follow regimes |
| `lodestone-physics` | `movement_integration` | `player::tick` per-tick cost, walking-on-ground vs. falling-in-air |
| `lodestone-physics` | `collision_sweep` | `collide` swept against open air / simple cube / real complex-shape census |
| `lodestone-physics` | `pose_fit_gate` | `can_player_fit_within_blocks_when`, succeeding vs. repeatedly-failing transition |
| `lodestone-physics` | `crowd_push` | `entity_push_impulse` pair-test cost at N = 10/50/200/1000 nearby entities |

Run any of them with `cargo bench -p <crate> --bench <name>`.

**`lodestone-physics` is the fifth `support.rs` copy** — the threshold "How
to change it" below already names for promoting the recorder to a real
crate. Not done in this pass: promoting it would mean editing `worldgen`'s
and `entity`'s `Cargo.toml`/`mod support;` lines while both crates were held
by concurrent agents working unrelated features, a bigger blast radius than
one more ~100-line copy. Left as the next thing to do once those crates are
free — see `benches/support.rs`'s own doc comment in `lodestone-physics`.

## How it works

### Two layers per bench function

Every `criterion_group!`-registered function does two things, in order:

1. **A one-shot diagnostic measurement** using plain `std::time::Instant`,
   printed to stdout and appended to `bench-results/<bench-name>.jsonl` via the
   `support` module (see below). This is what a developer reads for "what is
   the number" and what a future regression check would diff against.
2. **A criterion `bench_function`/`benchmark_group` call** over the same
   workload, for criterion's own statistically-robust sampling and its free
   local `--save-baseline`/`--baseline NAME` before/after workflow (see
   "Local before/after comparisons" below).

These are deliberately redundant rather than one replacing the other:
criterion's own numbers never leave `target/criterion/` and carry no scene/git
metadata; the JSONL recording carries metadata but has none of criterion's
outlier detection or confidence intervals. Read both.

### The `support` module (recording + metadata)

Each crate's `benches/support.rs` (logically identical across all four crates
— see the gotcha under "How to change it" for exactly what "identical" means,
and "Why duplicated, not a shared crate" below for why it is copy-paste at
all) exposes one function:

```rust
support::record(support::Record {
    bench: "generation",           // -> bench-results/generation.jsonl
    metric: "column_median_us",
    scene: "seed=42 patch=5x5(25 chunks)",
    value: 31798.04,
    unit: "us",
});
```

It appends one JSON line — `{timestamp, git_sha, machine, profile, scene,
metric, value, unit}` — to `<workspace-root>/bench-results/<bench>.jsonl`, then
prints a ratio against the most recent prior line with the *same* `(machine,
profile, scene, metric)` key, flagging anything outside a ±25% band. This is
advisory output only: nothing asserts, nothing fails a test, nothing runs in
CI. The workspace root is found by walking up from `CARGO_MANIFEST_DIR` to the
first ancestor whose `Cargo.toml` contains `[workspace]`, so it works
regardless of crate nesting depth (`crates/lodestone-worldgen` vs.
`crates/protocol/v770`).

### Local before/after comparisons

For a real "did my change help" workflow, use criterion's own baseline flow
directly — it needs no code changes:

```bash
cargo bench -p lodestone-worldgen --bench generation -- --save-baseline before
# make your change
cargo bench -p lodestone-worldgen --bench generation -- --baseline before
```

Criterion prints a statistically-tested percentage change per benchmark. The
`bench-results/*.jsonl` files are the cross-run, cross-session record; this is
the same-session, same-invocation record.

### Real inputs, not synthetic stand-ins — except where stated

`generation`'s benches build the actual `OverworldGenerator` via an
`FsResolver` reading `crates/lodestone-worldgen/tests/support/worldgen_data`
— the exact same checked-in JSON tree the JVM-parity tests use — so it is
benchmarking the real, verified pipeline, not a stub. `registry_decode`
benchmarks the real generated `block_states`/`block_registry` tables.

`chunk_light_decode` and `nbt_decode` are the exception, and say so in their
own module doc comments: no captured live-server bytes exist in this repo for
`level_chunk_with_light` or a representative NBT payload, so both build a
wire-format-accurate packet with this crate's own encoder (mirroring the
existing hermetic `tests/chunk_decode.rs` pattern) rather than replaying
captured bytes. That is a real limitation for *correctness* testing (per
`CLAUDE.md`'s evidence standard, a self-round-trip can validate a shared wrong
understanding) but not for *throughput* — decode cost is driven by container
shape, which the synthetic packets match exactly. `lodestone-world`'s
`heightmap_decode` inherits the same "no captured bytes" gap for the same
reason, but argues in its own module doc that it does not even need one:
`Heightmaps::decode`'s cost is a function of `count`/`world_height` alone, not
of the height values, so no fixture — synthetic or captured — could report a
different number for the same shape.

### The vacuous-world and duration-species traps, worked

`lodestone-world`'s `light_propagation` and `lodestone-entity`'s
`pathfinding_search`/`mob_tick` are the harness's concrete examples of two
`CLAUDE.md` failure modes that cannot be seen by reading the bench code —
only by checking what it was pointed at or how state moves across iterations:

- **World species.** A light bench over an empty/uniform section, or a
  pathfinding bench over open flat ground, degenerates to O(1)-ish work
  regardless of whether the algorithm under test is correct — a healthy
  number that proves nothing. `light_propagation` asserts
  `light_exercises_propagation` on its own fixture's output *before* timing
  starts, so a future fixture regression fails loudly. `pathfinding_search`
  keeps a genuinely cheap `open_flat` scene deliberately, but only as the
  negative control that makes the other three scenes' cost (measured at
  81x–574x `open_flat`, machine-dependent) evidence rather than assumption.
- **Duration species.** A `HashMap`-backed store (`World::load`) or a
  goal-driven mob (`NavigatingMob::tick`) both carry state that persists
  across a naïve `b.iter` closure's thousands of calls, so a late iteration
  can silently measure a different regime than an early one (an
  ever-growing map; a mob that has already reached its target and gone
  idle). `chunk_load` cycles a bounded ring of positions for the same reason
  `generation` does; `mob_tick` splits into two `iter_batched` benches with
  fresh per-batch setup — `mob_tick_with_search` and `mob_tick_steady_follow`
  — because those two regimes differ by roughly five orders of magnitude
  (~2–4 ms vs. ~30–40 ns per tick, machine-dependent) and a single number
  averaging across both would be meaningless for either.
- **A real measurement bug caught by exactly this discipline**, left in
  `chunk_load.rs`'s module doc as a worked example rather than scrubbed
  from history: an early draft built the `LoadedChunk` *inside* the timed
  `criterion` closure, reporting ~850 us/call for an operation the
  `Instant`-based diagnostic (which excluded construction) reported at ~2
  us/call — a 450x gap that was pure measurement error, not a real cost.
  A second draft fixed that with `iter_batched`, then hit a *second* trap:
  `BatchSize::SmallInput` told criterion the setup was cheap enough to batch
  hundreds of realistic chunks into one `Vec` ahead of the timed region,
  and the resulting allocator pressure bled into the timing anyway as wild
  variance (a single point estimate swinging 3 us → 330 us with no code
  change). `BatchSize::PerIteration` — appropriate whenever setup is not
  actually cheap — fixed it for good.

### Worked example: catching redundant work with `generation`'s stage split

`generation`'s per-stage split (shape / fluid+heightmap / surface / intern) is
what pointed at `lodestone-worldgen`'s surface stage as the place to optimise
— see [`docs/worldgen-surface-perf.md`](./worldgen-surface-perf.md) for the
full profiling story (a `samply` self-time breakdown, weighted by
`threadCPUDelta` per `CLAUDE.md`'s guidance, attributed back to pipeline
stages via stack-frame ancestry). The short version, measured on one machine
at seed 42 with `cargo bench -p lodestone-worldgen --bench generation`:

| point | column median (25-chunk patch) | shape | surface |
|---|---|---|---|
| before | ~24.1–25.0 ms | 36–38% | 55–57% |
| after two memoisation fixes | **12.5–12.8 ms** | 73.8% | 23.6% |

Both fixes were pure memoisation (same values, computed fewer times) —
verified bit-identical against the existing JVM-oracle parity tests, not a
new algorithm — and both were things the profile found, not things the code
looked like it needed on inspection: a per-column value that turned out to
be chunk-invariant and was being recomputed 256x per chunk, and a
98,304-entry `HashMap` pre-fill that only a handful of entries ever actually
needed to survive. Neither would have been obvious from `surface/mod.rs`
alone; both were obvious once `threadCPUDelta`-weighted self-time was
attributed back to `preliminary_surface_level` and `build_surface`'s own
frame, respectively.

## Status of specific sub-issues, re-verified rather than assumed

Per `CLAUDE.md`'s "re-verify before routing around 'X doesn't exist yet'"
rule, checked against the actual code rather than a prior comment's summary:

- **#85 (worldgen stage-cost split) is *not* fully done**, despite a prior
  epic-comment claim that it was "verified done, same commit". Read
  `OverworldGenerator::column_timed` (`crates/lodestone-worldgen/src/
  overworld.rs`): its four timed boundaries are shape (incl. aquifer),
  fluid+heightmap (actually biome), surface, and **intern — which silently
  folds in `carve_stage` and `ore_stage` too**. The issue explicitly asks for
  carvers/aquifer/ore features broken out on their own; they are not. Fixing
  that needs a `StageTimes` field change in `lodestone-worldgen/src/
  overworld.rs`, which is `crates/*/src/` (off-limits to this pass) and a
  crate currently held by a concurrent agent doing perf work there — flagged
  on the issue rather than attempted.
- **#93/#94/#95 are satisfied by `light_propagation.rs`**, which already:
  records the same functions #93 names into `support::record` (substance, not
  the literal `tests/memory.rs` conversion #93's acceptance criterion
  describes — the sanity tests stay untouched, per that issue's own
  instruction); reports the from-scratch cost at realistic edit rates for
  #94 (no incremental relight exists anywhere in the tree, confirmed by grep —
  the issue's own documented fallback); and writes down #95's negative
  finding (`Neighbourhood` is architecturally a fixed 3×3, confirmed at
  `lodestone-world/src/lighting.rs`, so there is no larger API to sweep
  against until that type changes shape). See that bench file's own module
  doc for the detail.
- **#86's remaining ask (thread-count sweep + in-benchmark parity)** is now in
  `lodestone-server/examples/bench_worldgen.rs`: a 1/2/4/8/workers/2×workers
  sweep reporting scaling efficiency, plus an FNV-1a fingerprint comparing
  serial vs. parallel output over a 3×3 chunk subset that `panic!`s on
  mismatch — the RNG-determinism break #86 is actually gated on, not merely a
  speed number.

## How to change it

- **Add a bench to a crate the harness already covers** (`lodestone-worldgen`,
  `lodestone-v770`, `lodestone-world`, `lodestone-entity`, `lodestone-physics`):
  add a `.rs` file
  under that crate's `benches/`, add a matching `[[bench]] name = "..."
  harness = false` entry to its `Cargo.toml`, and start the file with `mod
  support;` to get `support::record`.
- **Add the harness to a new (fifth+) crate**: copy `benches/support.rs` from
  any of the four existing copies, then update its opening doc comment's crate
  name (the one line every existing copy already differs on — see the gotcha
  below) and add the same `criterion` dev-dependency block. See "Why
  duplicated, not a shared crate" for why this is copy-paste today rather
  than a dependency.
- **Change the regression tolerance**: it is the `0.75..=1.25` literal in
  `support.rs`'s `record` function, matching the ±25% band
  `docs/roadmap/benchmarks.md` documents as policy. Change it in both copies if
  you change it at all — see the note below about keeping them in sync.
- **Gotcha: all four `support.rs` copies must stay logically identical.**
  There is no compiler check for this; a helper bug fixed in one crate's copy
  and not the others is a silent regression in three-quarters of the harness.
  Each copy's opening doc comment names its *own* crate (`"for
  `lodestone-world`'s criterion benches"`, etc.), so a raw `diff` between any
  two copies is expected to show that one line — not the actual gotcha this
  bullet is about. Anything past the header should agree word for word; if it
  does not, that is the regression to look for. (Checked while writing this
  section: `crates/lodestone-worldgen/benches/support.rs` and
  `crates/protocol/v770/benches/support.rs` currently differ by more than the
  header — a paragraph got rewrapped on one side without the other, purely
  cosmetic but a real instance of the two copies drifting. Both live in
  crates this pass does not own, so it is reported here rather than fixed.)
- **Gotcha: `bench-results/*.jsonl` is local and gitignored.** A fresh clone or
  CI runner has no baseline history; the first run of any bench always prints
  "no prior … baseline yet". This is intentional (a number is not comparable
  across machines), not a bug.

## Configuration

- **`criterion` dependency**: `default-features = false, features =
  ["cargo_bench_support"]` in all four crates' `Cargo.toml`, deliberately excluding
  `plotters` (HTML report charts) and `rayon` — neither is needed, and the
  harness design doc flagged `plotters` specifically as needing a
  dependency-tree check before adoption.
- **Bench profile**: benches build under Cargo's `bench` profile, which
  inherits `[profile.release]`'s `lto = "thin"`, `codegen-units = 1`, and
  `debug = 2` from the workspace root — the same optimisation level and
  DWARF-for-`samply` setup as a release build, not a separate profile to keep
  in sync.
- **Criterion CLI flags** (pass after `--`, e.g. `cargo bench -p
  lodestone-worldgen --bench generation -- --quick`): `--quick` for a fast,
  lower-rigor pass while iterating; `--sample-size N` / `--warm-up-time SECS`
  / `--measurement-time SECS` to control run length; `--save-baseline NAME` /
  `--baseline NAME` for local before/after diffing.
- **Output location**: `<workspace-root>/bench-results/<bench-name>.jsonl`
  (gitignored — see the `.gitignore` entry added alongside this doc).

## Dependencies

- `criterion` 0.8 (`cargo_bench_support` feature only) — dev-dependency in
  all four crates (`lodestone-worldgen`, `lodestone-v770`, `lodestone-world`,
  `lodestone-entity`).
- `serde_json` — already a dependency of `lodestone-worldgen`/`lodestone-v770`;
  added as a dev-dependency to `lodestone-world` (which had none before this
  pass) and was already present in `lodestone-entity`. Used for the JSONL
  encode/decode in `support.rs`.
- `git` (external binary, invoked via `std::process::Command`) — best-effort;
  its absence degrades `git_sha` to `"unknown"` rather than failing the bench.
- `lodestone-world`'s and `lodestone-entity`'s benches add no new crate
  dependencies beyond `criterion`/`serde_json`: both link only against the
  library they are benchmarking (its own public API) plus, for
  `lodestone-world`, `lodestone-core` (already a real dependency, for `Nbt`/
  `Reader`/`Writer`) and, for `lodestone-entity`, `lodestone-model`
  (already a real dependency, for `Vec3`/`BlockPos`).

## Why duplicated, not a shared crate

This pass's scope was deliberately narrowed to `crates/lodestone-worldgen/**`
and `crates/protocol/v770/**` bench files (see the epic issue), specifically
to avoid collisions with other agents working the same epic concurrently in
`lodestone-world`, `lodestone-render`, `lodestone-server`, etc. The workspace's
`crates/lodestone-*` / `crates/protocol/*` member globs mean a new crate (e.g.
`lodestone-benchkit`) would join the workspace with **no root `Cargo.toml`
edit needed** — so promoting `support.rs` to a real crate is a small, purely
additive change whenever a third bench site needs it. Doing that pre-emptively
for two call sites was judged not worth the naming/versioning overhead of a
new crate mid-epic; this doc records that as a decision, not an oversight.

A later pass (this one) *was* the third and fourth bench sites — `lodestone-world`
and `lodestone-entity`, filling in the epic's remaining "client-side chunk
loading" and "entities" areas — and made the same call again rather than
promoting `support.rs` now: still only four call sites, still cheap to keep in
sync by the `diff` check above, and each new crate stayed scoped to files this
pass owned (`crates/lodestone-world/**`, `crates/lodestone-entity/**`) with no
edit to any crate another agent might be holding. The threshold for actually
promoting it to a real crate has not moved: whenever a fifth site needs it.
