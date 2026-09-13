# Browser integrated tick loop

## What it is

The browser integrated server runs the same authoritative world simulation tick as the native server at 20 ticks per second. A browser-compatible timer supplies the scheduling boundary while the world source, scheduled queues, block-entity registry, and entity source remain shared with the connection.

## How it works

`IntegratedServer::open_in_memory_with_items_and_commands` starts the primary tick future for browser singleplayer and passes the same handles to the connection task. `run_primary_tick_loop_with_weather` executes the complete world tick body: scheduled block and fluid work, block entities, random updates, entity physics, mob simulation, and publication of world snapshots and feeds.

The timer uses a real browser macrotask so generation and simulation return control to the browser event loop. Its 50 ms cadence uses delay semantics: if the task is late, it emits one resumed tick and rebases the next deadline from the current time instead of replaying missed ticks in a burst.

The timer must work in both browser contexts used by the shell. The page context exposes a `Window`, while the dedicated server worker exposes a worker global. A portable sleep implementation must resolve `setTimeout` from the active global rather than assuming that a `Window` exists.

Simulation publication is separate from simulation mutation. After a world tick changes a block, scheduled queue, or entity snapshot, the connection loop must drain the corresponding feed and run its streaming diff even when the client has sent no packet. Otherwise an idle client can retain stale water, item entities, or mob positions until its next input packet.

## How to change it

Keep the tick body target-independent; target-specific code belongs at the timer boundary. If the timer policy changes, update the pure deadline tests with an input where delayed and burst policies produce different next deadlines. Tests should also cover the exact-deadline boundary, a multi-period stall, and repeated stalled polls to prove that one stall cannot create a zero-delay spin.

When adding a browser-produced feed, decide whether it is consumed by the world tick or by the connection publication pass. The producer and consumer must share the same authoritative handle, and the consumer must be reachable from a timer arm as well as from packet dispatch. Do not add a second mutable world or a connection-local simulation loop.

## Configuration

The primary browser interval is fixed at 50 ms, representing 20 ticks per second. The browser timer does not require environment variables. Worker startup and the single-thread fallback are selected by the shell launch path.

## Dependencies

The loop depends on `lodestone-server` world, scheduled-tick, block-entity, and entity-feed handles; `lodestone-time` for a wasm-safe monotonic clock; Tokio task primitives; and browser `setTimeout` bindings supplied by `js-sys`/`web-sys` and `wasm-bindgen-futures`. The worker transport remains a shell concern and must carry the same server byte stream without moving world authority to the page.
