# Tick-loop and worldgen per-phase profiling

## What it is

Per-phase timing for [`run_tick_loop`](../crates/lodestone-server/src/tick.rs)
(the server's 20 Hz world tick) and per-stage duration percentiles for
[`OverworldGenerator::column`](../crates/lodestone-worldgen/src/overworld/mod.rs)
(the overworld generation pipeline) — an answer to "which parts of the tick
loop and worldgen should we improve on", built to measure the **tail**, not
the mean, after this repo's own record of a keep-alive timeout once being
diagnosed from an average that hid the one window that actually mattered
(`DESIGN.md` §12, the "measure the tail" incident cited throughout
`CLAUDE.md`). The SIMD half of that original request is explicitly
downstream of this: see ["Where SIMD would pay next"](#where-simd-would-pay-next)
for a recommendation, not an implementation.

Two independent instruments, one per crate:

- **`lodestone-server`'s `tick.rs`**: `TickClock` grew three
  [`TickPhase`](../crates/lodestone-server/src/tick.rs) buckets
  (`MobsAndItems`, `WeatherAndSleep`, `ScheduledAndPhysics`), each with a
  rolling percentile history, an over-budget counter, and a global
  worst-single-phase-ever tracker that names which phase and which tick.
- **`lodestone-worldgen`'s `src/profile.rs`**: aggregates
  `OverworldGenerator::column_timed`'s existing ten-stage split
  (`StageTimes` — see `docs/benchmark-harness.md`'s `generation` bench for
  where that split itself comes from) over a batch of columns into
  per-stage percentiles, a dominant-stage ranking, and a worst
  single-stage-and-column tracker.

Both are `#[cfg(...)]`-gated where wasm needs it (see
["Configuration"](#configuration)) and both are documented and tested
independently — they do not share code, only a percentile formula
(nearest-rank) and a design shape (percentiles + a named worst window +
a counter where a counter suffices).

## How it works

### Tick loop: three phases, chosen for lock safety

`run_tick_loop`'s body is roughly 1,000 lines. The back two-thirds of it —
scheduled block-tick draining, fire/redstone/fluid propagation, random
ticks, falling blocks, vehicles, TNT, minecarts, dragons — all run inside
one `scheduled.with(|queues| { … })` closure, which holds the
scheduled-tick queue's mutex across its whole extent (see that closure's
own doc comment in `tick.rs`, and `CLAUDE.md`'s record of the self-deadlock
a re-entrant call into that same mutex caused elsewhere in this crate). A
phase boundary is a bare `tokio::time::Instant::now()` call with **no lock
held and nothing called back into `scheduled`**, so it cannot deadlock —
but *scattering* several of them through that closure's ~1,000 contested
lines, while another agent's mob/redstone work may be editing the same
region concurrently, is a collision risk this pass chose not to take just
to get a finer split. So the three phases are:

| phase | covers | boundary risk |
|---|---|---|
| `MobsAndItems` | border tick, item settling, mob removal/spawn/despawn, patrols, traders, detonations, drops, grazes, vocalisations, projectile hits, spawner blocks | low — timestamp sits between two loop-local sections |
| `WeatherAndSleep` | weather cycle, night-skip vote | low — same |
| `ScheduledAndPhysics` | **everything inside `scheduled.with`**: block-tick drain, fire/redstone/fluid, random ticks, falling blocks, vehicles, TNT, minecarts, dragons, and the only `world.column()` calls this loop makes | none — measured from *outside* the closure, before it opens and after it returns |

`ScheduledAndPhysics` is the phase a keep-alive-timeout-shaped stall would
show up in, because it is the only one that can call `world.column()` (a
scheduled block tick that crosses a chunk boundary can trigger real
worldgen) — the exact mechanism behind the incident `CLAUDE.md` records
under "measure the tail". A future pass wanting a finer split of that one
phase should do it from *inside* `scheduled.with`, once no other agent
holds `tick.rs`, rather than widening the lock-safety argument above.

Each phase records:

- **A duration**, into a 100-sample rolling ring buffer (same cap and shape
  as `TickClock`'s existing MSPT history) — because latency percentiles are
  fundamentally a duration question and there is no counter that answers
  "how long did this take". `TickClock::phase_stats` sorts a clone of the
  buffer and returns p50/p95/p99/max in milliseconds, plus the over-budget
  count below. This is the one place this instrument records a duration
  rather than a counter, per this repo's own "prefer a counter over a
  duration wherever the property allows it" rule — the property does not
  allow it here, so the distribution is recorded (not a single mean), as
  the same rule asks for when a duration is unavoidable.
- **A counter**: `phase_over_budget[phase]`, an `AtomicU64` incremented
  whenever that phase's single-tick duration exceeds `PHASE_SOFT_BUDGET`
  (10ms — 20% of the 50ms tick period, one threshold shared by all three
  phases rather than three untuned guesses; see the constant's own doc for
  why a shared, honestly-coarse number beats an invented per-phase one).
  This is the counter this instrument leans on: cheap, exact, and — unlike
  a duration — as meaningful after a billion ticks as after ten, with no
  rolling-window truncation to reason about.
- **A global worst-window update**: `worst_phase`, a `Mutex<Option<WorstPhaseWindow>>`
  holding the single largest phase duration ever recorded, which phase it
  was, and (approximately) which tick — the "worst unserviced window,
  named" the brief for this work asked for, and the reason a rolling
  100-sample window is not the whole story: it forgets anything older than
  100 ticks, and the worst tick in a server's whole session might not be
  recent.

### Worldgen: percentiles over `column_timed`, split from the I/O that drives it

`crates/lodestone-worldgen/src/profile.rs` has two halves:

- `aggregate_stage_samples` — pure. Takes `&[((i32, i32), StageTimes)]` it
  had no part in producing and returns a `StageDistribution`: per-stage
  p50/p95/p99/max/total, a `dominant_stage()` ranked by cumulative cost
  (`total_us`, not any single peak), and the single worst
  stage-and-column pair (`WorstStageWindow`).
- `profile_columns` — the thin I/O wrapper: calls
  `OverworldGenerator::column_timed(cx, cz)` once per requested coordinate,
  discards the generated column, and hands the ten `StageTimes` fields to
  `aggregate_stage_samples`.

The split matters for testability: `src/profile.rs`'s own unit tests drive
`aggregate_stage_samples` with hand-built `StageTimes` values and a
hand-derived nearest-rank expectation, needing no generator, no `Resolver`
fixture, and no timing of any kind. `tests/profile_columns_report.rs` is
the one place that drives a *real* generator (the same
`tests/support/worldgen_data` fixture tree `benches/generation.rs` and
`tests/overworld_gen.rs` already use, with every fixture file loaded —
`full: true` — because a shape-only resolver makes carve/ore/vegetation/
top_layer all early-return, which is the exact "world" species of vacuous
benchmark `benches/generation.rs`'s own `make_shape_only_generator` doc
comment records having shipped once already).

`column_timed` deliberately bypasses `OverworldGenerator`'s own
`pre_ore`/`post_ore` caches (see its own doc comment), so every profiled
column pays the full cache-cold cost on purpose — a per-stage split over
memoised calls would attribute ~0% to whichever stage happened to be warm.

## Validating the instruments

Per this repo's evidence standards, an instrument is only as good as the
control that proves it is measuring what it claims to. Both instruments
ship one:

- **Tick loop — an idle world, deterministically.** `run_tick_loop`'s own
  no-players/no-mobs default wrapper (`EmptyWorld`, the empty tick area
  `world_tick_args()` already uses for the existing overrun tests) cannot
  physically do any of the work a phase boundary measures. Rather than
  asserting "small" under real wall-clock time — which this repo's own
  hazards warn is attributable to machine load on a shared checkout, not to
  the code — `phase_durations_on_an_idle_world_are_exactly_zero_under_paused_time`
  runs the idle loop under `tokio::time::pause`, where `Instant::now()`
  cannot advance without an explicit `.advance()` and the tick body has no
  `.await` in it at all (verified, not assumed — see the closure's own doc
  comment). So every phase, every tick, must read **exactly** zero, not
  "small". An instrument whose boundary accidentally spanned the
  top-of-loop `sleep_until` wait, or that leaked a previous tick's
  timestamp into this one, would show up here as a large, nonzero reading —
  the same shape of control as a pure camera rotation revealing the
  `vram_bytes` mis-attribution `CLAUDE.md`'s evidence standards cite: an
  input that cannot physically move the quantity must not move it. This is
  deterministic rather than a wall-clock timing, so — unlike the general
  "idle world" control the brief for this work describes — it is immune to
  this machine's load.
- **Worldgen — an independent counter cross-check.** `profile_columns` is a
  duration-only instrument, and a duration-only instrument cannot tell "the
  loop ran once" from "the loop ran ten times and nine were free" — both
  look like one small number. `profile_columns_with_counter_check` (behind
  the `gen-counters` feature) resets and re-reads
  `lodestone_worldgen_core::counters`, an independent, already-tested
  instrument, around the same batch: `Stage::Intern`'s `StageGuard::enter`
  runs from exactly one call site
  (`OverworldGenerator::intern_from_dense`), reached exactly once per
  top-level `column`/`column_timed` call and never from the neighbour-chunk
  recursion inside `ore_stage`/`vegetation_stage`. So after `reset()`,
  profiling `N` columns must leave `stage_entered[Stage::Intern]` at
  exactly `N` — `profiling_matches_the_gen_counters_intern_count` asserts
  it. Disagreement would mean the aggregation loop skipped, doubled, or
  deduplicated a coordinate, not that generation itself is wrong. The
  counter and the percentile aggregator share no code path other than
  `column_timed` itself, so this is a real cross-instrument check, not a
  self-referential one.

### Neuter results (both instruments)

Both controls were run against a deliberately broken build and confirmed to
go red, then the build was restored from an md5-checked backup (never
`git checkout`):

- `tick.rs`: `record_phase` was changed to add a spurious extra millisecond
  to every recorded duration (simulating a boundary that leaked part of the
  `sleep_until` wait). Of the six new `tick.rs` tests, three went red as
  predicted — the idle-world control, the hand-derived percentile test, and
  the worst-window test — while the two tests whose specific chosen values
  happen not to cross a 1ms-shifted threshold correctly stayed green (not a
  blind spot: the shift was verified by hand not to flip any of their
  boundary values). Restored; all six pass again, sha unaffected (this pass
  made no commit with the neuter in it).
- `profile.rs`: `percentile` was changed to always return the smallest
  sample regardless of `p` (simulating a rank computation that never
  advances past the first element). Of the five `profile.rs` unit tests,
  two went red as predicted — the hand-derived percentile test and the
  aggregate-batch test, both of which assert a specific non-minimum
  percentile value — while the three structural tests (empty-slice, empty
  batch, stage-name coverage) correctly stayed green, since none of them
  assert a percentile value the neuter could touch. Restored; all five pass
  again, plus the real-generator report test and the `gen-counters`
  cross-check test, both re-verified green afterward.
## How to change it

- **Add a tick phase**: widen `TickPhase`, `TICK_PHASE_COUNT`, and
  `TICK_PHASE_NAMES` together (a test —
  `tick_phase_names_cover_every_phase_in_discriminant_order` — pins them
  staying in sync), add the array literal fields to `TickClock`'s
  `phase_history`/`phase_over_budget` construction (both already generic
  over `TICK_PHASE_COUNT` via `std::array::from_fn`/`[const { .. }; N]`, so
  no further edit needed there), and add the new boundary's
  `Instant::now()` + `record_phase` call pair at a clean section transition
  — never inside `scheduled.with`'s closure without first re-reading
  "Tick loop: three phases, chosen for lock safety" above.
- **Add a worldgen stage**: widen `StageTimes` (in `overworld/output.rs`,
  which this pass did not touch) and, together, `WORLDGEN_STAGE_NAMES` and
  `stage_micros` in `src/profile.rs` — they must stay the same length and
  order as `StageTimes`'s own fields, the same discipline
  `lodestone-worldgen-core`'s `STAGE_NAMES` already carries for `Stage`.
- **Change the soft budget**: `PHASE_SOFT_BUDGET` in `tick.rs` is a single
  constant (10ms) shared by all three phases, chosen because nothing in
  this pass established a real per-phase budget from a loaded server — see
  its own doc comment before replacing it with three separate, unjustified
  numbers.
- **Get real worldgen numbers again**: `cargo test -p lodestone-worldgen
  --test profile_columns_report -- --nocapture`, and read the
  `PROFILE_COLUMNS_REPORT` lines. With the `gen-counters` feature also on
  (`--features gen-counters`), the cross-check test runs too.

## Configuration

- **`tick.rs`'s instrumentation is unconditional** — no feature flag, no
  `cfg`. Cost: three `Instant::now()` calls and three small, uncontended
  mutex-guarded ring-buffer pushes per tick (the same shape and cost class
  as the MSPT history this crate already recorded on every tick before this
  pass), plus one atomic compare-and-maybe-store for the worst-window
  tracker. Not separately measured in this pass, but bounded above by the
  cost of the *existing* `record_tick` call this instrumentation is a
  straightforward multiple of.
- **`lodestone-worldgen/src/profile.rs`** is
  `#![cfg(not(target_arch = "wasm32"))]`, matching `column_timed`'s own
  gate: wall-clock timing has no meaning under wasm, and
  `lodestone_time::Instant::now()` panics on bare
  `wasm32-unknown-unknown`. This module does no timing of its own — it
  only aggregates `column_timed`'s already-gated output — so it compiles to
  nothing on that target rather than needing its own confinement rule in
  `scripts/wasm-check.sh`.
- **`gen-counters`** (existing `lodestone-worldgen`/`lodestone-worldgen-core`
  feature, default off) gates only `profile_columns_with_counter_check` and
  its test — the validation control, not the profiler itself.  Turning it
  on adds atomics to the worldgen hot path crate-wide (see
  `lodestone_worldgen_core::counters`'s own module doc for the cost), so it
  is a `--features gen-counters` opt-in for verification, never a default.

## Dependencies

- `tick.rs`'s phases depend on nothing new — `tokio::time::Instant`,
  already used and already allow-listed for this file in
  `scripts/wasm-check.sh`'s `lodestone-server tokio-instant-ban` rule.
- `profile.rs` depends on `overworld::{OverworldGenerator, StageTimes}`
  (same crate) and, behind `gen-counters`, `lodestone_worldgen_core::counters`.
  `tests/profile_columns_report.rs` additionally depends on the
  `tests/support/worldgen_data` fixture tree already used by
  `tests/overworld_gen.rs` and `benches/generation.rs`.

## Where time actually goes

**Machine-state caveat, read first.** This is a shared checkout with a live
multi-agent swarm (`CLAUDE.md`'s own operational notes describe exactly
this). `pgrep -l 'rustc|cargo'` showed 2-3 other `cargo`/`rustc` processes
running at every point during this pass, including at the moment both runs
below were captured — never a fully quiet machine. That means the absolute
microsecond/millisecond figures below carry real scheduling jitter from
concurrent builds and should not be treated as precise; the **relative
ranking** between stages/phases is far more trustworthy than any single
number, because every stage in the same run shares the same jitter. Re-run
both commands below in a quiet window (`pgrep -l 'rustc|cargo'` returning
nothing) before using a specific figure for a decision.

### Worldgen: `cargo test -p lodestone-worldgen --test profile_columns_report -- --nocapture`

A 3×3 patch (9 fresh, cache-cold columns; seed 42, `minecraft:plains`
fallback biome, the full fixture-backed `Resolver` `benches/generation.rs`
also uses), working tree based on `2d0c4cbc` (`git rev-parse HEAD`, read in
the same shell invocation as the run — this pass's own changes are
uncommitted on top of it):

| stage | p50 (µs) | p95/p99 (µs) | max (µs) | total (µs) | share of total |
|---|---:|---:|---:|---:|---:|
| **vegetation** | 77,820 | 241,723 | 241,723 | 721,239 | **~68%** |
| **ore** | 4,616 | 98,277 | 98,277 | 137,728 | ~13% |
| shape | 7,719 | 19,939 | 19,939 | 87,785 | ~8% |
| aquifer | 2,064 | 4,691 | 4,691 | 22,337 | ~2% |
| surface | 1,072 | 5,125 | 5,125 | 15,067 | ~1% |
| materialize | 1,020 | 2,255 | 2,255 | 12,127 | ~1% |
| carve | 706 | 1,745 | 1,745 | 9,399 | ~1% |
| intern | 5 | 23 | 23 | 68 | ~0% |
| biome | 2 | 3 | 3 | 15 | ~0% |
| top_layer | 0 | 0 | 0 | 0 | 0% |

**Vegetation dominates, ore is second, everything else is noise** — matches
this repo's own prior finding that ore alone was ~38.7% of a steady-state
column (`DESIGN.md` §12.143, before vegetation had its own dedicated
timing bucket) and is consistent with vegetation being the more expensive
of the two once measured directly. `top_layer` reading exactly zero across
all 9 columns is expected, not a bug: `docs/benchmark-harness.md` records
that `top_layer`'s freeze predicates come from `lodestone-data`'s jar
dumps, which this fixture-only `Resolver` does not supply — the
`make_embedded_generator`/production path would show it as non-zero (and
`docs/plans/worldgen-parity.md` §6 predicts under 5% even then).

The `p95`/`p99`/`max` columns being identical for every stage is an
artefact of the sample size (9 < 20, so `ceil(0.95*9)=9` and
`ceil(0.99*9)=9` both land on the same, largest sample — see
`profile.rs`'s own hand-derived percentile test for the arithmetic) — not
evidence the distribution has no tail. A batch of 50+ columns would
separate them; 9 was chosen to keep this test fast enough to run on every
`cargo test`, not to make a tail claim.

`ore`'s p50/p95 gap (4.6ms vs 98ms) is the more interesting shape: one
column paid a much larger ore-placement cost than the rest (the 3×3
neighbour-driver's RNG walk is data-dependent — a chunk near an ore vein
boundary walks more candidates than one that does not). That one column
also produced this batch's worst single sample:
**`worst_stage=vegetation worst_us=241,723 worst_chunk=(0, 0)`** — the
origin chunk, unsurprisingly the most decorated in a `minecraft:plains`
fixture set.

### Tick loop, idle-world floor cost: `cargo test -p lodestone-server --lib -- --nocapture tick::tests::phase_durations_floor_cost_on_an_idle_world_under_real_time`

This is **not** a loaded-server measurement — no live oracle was stood up
for this pass (see "Non-goals" in the tracking issue). It is the
unavoidable per-phase floor on real hardware with an `EmptyWorld` and no
players/mobs, i.e. the cost of the loop *existing*, which any real
workload adds on top of:

| phase | p50 (ms) | p95/p99 (ms) | max (ms) | over-budget count |
|---|---:|---:|---:|---:|
| `mobs_and_items` | 0.039 | 0.378 | 0.378 | 0 |
| `weather_and_sleep` | 0.003 | 0.051 | 0.051 | 0 |
| `scheduled_and_physics` | 0.007 | 0.032 | 0.032 | 0 |

Worst single sample: `worst_phase=mobs_and_items worst_us=378 at_tick=0` —
tick 0's warm-up cost (first spawn-cycle/despawn-pass allocation), not a
steady-state figure; every later tick was well under that. All three
phases are two to three orders of magnitude below `PHASE_SOFT_BUDGET`
(10ms) on an idle world, which is the expected shape: the budget exists to
catch real work, and there is none here to catch.

**What this does *not* show**: which phase dominates under a real,
populated world — that needs a live oracle
(`scripts/live-oracles/{creative,survival,terrain}.sh`) run with players
connected and `clock.phase_stats`/`clock.worst_phase_window` read off a
live `TickClock`, which this pass did not do. Given `ScheduledAndPhysics`
is the only phase that can call `world.column()`, it is the one to watch
first once a live reading exists — the structural argument in "Tick loop:
three phases" above, not yet a measured one.

## Where SIMD would pay next

Not implemented in this pass — the task this instrumentation was built for
is explicit that the SIMD half is downstream of the measurement. Based on
what this pass actually measured (see "Where time actually goes" above,
with its machine-load caveat):

- **The measured dominant stage, `vegetation` (~68% of total, `ore` a
  distant-but-real second at ~13%), is a poor SIMD target, and widening
  SIMD there would very likely be optimising the wrong axis.** Both stages
  are placement-RNG-bound: per-feature/per-block decisions driven by a
  scalar `WorldgenRandom` stream (tree/grass/ore placement, jigsaw-style
  branching), not a dense numeric loop over a fixed-shape buffer — exactly
  the shape SIMD does not help, and `ore`'s own p50/p95 gap in the table
  above (4.6ms vs 98ms on one column) points at a *branchy, data-dependent*
  cost (how many candidates one chunk's neighbourhood walks), not a
  throughput-bound one a wider vector would shrink.
- `lodestone-worldgen-core::noise` already has a real `std::simd` kernel
  (`noise_corner_batches` in `lodestone_worldgen_core::counters`, documented
  in `docs/worldgen-simd-kernels.md`) covering the gradient-noise inner
  loop, which lives inside `shape` (~8% of this batch's total). `shape` is
  a legitimate SIMD candidate in principle — it *is* the existing kernel's
  own call site — but at ~8% of a `vegetation`-dominated column, widening
  its coverage caps out well below what `vegetation`/`ore` cost even in
  the best case. The honest ordering, from this measurement: **profile
  `vegetation`'s and `ore`'s own RNG-draw counts first** (`rng_draws` per
  `Stage` in `lodestone_worldgen_core::counters`, already tracked, already
  the D1-style hot path this crate's own counters module was built to
  watch) to find out whether their cost is dominated by *draw count* (an
  algorithmic question, not a SIMD one) or by *per-draw* cost (where a
  vectorised `WorldgenRandom` batch could help, if vanilla's own algorithm
  allows drawing ahead — unverified in this pass) before spending any SIMD
  effort on either.
- The tick loop's `ScheduledAndPhysics` phase is redstone/fluid/physics
  logic over sparse, mutating per-block state (queues, neighbour lookups,
  entity AABBs) — not a dense numeric loop over a fixed-shape buffer, so it
  is a poor SIMD candidate on its own terms regardless of how large its
  share of the tick turns out to be. If it dominates (plausible: it is the
  only phase that can trigger worldgen), the lever is the *worldgen* side
  above, not vectorising the redstone/physics code itself — the same
  “SIMD does not fix a latency defect” shape as this repo's own recorded
  keep-alive-timeout incident, where `spawn_blocking` moved work off the
  core thread without shortening the suspension point it was inside.
