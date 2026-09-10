# Region owner execution

`lodestone_server::tick_region::run_bounded_owner_jobs` is the production
handoff boundary for work assigned to disjoint chunk owners. It lets immutable
owner snapshots overlap on bounded native lanes while preserving deterministic
central publication.

## What it is

`lodestone_server::tick_region::run_bounded_owner_jobs` is the production
handoff boundary for work assigned to disjoint chunk owners. Block-entity
non-hopper ticks and several entity simulations use it to execute immutable
owner snapshots without holding their shared registry lock.

## How it works

The caller snapshots each owner before dispatching it. The executor places jobs
on a bounded number of native lanes, waits for every lane, and restores the
submission order before returning results. A worker therefore produces only a
private completion; the caller remains responsible for validating the owner
and publishing effects centrally. The browser build keeps the same ordered
contract with a serial implementation because native threads are unavailable.

The cross-region executor gate uses two chunk owners on opposite sides of the
origin and a barrier inside both jobs. It proves that the native path actually
overlaps two owners, while its result assertion proves completion timing cannot
change deterministic publication order.

## How to change it

Keep jobs self-contained: clone or otherwise snapshot every value needed by a
worker before calling `run_bounded_owner_jobs`. Do not capture a registry guard,
world writer, or another owner’s mutable state. If a new result can publish
world-visible state, add a central merge step that validates a complete,
duplicate-free owner set and restores the tick-start sequence.

The lane count is a bound, not a promise that every call is parallel. A call
with one job has one useful lane; callers should choose their worker count from
the measured owner workload. Tests that claim concurrency must use a
barrier-controlled fixture so a serial fallback cannot pass accidentally.

## Configuration

`run_bounded_owner_jobs` receives its lane bound from each subsystem. Native
callers commonly derive it from `std::thread::available_parallelism` and cap it
at a small fixed value; wasm always uses one lane. No environment variable
changes the ownership contract.

## Dependencies

The executor is part of `lodestone-server` and is consumed by the block-entity
registry, mob simulations, and the tick loop's central publication phases. Its
ordering guarantees complement [`tick-region-ownership`](./tick-region-ownership.md)
and the scheduled/block-entity handoff rules in
[`tick-scheduling`](./tick-scheduling.md).
