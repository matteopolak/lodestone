# Benchmark harness

## What it is

The criterion-based benchmark harness for epic [#78](https://github.com/matteopolak/lodestone/issues/78),
implemented for six crates so far: `lodestone-worldgen` (chunk generation,
sub-issues #84/#85), `lodestone-v770` (protocol decode throughput,
sub-issues #137/#142/#146/#88), `lodestone-world` (client-side chunk loading —
store insertion, heightmap decode, light propagation, light *application*,
memory footprint), `lodestone-entity` (mob simulation and pathfinding),
`lodestone-physics` (movement integration, collision sweep, pose fit gate,
crowd push — sub-issues #115/#120/#124/#102) and `lodestone-render` +
`lodestone-shell` together (render-submit counts and durations — sub-issues
#106/#128/#133/#160, see
["The render/entity batch"](#the-renderentity-batch-87-90-91-92-97-99-106-128-151-160)
below for the split between the two crates and what closed in the most recent
pass). It is the concrete implementation of the design recorded in
[`docs/roadmap/benchmarks.md`](./roadmap/benchmarks.md) — that doc is the
*argument* for the shape; this one is *how it actually works* and how to
extend it.

Eighteen bench binaries exist today:

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
| `lodestone-render` | `render_submit` | terrain draw-list sizes, entity-planning batch counts, mesh-arena occupancy under load, texture-atlas packing occupancy |
| `lodestone-shell` | `render_submit` | `RenderState::render`'s terrain draw-call/camera-bind-group-switch counts and CPU submit-time baseline, swept by resident section count over the packed/demo path |

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

## Structural counters, and the two numbers they replace

Added by Unit 1 of [`docs/plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md).
`crates/lodestone-worldgen/src/counters.rs` counts *events* in the generation
pipeline — `block_at` calls, density component evaluations by kind, corner
lookups and slot-cache hits/misses, palette interns, `pre_ore`/`post_ore`
computations vs cache hits, biome nearest-neighbour searches and rows compared,
stitch copies, `String` allocations, interner interns and id-to-name lookups, and
RNG draws attributed per stage. The bench binary's counting allocator additionally
bins **real** heap allocations per stage (`ALLOC_BY_STAGE`), which is how a
residual allocation total gets attributed rather than guessed at.

**Why a counter and not another timing.** `DESIGN.md` §12.95 carries a measured
**585×** mis-attributed timing, and §12.98 records that release-profile
benchmarking was silently broken workspace-wide for long enough that a
9× structural regression was found by a 700-second test run instead of an
assertion. A counter is reproducible under machine load, predicts an exact value,
and can therefore *gate*. Acceptance criteria for worldgen-rewrite Units 3–14 are
written in these counters, not in microseconds — the harness is the contract.

**The rule that makes them worth having: a counter that cannot predict is a
counter that cannot gate.** Every counter has a hand-derivable expected value,
and `bench_counter_calibration` asserts them exactly on one known chunk. Five
independent predictions from the rewrite plan were confirmed this way (98,304
`block_at` per chunk fill; a 25-chunk pre-ore closure; 9 ore walks; a 7,594-row
climate table; ~885k `String`s per warm column). If you add a counter, add its
prediction in the same commit.

**When a unit deletes what a calibration asserts, invert the assertion — do not
delete it.** Unit 3 removed the ~885k `String`s, so that prediction became false
and `bench_counter_calibration` failed, correctly. Its replacement names *both*
hypotheses and requires the measurement to land on one: pre-U3 is
`string_allocs >= 884,736`, post-U3 is interner warmup only (measured 65, ceiling
1,000). A one-directional bound would have been the *magnitude* species of vacuous
test — satisfied by any improvement at all, including a broken one.

**Measured on this file, in one sitting, when the two instruments disagreed.**
Re-taking the U2 baseline counters-off produced three runs of an *identical*
release binary:

| run | C_ss | vegetation stage | steady-state allocations |
|---|---|---|---|
| 1 | 101.68 ms | 63.42 ms | **905,459** |
| 2 | 97.78 ms | 63.77 ms | **905,459** |
| 3 | 95.99 ms | 52.28 ms | **905,459** |

The vegetation figure swung **22%** across three runs of the same binary while
the allocation counter reproduced **to the digit, three times out of three**. So
when a duration and a counter disagree here, **believe the counter** — and the
corollary, which is the part that actually costs time if you get it wrong: an
effect *smaller* than the timing spread cannot be measured with a timing at all.
The same three runs put the counters-off C_ss median at 97.8 ms against a
counters-on 96.8 ms, i.e. counter overhead is **below this instrument's own noise
floor**, so the prediction that removing the counters would show a speedup was
simply not answerable that way. (§12.98's follow-up block carries the full
record.)

Two practical consequences for a unit written against these counters:

- **Verify the `gen-counters` feature is neutral for whatever you are counting,
  and say so.** Unit 3 needed per-stage allocation attribution, which only works
  with the feature on; `steady_state_heap_allocs_per_column` reads 20,684 with it
  on *and* off identically, which is what licenses attributing the one to the other. Without
  that control the attribution would describe a different program from the one
  the ratchet measures.
- **A per-column counter only sees stages that actually run on that column.** On
  a warm column, fill/surface/carve/ore are cache hits and contribute exactly 0,
  so a change to any of them is invisible to `C_ss`'s counters no matter how much
  it improves `C_cold`. Check *which* stages your metric can see before aiming a
  unit at it.

### `I_ss`: instructions retired, the standing before/after comparator

**The problem it solves.** Wall clock in `benches/generation.rs` reproduces to
~10.8% on this machine (DESIGN.md §12.112), and §12.98 records a 20% swing on an
*identical binary*. That is wider than most of the wins a later optimisation unit
will claim, which is why 21 units of the rewrite drive landed against allocation
counters — and why the headline figure went unmeasured for the whole drive. Once
allocations reached 118 per column the counters stopped being a proxy for time, and
nothing had replaced them.

`proc_pid_rusage(getpid(), RUSAGE_INFO_V4, …)`'s `ri_instructions` reproduces to
**0.18–0.21%** measured over 7 repetitions of identical work in this very bench,
against **11.6–19.1%** for the wall clock on the same repetitions. It is
unprivileged, ~600 ns of wall time per read, and the transcription reaches the field
**by name** out of a field-by-field `#[repr(C)]` struct — the pattern comes from
`crates/lodestone-shell/tests/client_chunk_cycles.rs`, which is the first customer
and carries the third (locality) control. Baseline: **488,507,564
instructions/column**, recorded as `i_ss_median_instructions_per_column`.

**Read it as a pair with the wall clock, never instead of it.** Instructions are
blind to locality: §12.120 records a case measuring 490k instructions against ~7×
more time. The bench prints `insn/µs` for exactly this reason — a change that moves
that ratio while `I_ss` stays flat is a cache story, not a work story.

Three things about it that were paid for (§12.130):

- **Measure it over the same scene the wall-clock metric uses.** The easy shape —
  repeat one already-warm column and take the median — measured 3.6 ms against
  C_ss's 21.4 ms, because a repeat re-runs only vegetation/top_layer/intern while
  the store answers the rest. `I_ss` is therefore the median of the *same 100
  interior columns* `C_ss` is. **A comparator blind to 83% of the cost is worse
  than none, because it looks like one.**
- **The instrument has a floor of tens of thousands of instructions.** A
  `proc_pid_rusage` read costs about **80,000 instructions** — it walks the task's
  threads, far more than its wall figure suggests. Nothing under ~1 ms of work can
  be measured with it directly, and a scaling control needs a loop bound large
  enough that the fixed term is negligible: at 200,000 iterations the 4× control
  read **3.663** on a correct reading.
- **A pure kernel used as a scaling control will be CSE'd.** `#[inline(never)]`
  prevents inlining, not common-subexpression elimination; a kernel that reads no
  memory is inferred `readnone`, and two calls with provably equal arguments
  collapse into one. That made the control read **1304.7×**. `black_box` the
  *argument*, not only the result.

### The five traps this cost us to learn

- **A stage-participation counter belongs *below* the stage's no-data early
  return.** Above it, the counter reports "the stage ran" for exactly the run
  where the resolver supplied nothing and the stage early-returned — the *world*
  vacuity species, and `benches/generation.rs`'s own documented history. Below it,
  `stage_entered[top_layer] == 0` is a mechanical detector for "this bench is
  secretly pointed at the fixture tree".
- **"Exactly once per chunk" is one sentence with a different count per stage.**
  Each stage has its own dependency radius, so over a 12×12 sweep the correct
  assertion is 256 (5×5 closure) for fill/surface/carve, 196 (3×3) for ore, and
  144 for vegetation/top_layer/intern. A single "144 of each" is wrong for nine of
  the ten and fails for the wrong reason.
- **Counting allocations needs a thread-local, not a global atomic.** The
  counting allocator in `benches/generation.rs` copies the design
  `crates/lodestone-fuzz/tests/length_prefix_allocation.rs` arrived at after two
  failures: a process-wide `AtomicU64` let one measurement absorb
  another thread's allocations, and adding a mutex *flaked*, because a lock only
  excludes code that takes it. A `const`-initialised `Cell` per thread needs no
  cooperation from anything.
- **A counter gate needs its own test binary.** Sharing one made
  `pre_ore_computed` read **502** against a true **256** — a 96% over-count that
  looks exactly like a broken store, because a second test in the same binary had
  already bumped the process-wide counters. This is why
  `crates/lodestone-worldgen/tests/engine_counters.rs` is a separate binary
  rather than another `#[test]` alongside its neighbours. When you add a counter
  assertion, give it its own binary or reset-and-own the counters for the whole
  binary; a shared one is a *duration*-species vacuity (the flaw is the test's
  lifetime against process-wide state, and reading the test cannot reveal it).
- **Counters-on inflates a burst by roughly 3×, so a counter run and a timing run
  must never be the same run.** This is now **enforced rather than remembered**:
  `benches/support.rs`'s `record()` refuses to write an absolute timing when
  `lodestone_worldgen::counters::enabled()`, printing a loud `REFUSING to record`
  line instead. A recorded poisoned timing is worse than a missing one, because
  `cargo xtask bench-compare` will cheerfully ratio it against a clean run and
  report a 3× "regression" that is pure instrumentation.

  The predicate is `support::timing_is_poisoned_by_counters(unit, enabled)`, keyed
  on **absolute** time units (`ns`/`us`/`µs`/`ms`/`s`) **and on units measuring work
  the process actually performed** (`instructions`/`cycles`, `WORK_PERFORMED_UNITS`).
  That second list is not a widening of the first — it is the distinction that
  decides where a new unit goes. `allocs`, `calls` and `draws` count *events in the
  pipeline*, and the counter hooks add none of those, so they are exempt. Retired
  instructions count every instruction the **process** executed, and a `bump` is a
  `fetch_add` plus a thread-local read at hundreds of thousands of sites per column,
  so an instruction count from a `gen-counters` build is inflated by the instrument
  for a *stronger* reason than a timing is. Ratio, count and size
  metrics — `stage_<name>_pct` (`%`), `linearity_ratio_vs_expected` (`x`),
  `calibration_*` (`calls`), `region_rss_*` (`bytes`) — still record under
  counters, deliberately: the stage split is one of the things you run *with*
  counters on, so blocking everything would defeat the purpose. Verified on a real
  `--features gen-counters --bench generation -- --test` run: **33 metrics
  refused, 18 still recorded**; of the 9 units in use, 3 blocked and 6 not. That
  is the check that the guard is neither vacuous nor total. **If you add a timing
  metric in a new unit, add it to `ABSOLUTE_TIME_UNITS`** — the guard is a table,
  and a unit missing from it fails open.

  The negative control matters as much as the guard: with counters **off** the
  same run refuses **0** and records **50**, so the guard is not simply always-on.
  (The counts do not sum across the two runs — `calibration_*` metrics only exist
  *with* counters, which is why the counters-on run attempts one more.)

  One honest caveat on the 3× figure: it is a **burst** phenomenon, and the guard
  is deliberately blanket because the inflation is metric-dependent and a bench
  cannot know which of its numbers is burst-shaped. Measured for the instruction
  counter specifically: `i_ss_median_instructions_per_column` reads 500,189,041 with
  counters on and 488,507,564 with them off, a 2.4% inflation — small, entirely
  systematic, and 10× the instrument's own 0.21% repeatability, which is exactly the
  size of thing a comparator exists to see. In the two runs above the
  steady-state `c_ss_median_interior_us` moved only ~1% (19416 → 19624 µs) — but
  those were **separate, non-concurrent processes under different machine load**,
  so per `CLAUDE.md` that ratio is a sample, not a measurement, and it is
  emphatically not evidence that counters are free for steady-state timings.

  Counting the units by grep is itself a trap worth knowing: metrics are recorded
  through **two** call shapes, an explicit `unit: "us"` field and a
  `("name", value, "us")` tuple loop, and grepping only the first reports 5 units
  when 9 are live — it misses `ns` and `bytes` entirely. The first pass at this
  guard's own non-vacuity check made exactly that mistake and got the right
  verdict from wrong numbers. Read the run.

### Which resolver a bench may use

`benches/generation.rs` has three generator constructors and they are not
interchangeable:

| constructor | data | valid for |
|---|---|---|
| `make_shape_only_generator` | fixture tree, 2 of 9 resolver methods | raw noise-router throughput only |
| `make_full_generator` | fixture tree, all 9 methods | single-biome composed benches — **`biome` and `top_layer` are structurally inert** |
| `make_embedded_generator` | `lodestone-server`'s embedded 26.2 data | C_ss / C_cold / calibration — all ten stages live |

The fixture tree is single-biome plains and carries no `block_freeze_facts`
document, so against it the biome search never runs and `freeze_top_layer`
early-returns. A "composed pipeline" number taken against it describes a pipeline
with two stages missing while looking entirely plausible. **The defence is
`assert_all_ten_stages_ran`, not a comment** — this file already carried the
comment, and it did not help.

### C_ss and C_cold

The two numbers the rewrite is measured against, defined in the plan's §Q3 so
they can be held to:

- **C_ss** — median `column()` over the **100 interior chunks of a 12×12 sweep**,
  single thread, release, embedded data, seed 42. The border chunk ring is
  excluded because its neighbours were computed *by it*, so it carries
  cold-neighbour cost.
- **I_ss** — the same 100 interior columns, in **instructions retired**. This is
  the acceptance criterion a later unit should be written in; see
  [`I_ss` above](#i_ss-instructions-retired-the-standing-beforeafter-comparator).
- **C_cold** — the first `column()` in a fresh region (25 pre-ore chunks, 9 ore
  walks from nothing, **and 441 chunks' structure starts** now that structure starts are
  computed too). Needs
  a **fresh generator**: the memo caches are per-generator, and reusing a warm one
  is the trap that neutered two determinism gates in this repo already.

The sweep also asserts, in both feature arms, that `store_evictions() == 0`. That is
not decoration: eviction during the sweep inflates every stage-computation count
below it, and it is what made `C_ss` read 79.9 ms against 21.0 ms for four months
without any counter saying so (DESIGN.md §12.130). **A memoisation-bearing benchmark
needs an eviction control, or its "exactly once" counters are upper bounds it never
checked.**

Run them:

```bash
# NOTE the --config: see "Configuration" below. Without it this ICEs on tokio.
cargo --config 'profile.release.package.tokio.opt-level=1' \
  bench -p lodestone-worldgen --features gen-counters --bench generation
```

Counters are **off by default** and every hook compiles to an empty
`#[inline(always)]` function without the feature, so a shipped build carries no
counter code. They are measurably slower when on, which means **a timing run and
a counter run are two different runs, never one.**

### The two-arm rule, for every unit that claims an improvement

A later unit's "we made it faster" is a before/after comparison, and this repo has
a recorded false signal from doing that sequentially: a 3/5-vs-0/5 result that was
1/6 on both arms once the arms were interleaved. So:

- **Run both arms interleaved in one process**, alternating per iteration — not
  arm A to completion then arm B. A machine that gets busy halfway through
  otherwise attributes the change to your diff.
- **Build a fresh generator per arm.** `OverworldGenerator` holds a 2,048-entry
  staged store, so a second arm on the same generator reads the first
  arm's cached results and agrees with itself no matter what. This already
  neutered two determinism gates here; `bench_stage_split`'s anti-drift control
  carries the same rule for the same reason.
- **Prefer a counter to a duration**, and when you must report a duration, report
  the counter beside it so the ratio is machine-independent. For whole-column work
  `I_ss` is now that counter and needs no spec-bound denominator to be comparable. The vegetation walk's
  headline figure is *ns per RNG draw* rather than ms per column precisely so a
  later unit can claim an improvement without reproducing this machine's state.
- **Re-run a timing-shaped result alone before believing it.** The U2 baseline was
  taken twice with no other CPU-consuming process and agreed within 2%; that
  agreement is what makes it a measurement rather than a sample.

## Status of specific sub-issues, re-verified rather than assumed

Per `CLAUDE.md`'s "re-verify before routing around 'X doesn't exist yet'"
rule, checked against the actual code rather than a prior comment's summary:

- **The worldgen stage-cost split is now done, and closing it found something
  worse than the missing fields.** The four-bucket `StageTimes` is now ten:
  aquifer / shape / biome / surface / materialize / carve / ore / vegetation /
  top_layer / intern. Two of the old four were misnamed — `fluid_heightmap` was
  the biome stage (the heightmap is computed inside the shape window) and
  `intern` was materialize + carve + ore + vegetation + interning, so the
  persisted `stage_intern_pct` attributed carvers and ores to "interning".

  The larger finding is that **the bench was measuring a pipeline with no
  carvers, ores or vegetation in it at all.** `Resolver` has nine methods;
  `benches/generation.rs`'s `FsResolver` implemented the two required ones and
  inherited `Value::Null` for `biome_document`, `configured_carver`,
  `configured_feature`, `placed_feature` and `block_tag`, which resolve to empty
  carver lists and empty feature steps — every one of those stages an early
  return. `bench_ore_composition_sweep` meanwhile asserted in its own doc comment
  that it "actually exercises `OverworldGenerator::ore_stage`" and warned that a
  resolver with no ore data would make it "an early-return no-op", while citing a
  `biome/plains.json` its resolver never opened. `CLAUDE.md`'s **world species**,
  in a file that names the trap.

  With the fixture actually resolved (`make_full_generator`), the composed split
  over 9 columns at seed 42, release, is **vegetation 61.8% / ore 18.1% / shape
  12.8%** / surface 2.2% / aquifer 2.2% / materialize 2.1% / carve 0.95%. The old
  "noise 40% / surface 51%" figure describes the *shape-only* generator, which is
  kept as `make_shape_only_generator` for the raw noise-router throughput numbers
  and now says so at every call site.

  The gate that stops this recurring is a **floor, not a ceiling**: seven stages
  must each measure >1000µs across the patch, so a re-inherited `Value::Null`
  fails loudly instead of producing a plausible percentage table for a pipeline
  with stages missing. `biome` and `top_layer` are excluded for stated reasons
  (single fixed biome; no `block_freeze_facts` fixture), printed in the output.
  `column_timed` is also asserted equal to `column()` block-for-block over a
  **fresh generator per arm** — the 512-entry memo cache would otherwise have the
  second arm agree with itself — with its own control proving the comparison can
  see a difference at all.
- **The lighting benchmark sub-issues are satisfied by `light_propagation.rs`**, which already:
  records the same functions the acceptance criterion names into `support::record` (substance, not
  the literal `tests/memory.rs` conversion the acceptance criterion
  describes — the sanity tests stay untouched, per that criterion's own
  instruction); reports the from-scratch cost at realistic edit rates
  (no incremental relight exists anywhere in the tree, confirmed by grep —
  the documented fallback); and writes down the negative
  finding (`Neighbourhood` is architecturally a fixed 3×3, confirmed at
  `lodestone-world/src/lighting.rs`, so there is no larger API to sweep
  against until that type changes shape). See that bench file's own module
  doc for the detail.
- **The remaining ask (thread-count sweep + in-benchmark parity)** is now in
  `lodestone-server/examples/bench_worldgen.rs`: a 1/2/4/8/workers/2×workers
  sweep reporting scaling efficiency, plus an FNV-1a fingerprint comparing
  serial vs. parallel output over a 3×3 chunk subset that `panic!`s on
  mismatch — the RNG-determinism break is actually gated on, not merely a
  speed number.

### The render/entity batch

Three new bench sites — `lodestone-render/benches/{meshing,render_submit}.rs`,
`lodestone-world/benches/session_rss.rs`, `lodestone-shell/benches/entity_tick.rs`
— all CPU-only except one adapter-gated occupancy bench. A later pass added a
fifth, `lodestone-shell/benches/render_submit.rs`, and two new `lodestone-render`
seams (`atlas_occupancy`, and `lodestone-shell`'s own
`RenderStats::terrain_camera_bind_group_switches`) — see the corrected note
below the table.

**The design rule this batch settled: prefer counts, and say so when you can't.**
Measured on this machine, two runs of the *identical* release binary minutes
apart: `mesh_simple` 202µs then 472µs, `mesh_models` 62µs then 125µs, a cold
21×21 build 2.38s then 4.74s. A 2.3× spread from concurrent load alone — while
every count (256 quads, 63 sections, the 14-vs-1024 merge control) was identical
across both runs. So:

| issue | gate | shape |
|---|---|---|
| #90/#91 | greedy ≤ simple quads, uniform-surface merge control, culling-ran check | count |
| #92 | job count for one arriving column identical in a 9×9 and a 21×21 world | count |
| #106 | 3 batches and 3 instance buffers at n = 10…5000 | count |
| #128 (`lodestone-render` half) | draw-list sizes; `regions.len() == drawable`, `visible == drawn` | count |
| #128 (`lodestone-shell` half) | `draw_calls == sections_drawn`; `terrain_camera_bind_group_switches <= 1` at radius 1/3/6 | count |
| #133 | draw-call/bind-group-switch counts above are the shape gate; CPU submit-time is a recorded baseline | count + duration (baseline) |
| #151 | healthy churn growth vs a deliberate-leak control | count (bytes) |
| #160 (mesh arena) | `live_allocations == resident_len()`, `used == 0` after unload | count |
| #160 (texture atlas) | `used_pixels > 0`, `fraction ∈ (0, 1]`, `total_pixels >= used_pixels` at n = 16/64/256/1024 sprites | count |
| #87/#97/#99 | — | duration, recorded baseline only |

**Which mesher is which** (two of the rows above name different meshers and a
bench aimed at the wrong one measures nothing): `--headless`/demo → `mesh_simple`, live terrain →
`mesh_models`, decided by `crates/lodestone-shell/src/mesher.rs`'s `mesh_one`
(`match classifier.models()`). The shell never calls `mesh_greedy`.
**One of those rows' premise is wrong**: `tests/world_mesher_bench.rs` does *not* exercise
`mesh_models` — it passes `greedy = true` into `build_batch` and lands in
`mesh_greedy` (`crates/lodestone-render/src/mesher.rs`'s `build_batch`).

**Another row's premise needed correcting too.** It asks to assert the remesh does not
touch interior sections; the per-load job set deliberately does not have that property
(`crates/lodestone-render/src/mesher.rs`'s `neighbour_columns` doc comment says callers
re-mesh whatever loaded sections fall in the 9 columns). Asserting it would report a
defect where there is a design choice, so the gate is the count-identity above instead.

**Closed by a later pass**, and worth recording exactly what closed since the
note above was wrong by the time it was re-read (`CLAUDE.md`'s "re-verify
before routing around 'X doesn't exist yet'" — `lodestone-shell` already had a
`benches/` directory, `entity_tick.rs`, so "no `benches/` directory" was stale
the moment it was checked, not merely optimistic):

- **Bind-group switches** are now counted:
  `RenderStats::terrain_camera_bind_group_switches`
  (`crates/lodestone-shell/src/gpu/stats.rs`), incremented by a small
  `bind_terrain_camera` helper (`gpu/frame.rs`) at every terrain group-0 bind
  site (the packed loop, `emit_terrain_draws` — called for both the opaque and
  water passes — and the moving-block/item/framed-map draws), by **pointer
  identity** rather than call count: a run of draws that all reuse the same
  `&wgpu::BindGroup` (differing only in dynamic offset, the cheap and expected
  case) contributes exactly one. This is deliberately narrower than "every
  `set_bind_group` call" (179 sites workspace-wide, most of them unrelated
  bind groups — entity camera, armour, block entities, …): this gate asks about the
  *terrain* camera bind group specifically, the exact thing an earlier fix addressed.
  `write_buffer` calls were **not** separately counted — the shared-uniform
  write is already exactly one per frame per path by construction
  (`update_model_shared_camera_buffer`), so a call-count gate there would
  measure a constant the code already asserts by its own shape, the code-reading
  substitution this gate explicitly forbids; the *bind-group* count is the one that
  can actually regress silently.
- **Texture-atlas occupancy**: `lodestone_render::atlas_occupancy` /
  `AtlasOccupancy` (`crates/lodestone-render/src/texture.rs`) — CPU-only,
  computed from `Atlas::width`/`height` and the same `sprite_rects` helper
  `GpuAtlas::from_rgba` already uses for mip isolation, so no GPU-side
  `used_area`/`slot_occupancy` accessor on `GpuAtlas` was needed after all: the
  CPU-side `Atlas` already carries everything the computation needs, and
  `GpuAtlas` is built from it at identical dimensions.
- **The draw-call/bind-group-switch/submit-time bench** is now built as `crates/lodestone-shell/benches/render_submit.rs`,
  through the four public wrappers (`RenderState::render`,
  `render_with_crack_and_effects`, …) exactly as this note originally
  proposed: `draw_calls == sections_drawn` and
  `terrain_camera_bind_group_switches <= 1` at radius 1/3/6 over the
  packed/demo path (up to ~4056 sections at radius 6, the same order of
  magnitude as an earlier `sections=3880` profile), with CPU submit time
  recorded as a provisional baseline. `MODEL_ORIGIN_ARENA_SLOTS` stays
  `pub(super)` — not widened — because the ceiling-headroom ask is answered by
  two new narrow accessors instead: `RenderState::model_origin_arena_stats`/
  `packed_origin_arena_stats`, returning the arena's own `AllocStats` (bytes
  used/capacity, live allocation count) without exposing the arena type or the
  constant itself.
- **The live-vanilla model path stayed out of scope.** All three of the above
  exercise the **packed/demo** path only (`RenderState::new(.., vanilla:
  None)`), which needs no `client.jar`. The model path needs
  `crate::resources::BlockResources::load(true)`, which degrades to `None`
  without one rather than failing — so a bench built against it would run
  differently in CI than on a machine with `.cache/mc/26.2` present. The
  packed path is not a stand-in invented for this gap either: it is the same
  path most recently fixed, so a reversal there is exactly what
  these gates are positioned to catch.

**The `LockHolds` axis is deliberately absent rather than faked.** Driving
`world.run_schedule(GameTick)` directly involves no guard, so a
`LockHolds::snapshot()` there reads zero holds and a gate on it would be green,
plausible and measuring nothing. That axis needs
`hold_write(&handle, |w| w.run_schedule(GameTick))` against a real `EcsHandle`,
which measures lock contention rather than per-system compute.

**One earlier note names a function that no longer exists.** `fold_entity_snapshots` was
deleted; the live replacement is `fold_entities`. The docs still referencing it
(`docs/world-unification.md`, `docs/entity-components.md`) are stale.

## How to change it

- **Add a bench to a crate the harness already covers** (`lodestone-worldgen`,
  `lodestone-v770`, `lodestone-world`, `lodestone-entity`, `lodestone-physics`,
  `lodestone-render`, `lodestone-shell`):
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
- **`lodestone-worldgen`'s `gen-counters` feature**: default **off**. Turns on
  `src/counters.rs`'s relaxed atomics. `cargo bench -p lodestone-worldgen
  --features gen-counters` is required for the calibration assertions and for the
  exactly-once invariant on C_ss; without it those benches **skip loudly** rather
  than asserting against zeros.
- **`--config 'profile.release.package.tokio.opt-level=1'` is currently required
  to run any release bench in this workspace.** `rustc 1.99.0-nightly
  (da86f4d07 2026-07-24)` ICEs compiling `tokio` 1.53.1 at `opt-level=3` when
  dev-dependencies enable `rt-multi-thread`
  (`rustc_codegen_ssa/src/mir/operand.rs:291: not immediate`). This is
  **pre-existing and unrelated to any bench** — `cargo build --release
  -p lodestone-server --tests` reproduces it — and it is invisible to every health
  check in `CLAUDE.md` because all of them are debug builds. The override cannot
  affect a measurement: no bench here executes tokio. See `DESIGN.md` §12.98; if a
  dated nightly is pinned, pin one that compiles tokio in release.

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
- `lodestone-server` — **dev-dependency of `lodestone-worldgen`, benches only**,
  for `overworld_generator`: the embedded 26.2 production data C_ss/C_cold are
  defined against. This is a dev-dependency *cycle* (`lodestone-server` depends on
  `lodestone-worldgen` normally), which Cargo supports, and it adds nothing to
  `lodestone-worldgen`'s lib — the wasm build and `cargo check -p lodestone-shell
  --no-default-features` are untouched. The alternative (pointing an `FsResolver`
  at `crates/lodestone-server/assets/worldgen/`) reproduces eight of the nine
  resolver methods and silently misses `block_freeze_facts`, which is built from
  `lodestone-data`'s jar dumps rather than from any JSON asset — so it would leave
  the `top_layer` stage inert while looking complete.
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
