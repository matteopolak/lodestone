# Join-stage tracing

## What it is

The optional join trace records the initial chunk timeline across the integrated
server and shell: generation, packet encoding, socket delivery, client receipt,
remesh scheduling, and completed mesh work. It is intended to distinguish a
world-generation stall from a network or renderer publication gap.

## How it works

Set `LODESTONE_JOIN_TRACE=1` and enable the `lodestone_join_trace` tracing target.
The server emits one event per chunk at `generated`, `encoded`, and `delivered`;
the shell emits `received`, `remesh_queued`, and `remeshed`. Each event includes
the chunk coordinate, elapsed milliseconds from that join's chunk phase, and a
`first` field identifying the time-to-first event for each stage. Payloads and
shader/source text are never included. Deferred native join generation gives the
connection loop a bounded 25 ms wait for its ordered head; cancelling that wait
leaves the head in the pipeline, so socket and timer work stays serviceable
without changing chunk order. The shell trace ends when CPU meshing
completes; GPU upload and presentation remain separate frame-profiler phases.

## How to change it

Keep stage names stable: capture scripts can group them directly. Add a stage at
the boundary that owns it, and keep the default path as `None`/disabled so normal
joins do not clone trace state into workers or read a clock per chunk. A trace is
diagnostic evidence only; it must not alter admission order, generation policy,
packet bytes, or mesh scheduling.

## Configuration

`LODESTONE_JOIN_TRACE=1` enables the trace on native builds. The tracing subscriber
must also accept the `lodestone_join_trace` target at `INFO`; for example, use
`RUST_LOG=lodestone_join_trace=info` alongside the flag. Browser builds leave the
trace disabled because they have no host environment configuration.

## Dependencies

The server trace uses `JoinStopwatch` and follows `ColumnPipeline` worker and
connection boundaries. The shell trace uses the portable `lodestone-time` clock,
`Sim::poll_net`, the terrain mesher, and the existing `tracing` subscriber.
