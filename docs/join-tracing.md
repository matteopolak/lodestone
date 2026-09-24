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
`first` field identifying the time-to-first event for each stage. Client receipt
and queue counts are sampled every 16 events; completed meshes are counted per
section and sampled every 256 events on both native and browser builds. Payloads and
shader/source text are never included. Deferred native join generation gives the
connection loop a bounded 25 ms wait for its ordered head; cancelling that wait
leaves the head in the pipeline, so socket and timer work stays serviceable
without changing chunk order. The shell trace ends when CPU meshing
completes; GPU upload and presentation remain separate frame-profiler phases.
An integrated connection error is logged at the server task boundary before
that task closes its transport. Without that error, the client's subsequent
write can report only `broken pipe`, which does not identify the failed stage.
When tick-driven block updates hold the connection loop for at least 200 ms,
the stall log includes the number of changed blocks, relit columns, and time
spent in lighting. This separates update fan-out from generation delay.

## How to change it

Keep stage names stable: capture scripts can group them directly. Add a stage at
the boundary that owns it, and keep the default path as `None`/disabled so normal
joins do not clone trace state into workers or read a clock per chunk. A trace is
diagnostic evidence only; it must not alter admission order, generation policy,
packet bytes, or mesh scheduling.

## Configuration

`LODESTONE_JOIN_TRACE=1` enables the sampled trace on native builds. The tracing subscriber
must also accept the `lodestone_join_trace` target at `INFO`; for example, use
`RUST_LOG=lodestone_join_trace=info` alongside the flag.
`LODESTONE_WORLDGEN_LEDGER_TRACE=1` adds a failure-only comparison of published and retained
stage records when a generation checkpoint is rejected. It is native-only and can be enabled
alongside the join trace to find the failure behind a disconnected join.

Browser builds use the same monotonic clock and expose bounded operational diagnostics through the SDK's
`logLevel` mount option. Use `mount({ ..., logLevel: "debug" })`, or append
`?log=debug` to the standalone runner. The console then reports spawn-search
duration and work counts, each worldgen session transition with elapsed time
from worker launch, join-stream progress, the play-loop heartbeat, and world-tick
phases that exceed one 50 ms tick period. It also records the first column and
each sixteenth column at packet receipt and mesh admission, plus each 256th completed section mesh,
so server generation and client rendering stalls can be separated without a
per-column console flood.
Supported levels are `off`, `error`, `warn`, `info`, `debug`, and `trace`; the
default is `warn`.

## Dependencies

The server trace uses `JoinStopwatch` and follows `ColumnPipeline` worker and
connection boundaries. The shell trace uses the portable `lodestone-time` clock,
`Sim::poll_net`, the terrain mesher, and the existing `tracing` subscriber.
