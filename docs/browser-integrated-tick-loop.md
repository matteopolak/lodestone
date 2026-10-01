# Browser integrated tick loop

## What it is

The browser integrated server runs the same authoritative world simulation tick as the native server at 20 ticks per second. A browser-compatible timer supplies the scheduling boundary while the world source, scheduled queues, block-entity registry, and entity source remain shared with the connection.

## How it works

`IntegratedServer::open_in_memory_with_items_and_commands` starts the primary tick future for browser singleplayer and passes the same handles to the connection task. `run_primary_tick_loop_with_weather` executes the complete world tick body: scheduled block and fluid work, block entities, random updates, entity physics, mob simulation, and publication of world snapshots and feeds.

`TickDriver` uses the same anchored 50 ms `TickSchedule` on native and browser targets. Ordinary lateness retains the deadline phase; recovery yields after two overdue ticks or eight milliseconds of work. Debt beyond two seconds is shed in whole tick periods. A startup pause resets the next deadline, preventing loading time from becoming simulation catch-up. Browser waits and recovery yields use host macrotasks. See [Server tick clock](tick-clock.md) for scheduling telemetry and controls.

The timer must work in both browser contexts used by the shell. The page context exposes a `Window`, while the dedicated server worker exposes a worker global. A portable sleep implementation must resolve `setTimeout` from the active global rather than assuming that a `Window` exists.

Simulation publication is separate from simulation mutation. After a world tick changes a block, scheduled queue, or entity snapshot, the connection loop must drain the corresponding feed and run its streaming diff even when the client has sent no packet. Otherwise an idle client can retain stale water, item entities, or mob positions until its next input packet.

The browser connection timer continues publishing world changes during an initial join, but does not advance that player's vitals until the client sends `PlayerLoaded` after a ready frame is presented. The shared `player_tick_ready` gate also releases the world simulation's initial tick hold. Both connection loops use that gate, preventing air supply or health from changing while the terrain screen covers the new world. A late timer callback never replays skipped vitals ticks.

The shared `tick_deadline` tests cover anchored late service, bounded recovery,
debt shedding, and pause resets. `browser_timer` tests cover the separate delay
policy used by connection intervals. Integrated fluid controls include
`integrated_server_generated_fluid_seed_reaches_live_tick_loop` and
`integrated_server_fluid_tick_crosses_into_its_next_chunk_owner`;
`integrated_item_tick::integrated_tick_loop_advances_a_live_dropped_item` covers
item motion and lifecycle counters.

`just wasm-check` checks browser compilation, confinement, and worker control
tests. For rendering and transport acceptance, use `just run-wasm` and the
ordinary singleplayer flow: verify placed-water progression and a mined item's
motion in the rendered world. The [browser worker](browser-worldgen-worker.md)
document describes the `?probe=1` movement and mining controls and tick-health
diagnostics. Native tests and worker control tests do not establish those
visible outcomes.

## How to change it

Keep the tick body target-independent; target-specific code belongs at the timer boundary. Change world scheduling in `tick_deadline` and update its injected-clock tests with inputs that distinguish anchored recovery from delay semantics. Preserve the recovery work bounds, debt-shedding threshold, and pause-reset controls. Connection interval changes belong in `browser_timer` and its deadline tests; do not apply world catch-up behavior to player vitals or publication intervals.

When adding a browser-produced feed, decide whether it is consumed by the world tick or by the connection publication pass. The producer and consumer must share the same authoritative handle, and the consumer must be reachable from a timer arm as well as from packet dispatch. Do not add a second mutable world or a connection-local simulation loop.

## Configuration

The world period is fixed at 50 ms through `tick::MILLIS_PER_TICK`. `tick_deadline` owns the recovery limits: `RECOVERY_TICKS` is two, `RECOVERY_BUDGET` is eight milliseconds, and `MAX_DEBT` is two seconds. The browser timer does not require environment variables. Worker startup and the single-thread fallback are selected by the shell launch path.

## Dependencies

The loop depends on `lodestone-server` world, scheduled-tick, block-entity, and entity-feed handles; `lodestone-time` for a wasm-safe monotonic clock; Tokio task primitives; and browser `setTimeout` bindings supplied by `js-sys`/`web-sys` and `wasm-bindgen-futures`. The worker transport remains a shell concern and must carry the same server byte stream without moving world authority to the page.
