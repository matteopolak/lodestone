# Server tick clock

## What it is

The internal `lodestone_server::tick_clock` module owns the shared timing and accounting boundary
for the integrated server's world tick. It keeps tick duration, phase timing,
owner-handoff counts, and overload counters independent from simulation code.

## How it works

`TickClock` records one completed tick and bounded rolling histories for the
three coarse phases. Phase snapshots calculate percentiles and over-budget
counts; `stats()` combines those with MSPT, TPS, overrun, owner-work, and the
largest phase window. Atomic counters allow readers to inspect a live clock
while the tick task records work.

World simulation uses one anchored 50 ms schedule on native and Wasm. Ordinary
late service retains deadline phase; recovery yields after two overdue ticks
or eight milliseconds of work. Debt beyond two seconds is shed in whole tick
periods. Startup pause resets the next deadline, so time spent loading cannot
become damage, AI, or physics catch-up. Native sleeps against the runtime clock;
Wasm uses host macrotasks. Connection keep-alive and publication intervals retain
their separate delay behavior.

`TickStats::schedule` separates timer/executor service lateness from already-known
deadline debt. Service lateness is `resume - max(deadline, wait_requested)`,
clamped to zero. Its p95 covers the last `TICK_HISTORY_LEN` waits; service and
deadline maxima cover the session. Cumulative counters report active waits,
already-overdue admissions, cooperative recovery yields and shed ticks.
Recovery-yield duration has its own session maximum, so a delayed host
macrotask cannot disappear from telemetry when the subsequent wait is requested.
Paused waits are excluded. `overrun_count` counts shedding events even when
their warning is rate-limited. MSPT measures only simulation work, and `tps`
estimates work-budget capacity, not observed wall-time throughput.

The parent `tick` module re-exports the public types, so callers continue to
use `lodestone_server::{TickClock, TickStats, TickPhase}`. The clock has no
authority over when a phase runs and does not read or mutate world state.
`IntegratedServer::tick_monitor` gives a cloneable read-only view of this clock
and the world-tick witness. The native client profiling workload can retain it
after server construction without locking the tick-owned ECS world.

## How to change it

Add accounting fields and their snapshot values together in `TickClock` and
`TickStats`. Keep phase discriminants aligned with `TICK_PHASE_NAMES` and the
fixed-size arrays. Changes to what a phase includes belong at the timestamps
in `tick.rs`; do not move world or scheduled-tick operations into this module.
The pure `tick_deadline` policy owns debt and recovery bounds. Keep its injected
clock controls independent of platform sleep mechanics; the shared `TickDriver`
is their production consumer. Never apply world catch-up policy indiscriminately
to connection intervals.

## Configuration

`TICK_HISTORY_LEN` controls the rolling sample window. `MILLIS_PER_TICK` in the
parent tick module supplies the normal period, and `PHASE_SOFT_BUDGET` derives
the phase warning boundary from it.

## Dependencies

The module uses Rust atomics, mutexes, and bounded `VecDeque` histories. It is
consumed by `lodestone_server::tick` and indirectly by integrated and
dimension tick loops through the unchanged `TickClock` API.
