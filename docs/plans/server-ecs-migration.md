# Plan: migrating `lodestone-server` onto its own `bevy_ecs::World`

## What it is

The phased plan for moving the server's `Arc<Mutex<_>>` state onto a tick-thread-owned, unlocked `bevy_ecs::World`, with core subsystems as bevy plugins. The architecture itself (two `World`s, no lock on the server's, clause 4's inversion) is settled in [The integrated and dedicated server](../dedicated-server.md); this plan only covers migration mechanics.

## How it works

The shape every phase works toward:

- The tick task owns a `World` (not an `App`, which is `!Send`). `ServerApp::bootstrap` in `crates/lodestone-server/src/ecs/` builds it from `ServerCorePlugin` and runs `ServerBoot` once; the tick loop calls `run_schedule(GameTick)` once per world tick.
- `bevy_app`/`bevy_ecs` are `0.19`, `default-features = false`, `features = ["std"]`, never `multi_threaded`, so `run_schedule` is a plain synchronous call and tokio has no second executor to reconcile with.
- Connection tasks never touch the `World`. They enqueue proposals over `mpsc` and read published snapshots; the single exception is one oneshot reply for container open. Do not add a second place a connection task waits on the tick thread.
- Proposals flow `Drain -> Adjudicate -> Apply` inside `GameTick` (`ecs/proposals.rs`). A refusal produces a corrective packet to the client.
- Do not install `lodestone_ecs::CorePlugin` on the server: it inserts `FrameClock` (meaningless server-side) and `LockHolds` (the meter for a lock the server does not have, so a zero reading would look like a measurement). `WorldTime` is reusable.
- Keep the tick accumulator out of the `World`.

## Remaining phases

Each phase lands on its own, leaves `main` green, and names a gate plus a negative control that must be seen failing.

1. **Feeds become broadcast egress.** Replace single-consumer block-tick and explosion feeds with `broadcast::Sender` and a per-connection receiver; `RecvError::Lagged` is a resync signal. Gate: two LAN connections both receive a tick-produced block change.
2. **Player entities.** Per-connection simulation scalars (vitals, fall, inventory state) become components on a server entity spawned by a proposal; the per-connection vitals timer becomes a `GameTick` system. Gate: A attacking B reduces the server's copy of B's health.
3. **World-scoped state.** Broadcast game time comes from the `WorldTime` resource and `WorldAdminState` becomes a resource; behaviour lives in [world-state](./world-state.md). Gate: two connections see the same game time.
4. **Block entities become components** (furnace, hopper, composter, brewing stand); the container-open oneshot becomes a `Query`.
5. **Mob sim becomes components, then `MobAiPlugin`.** Two separate commits: move the population, then wrap in the plugin. Keep the `dyn Goal` seam on `SimMob::add_goal`. Benchmark `world_tick` after each commit; the `lodestone-entity` mob bench sits below the seam and cannot see either.
6. **Plugin surface.** `Cancelled` on a proposal and `EventPriority` ordering in the server schedules. Gate: a plugin ordered before the consumer vetoes a break; ordered after, it cannot.
7. **Parallel track: split `LocalPlayerPlugin`** (`crates/lodestone-ecs/src/player.rs`) into `PlayerStatePlugin` and `PlayerPhysicsPlugin` so a headless bot can omit physics. `pin_passenger_to_vehicle` belongs in the state plugin, ordered after `TickSet::Physics`. Test it as a plain `App` with `Runner::Headless`, not a `--headless` render, which uses a different mesher.

## How to change it

- A phase gate must run against a driver production spawns; a hand-built `App` cannot tell "registered and running" from "registered and never run".
- The ambiguity gate (`ambiguity_detection: LogLevel::Error` then `schedule.initialize`) goes vacuous if the app runs first. Copy the comment in `crates/lodestone-controller/src/ecs.rs` with the code.
- `ChunkSource::set_block` takes `&self` (interior mutability), so the type system cannot stop a connection task mutating terrain; enforce it with a source scan. Its default body is a silent no-op, and the default `block_state` regenerates a whole column per query.
- Perf gate for every phase: `world_tick` within +-5% of the previous baseline, and `scripts/wasm-size.sh` for size.

## Configuration

None. No feature flag gates the migration; a feature-gated `World` would mean two server architectures to test.

## Dependencies

- `bevy_app`, `bevy_ecs` (workspace pins above), `lodestone-world`, `lodestone-game` (`cargo xtask check-isolation` enforces version-freedom).
- `tokio::sync::{mpsc, broadcast, oneshot}`, already wasm-safe.
- `lodestone-ecs` `EventPriority` and `GameEventBus` for phase 6. See also [plugin-api](../plugin-api.md).
