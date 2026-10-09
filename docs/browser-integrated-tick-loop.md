# Browser integrated tick loop

## What it is

The browser integrated server runs the same authoritative world tick as the native server at 20 ticks per second, with a browser-compatible timer at the scheduling boundary and the world source, scheduled queues, block-entity registry and entity source shared with the connection.

## How it works

- `IntegratedServer::open_in_memory_with_items_and_commands` starts the primary tick future for browser singleplayer and hands the same handles to the connection task. `run_primary_tick_loop_with_weather` runs the full tick body: scheduled block and fluid work, block entities, random updates, entity physics, mob simulation, and publication of snapshots and feeds.
- `TickDriver` uses the same anchored 50 ms `TickSchedule` everywhere: ordinary lateness keeps the deadline phase, recovery yields after two overdue ticks or 8 ms of work, debt beyond two seconds is shed in whole periods, and a startup pause resets the deadline so loading is not simulation catch-up. Browser waits and recovery yields use host macrotasks ([tick-clock](tick-clock.md) for telemetry).
- The timer must work in both contexts: the page exposes a `Window`, the dedicated server worker a worker global, so the sleep resolves `setTimeout` from the active global.
- Publication is separate from mutation: after a world tick changes a block, scheduled queue or entity snapshot, the connection loop must drain the feed and run its streaming diff even with no packet from the client, or an idle client keeps stale water, items and mobs.
- The connection timer publishes world changes during initial join but does not advance the player's vitals until `PlayerLoaded` after a ready frame is presented; the shared `player_tick_ready` gate also releases the world's initial tick hold in both connection loops, so air and health do not change under the terrain screen. A late timer never replays skipped vitals ticks.
- Tests: `tick_deadline` (anchored late service, bounded recovery, debt shedding, pause resets), `browser_timer` (the separate connection-interval delay policy), `integrated_server_generated_fluid_seed_reaches_live_tick_loop`, `integrated_server_fluid_tick_crosses_into_its_next_chunk_owner`, `integrated_item_tick::integrated_tick_loop_advances_a_live_dropped_item`. `just wasm-check` covers browser compilation, confinement and worker control tests; visible acceptance needs `just run-wasm` with ordinary singleplayer (placed-water progression, a mined item's motion; probe controls in [browser-worldgen-worker](browser-worldgen-worker.md)).

## How to change it

- Keep the tick body target-independent; target code belongs at the timer boundary. Change world scheduling in `tick_deadline` with injected-clock tests that distinguish anchored recovery from delay semantics, preserving recovery bounds, debt shedding and pause resets. Connection intervals belong in `browser_timer`; do not apply world catch-up to vitals or publication intervals.
- A new browser-produced feed is consumed either by the world tick or the connection publication pass; producer and consumer share the same authoritative handle, and the consumer must be reachable from a timer arm as well as packet dispatch. Never add a second mutable world or connection-local simulation loop.

## Configuration

`tick::MILLIS_PER_TICK` fixes 50 ms. `tick_deadline` owns `RECOVERY_TICKS` (2), `RECOVERY_BUDGET` (8 ms) and `MAX_DEBT` (2 s). No environment variables; the shell launch path picks worker startup versus the single-thread fallback.

## Dependencies

`lodestone-server` world, scheduled-tick, block-entity and entity-feed handles; `lodestone-time` (wasm-safe clock); Tokio task primitives; `setTimeout` bindings via `js-sys`, `web-sys` and `wasm-bindgen-futures`. The worker transport is a shell concern and must carry the same byte stream without moving world authority to the page.
