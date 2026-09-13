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

The parent `tick` module re-exports the public types, so callers continue to
use `lodestone_server::{TickClock, TickStats, TickPhase}`. The clock has no
authority over when a phase runs and does not read or mutate world state.

## How to change it

Add accounting fields and their snapshot values together in `TickClock` and
`TickStats`. Keep phase discriminants aligned with `TICK_PHASE_NAMES` and the
fixed-size arrays. Changes to what a phase includes belong at the timestamps
in `tick.rs`; do not move world or scheduled-tick operations into this module.

## Configuration

`TICK_HISTORY_LEN` controls the rolling sample window. `MILLIS_PER_TICK` in the
parent tick module supplies the normal period, and `PHASE_SOFT_BUDGET` derives
the phase warning boundary from it.

## Dependencies

The module uses Rust atomics, mutexes, and bounded `VecDeque` histories. It is
consumed by `lodestone_server::tick` and indirectly by integrated and
dimension tick loops through the unchanged `TickClock` API.
