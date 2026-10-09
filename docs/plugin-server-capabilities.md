# Server-side plugin capabilities

## What it is

The general server-side capability surface on the server's own `bevy_ecs::World` (`crate::ecs` in `lodestone-server`): typed plugin messages, native world snapshots, the proposal/adjudication layer on `TickSet::Adjudicate`, a Paper-shaped event bus, and a tick scheduler with off-tick hand-back. It also records which older capability clusters follow the client's intent doctrine ([`plugin-api.md`](plugin-api.md)).

## How it works

### Plugin messages

Native server plugins share observation types with `#[derive(Message)]` and `App::add_message::<T>()` (idempotent, so a consumer may register before the producer). Producers use `MessageWriter<T>` or `World::write_message`; consumers use `MessageReader<T>`.

`ServerCorePlugin` runs `message_update_system` before `TickSet::Drain`, because the server drives `GameTick` directly. A message is readable for the rest of its tick and all of the next, then lost; it is not a durable queue. Do not add another aging system for a registered type, and note that inserting `Messages<T>` as a resource does not register maintenance. Scheduler callbacks run after maintenance, with the same lifetime.

### World snapshots

`ecs::ServerWorldSnapshot` is inserted by the integrated-server constructor with the same `Arc<dyn ChunkSource>` serving clients. `read_blocks`/`read_block` return at most `MAX_WORLD_SNAPSHOT_POSITIONS` (128) copied `StateId`s per call, with `None` for a cold, contended or out-of-range cell, so a plugin cannot trigger generation. It is observation only; writes go through `ServerProposalHandle::set_resident_block` (or the checked `IntegratedServer` wrapper) and `Drain -> Adjudicate -> Apply`.

### Proposals and adjudication

`TickSet` is `Drain`, `Adjudicate`, `Apply`, `Simulate`, `Publish`. Server-side, the plugin outranks the client's proposal, the inverse of the client doctrine's "human wins". `ServerProposal` carries a `ServerProposalAction`: `SpawnMob`, `NaturalSpawnMob`, `DespawnMob`, `SetResidentBlock { BlockPos, StateId }`, `PlayerInteract`, `BlockBreak`, never a world borrow, source handle or raw registry integer.

Plugins observe with `MessageReader<ServerProposal>` and answer with `ServerProposalDecisions::decide`: `Allow`, `Deny` (typed refusal) or `Replace(action)`. Lower numeric priority wins; ties keep the first decider. `Apply` consults the decision table before performing the action.

- **Checked spawn/despawn:** callers await `IntegratedServer::spawn_mob_proposed`/`despawn_mob_proposed`. The request enters a bounded queue (64 in flight, one-second deadline), is adjudicated in `Adjudicate`, and `Apply` returns the final action before `MobHandle::with` runs. A stalled tick task yields `Unavailable` or `TimedOut`, never a late mutation. Legacy `spawn_mob`/`despawn_mob` stay direct for compatibility.
- **Natural spawn:** the tick loop plans candidates after a short census lock, stages `NaturalSpawnMob` actions, runs `GameTick`, then materializes accepted candidates under a fresh lock. No adjudicator runs while `MobHandle` is held. Automatic distance despawn does not yet submit proposals.
- **Block mutation:** `set_resident_block_state_proposed` takes a `BlockPos` and validated `StateId`; after adjudication it checks the column is resident and Y in range, writes through the live source and publishes the change. There is no load or generation fallback. `BlockMutationRefusal` covers policy, timeout, availability, mismatched replacement, nonresident column and vertical bounds. The write happens after adjudicators finish, so no callback runs under a source lock.

### Paper events

`ecs::PaperEventBus` layers ordered listeners over proposals:

| `PaperEventKind` | proposal owner | status |
|---|---|---|
| `EntitySpawn`, `EntityDespawn` | `SpawnMob`, `DespawnMob` | supported |
| `ResidentBlockChange` | `SetResidentBlock` | one resident-state proposal; not every Bukkit block event |
| `PlayerInteract` | `PlayerInteract` | value-only block-face interaction, no player wrapper |
| `BlockBreak` | `BlockBreak` | validated player breaks; cancellation sends authoritative state back to the connection |
| `InventoryClick` | none | registration returns `PaperEventRegistrationError::Unsupported` |

Connection-facing owners submit value-only proposals via `ServerProposalHandle::player_interact`, `despawn_mob` and `block_break`. Listeners sort by `PaperEventPriority` then registration order and share one mutable event, so later listeners see earlier replacements and cancellations. After dispatch, cancellation becomes `Deny`, a changed event `Replace`, an unchanged one `Allow`. `Monitor` runs last and is read-only by enforcement: mutations are rolled back and recorded as `PaperEventFailureReason::MonitorMutation`. A panicking listener is caught and recorded without stopping later ones. Proposal variants without an event kind stay with their existing consumers.

There is no server-lifecycle kind and no owner for inventory clicks, game-mode changes, natural-spawn candidates or block-write batches. This is a local vocabulary, not a drop-in Bukkit claim: it has not been differentially run against Paper, the JVM runner shares no production event-driving path, and corrective client output is unproven for every owner. The conformance scaffold must keep reporting these gaps.

### Tick scheduling and off-tick work

`ServerTaskScheduler` (installed by `ServerCorePlugin`) runs `run_server_tasks` in `TickSet::Drain` on in-memory and persistent worlds, including the dedicated binary. Register during `ServerApp::bootstrap_with` or from a system's `ResMut<ServerTaskScheduler>`; callbacks get `&mut World` and a `ServerTaskId`.

- `schedule_once(delay, cb)` fires after `max(delay, 1)` passes; `schedule_repeating(delay, period, cb)` then every `max(period, 1)`. Boot does not advance the clock, so delay 2 and period 3 fire on ticks 2, 5, 8. Equal deadlines keep registration order; an ordered queue avoids scanning future entries.
- `cancel(id)` returns whether the handle was live and may cancel another due callback or the caller itself. Work registered inside a callback starts no earlier than the next pass. Tasks are transient: no persistence or unloading. Delays reject `u64` overflow.
- `spawn_with_handback(work, hand_back)`: `work` is parameterless, `Send` and runs on a named worker thread (inline on wasm32); `hand_back(value, &mut World)` runs from `run_server_tasks` after message maintenance and before due callbacks. Arrival tick is nondeterministic, the mutation site is not. At most 64 jobs (running plus awaiting hand-back, `with_async_hand_back_capacity` to change) else `ServerAsyncTaskError::Full`, which is backpressure, not a reason to block the tick.
- `cancel_async(id)` guarantees an unfinished result will not mutate the world but cannot interrupt running work. `shutdown_async_tasks()` (also on drop) rejects new work and drops queued completions; a panicking work closure is discarded.

```rust,ignore
ServerApp::bootstrap_with(|app| {
    app.world_mut().resource_mut::<ServerTaskScheduler>()
        .schedule_repeating(2, 3, |world, id| {
            let _ = world.resource_mut::<ServerTaskScheduler>().cancel(id);
        });
})
```

### Older capabilities against the five-clause doctrine

The client doctrine asks: (1) observation vocabulary, (2) one owner, (3) observable refusal, (4) arbitration against a human source, (5) lifecycle shape. Server-side there is no local human, so clause 4 is either not applicable or inverted (plugin outranks proposal).

| capability | symbol | verdict |
|---|---|---|
| custom generator | `lodestone_worldgen::generator::ChunkGenerator` | a trait a plugin implements; sole terrain source, so clauses 3-5 are N/A |
| custom dimension | `lodestone_server::plugin_dimension::DimensionRegistry` | registration; `register` returns `None` on a duplicate key |
| live structure placement | `lodestone_server::structure_placement::place_structure_live` | direct synchronous call returning cells written, no veto |
| entity spawn/despawn, block mutation | `IntegratedServer::*_proposed` | proposal layer above, with typed refusals and priority arbitration |
| block observation | `ecs::ServerWorldSnapshot` | copied answers, `None` for cold cells |
| crafting stations | `plugin_crafting::CraftingStationHooks`, `StationVerdict::{Allow, Deny, Replace(ItemStack)}` | observation-only `StationInputs`; one choke point (`workstation_result`); clauses 4 and 5 dropped by argument ([`plugin-crafting-hooks.md`](plugin-crafting-hooks.md)) |

Template for new capabilities: when one resembles a prepare-style event (observation, verdict, first non-`Allow` wins), port that shape; otherwise decide clauses 4 and 5 by argument in the doc. Never ship only `Allow` (silently dropping clause 3) or fake a human to outrank (clause 4 by fabrication).

## How to change it

- **Do not build a bespoke adjudication mechanism for the next capability.** Add a `ServerProposalAction` variant only once it has a production owner and a second real claimant; a speculative variant has no consumer. Worldgen, dimensions and structure placement have no second claimant and stay off the layer. Crafting hooks stay on `StationVerdict`: migrating them would be change for its own sake.
- Follow the checked-spawn split: no callback under `MobHandle` or a source lock. Future automatic despawn must use it.
- Message maintenance belongs in `ServerCorePlugin`, before the first gameplay set. Keep shared observation types in the plugin's public API and version-free. Controls: `ecs::messages` tests (retention boundaries), `independent_plugins_exchange_bounded_messages_on_the_primary_tick_task`, and the dedicated `dedicated_scheduler_runs_delayed_work_on_the_persistent_primary_world` (exact per-tick counts, including cancellation before a third repetition).
- The scheduler lives in `ecs/scheduler.rs`. Systems sharing resources with callbacks must order against `run_server_tasks` if they also occupy `TickSet::Drain`. Keep the resource installed during callbacks so nested scheduling and cancellation work.
- Keep `spawn_mob` direct; `crates/lodestone-server/tests/native_plugin_spawns_and_despawns_a_mob.rs` is the compatibility control.
- There is no WASM-tier server counterpart (`lodestone-wasm-host` is client-only); every surveyed capability is native-tier. Phased work is in [`plans/server-ecs-migration.md`](plans/server-ecs-migration.md).

## Configuration

No environment variable or flag. Scheduling uses integer gameplay-tick delays. The adjudication layer lives in `lodestone_server::ecs::proposals`: 64 in-flight requests, one-second async caller deadline.

## Dependencies

`bevy_ecs`/`bevy_app` and Tokio, direct dependencies of `lodestone-server`. It deliberately does not use `lodestone-ecs`, which would drag the client vocabulary into the server graph; re-run `scripts/wasm-size.sh` before adding it. Related: [`plugin-worldgen-api.md`](plugin-worldgen-api.md), [`plugin-entity-api.md`](plugin-entity-api.md), [`dedicated-server.md`](dedicated-server.md), [`packet-wiring.md`](packet-wiring.md) (the client-side first-non-`Allow` equivalent).
