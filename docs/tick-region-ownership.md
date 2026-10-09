# Tick region ownership

## What it is

`lodestone_server::tick_region::TickRegionPlan` assigns every chunk selected for a server tick to an explicit owner, `TickOwner::Chunk { cx, cz }`. Most phases still run owners serially; the owner boundary exists so a phase can plan work per owner (optionally on bounded native lanes) and publish it through one central, order-restoring writer.

## How it works

### The plan

`tick_area::FollowArea` builds a stable, duplicate-free chunk set from player anchors and stores it in a `TickRegionPlan`. The tick loop iterates `FollowArea::owned_chunks()` for the random-tick and thunder passes, in the canonical order the random-number draws depend on. A duplicated list is rejected before ownership is assigned, because a duplicate would advance one chunk twice per tick.

The plan's size is the world's simulation distance: each player contributes the `(2d + 1)²` square around their chunk (441 columns at the default `d = 10`), unioned and deduplicated. The tick loop re-reads `WorldStateHandle::simulation_distance` every tick, so `set_simulation_distance` moves the area on the next tick. With no player in the dimension the plan is the fixed `FALLBACK_TICK_RADIUS` square about the origin. Every pass over the plan reads columns only if they are resident and skips the rest, so a wide plan never generates on the tick task; the ticked set is the plan intersected with what the players' streamed views hold.

`TickStats::owner_work` counts completed ownership visits (`random_tick_owned_chunks`, `thunder_owned_chunks`, scheduled block/fluid entries, block-entity and entity batches and effects). These are workload counters, not timings.

### The owner-batch pattern

Every owner-aware phase has the same shape:

1. A `tick_*_owner_batches` step clones tick-start state under each source chunk, one batch per owner. Empty batches are kept so completeness is checkable.
2. Planning is serial, or runs on `run_bounded_owner_jobs` (see [region owner execution](region-owner-execution.md)) with immutable inputs only.
3. A central `apply_*_owner_batches` writer is the sole mutator. It checks the plan generation and requires a complete, unique owner set (stale, replayed, incomplete, duplicate or mixed completions fail before any write), then restores the original serial slots (entity-id, registry or vector order) before publishing.

Owners are found with `floor` then Euclidean division, so `x = -0.5` belongs to chunk `-1`.

| Phase | Planner / writer | Parallel when |
|---|---|---|
| Scheduled ticks (block and fluid) | `ChunkScheduledTickQueue` / `tick::apply_scheduled_tick_owner_batches` | serial; global `(trigger, priority, insertion)` order restored |
| Block entities | `BlockEntityRegistry::tick_plan` / `tick::apply_block_entity_effect_batches` | >=128 entries, non-hopper, native |
| Ambient sound effects | `MobSim::take_ambient_sound_effect_batches` / `tick::apply_entity_effect_batches` | serial |
| Lightning, fishing, wither, dragon | `MobSim::tick_*_owner_batches` / `apply_*_tick_owner_batches` | serial: they share one RNG stream, consumed in entity-id order during planning |
| Spawner blocks | `SpawnerTickBatchBuilder` / `apply_spawner_tick_owner_batches` | serial: one world-wide spawner RNG; entity ids allocated after slots are restored |
| Falling blocks | `tick_falling_block_owner_batches`, `merge_falling_block_tick_effect_batches` | serial; landing placement and discard stay separate effects |
| Boats | `tick_vehicle_owner_batches` | >=128, up to 4 lanes |
| Primed TNT | `tick_tnt_owner_batches` | >=128, up to 4 lanes |
| Minecarts | `tick_minecart_owner_batches` | >=128, up to 4 lanes |
| Experience orbs | `tick_orb_owner_batches`; merge scan stays central and id-ordered | >=128, up to 4 lanes |
| Dropped items | `tick_item_owner_batches`; proximity merge stays central | >=128, up to 4 lanes |
| Entity pushing | `tick_entity_push_owner_batches` / `apply_entity_push_owner_batches`; pair scan stays global | >=128 mobs, up to 4 lanes |
| Burning | `tick_burning_owner_batches` / `apply_burning_owner_batches`; runs after projectile impacts | serial |
| Leashes | `tick_leash_owner_batches` / `apply_leash_owner_batches`; holder positions copied up front | >=256, up to 4 lanes |
| Projectile motion | `tick_projectile_owner_batches`; impact search stays serial before it | serial arm in production |

Hoppers stay serial: their vertical container relation has no cross-owner hand-off. Browser builds always use the serial arm because scoped native threads trap there.

### Chunk lifecycle owners

`ChunkLifecyclePlan` assigns each on-demand load and cache release to `ChunkLifecycleOwner::Chunk`. `ChunkStore` consumes it through `ChunkLifecycleHandoff` around the real `column_at` and `unload` calls. Evictions are bounded by current cache entries, deduplicated and ordered `(cx, cz)` before `ChunkSource::unload`.

Each batch slot moves through `SourceReady`, `SourceInFlight`, `PersistenceReady`, `PersistenceInFlight`, `Complete`; out-of-order or old-batch replies are rejected. A same-coordinate load or release holds a weak-referenced source gate until persistence acknowledges.

Durable saves use `WorldSaveHandle::begin_save`, which creates a single-use `WorldSaveJob` carrying a bounded token snapshot. Tokens are acknowledged only after their region writes succeed. A failed save leaves the token queued, and a newer unload of the same coordinate supersedes the old token. The save plan (`WorldSaveRegionPlan`) groups columns by physical region file, and at most two region owners write concurrently, with results consumed in canonical owner order.

World border, game rules, time, weather and natural-spawn planning remain global.

### Measuring workloads

- `FollowArea::candidate_region_workload` groups the selected chunks into cells of an observer-supplied edge. It reports spatial spread only; it does not change ownership or order.
- `chunk_owner_profile::SCENE_NAME` is a deterministic scene: eight owners each with a furnace, one due block tick and one due fluid tick, plus 64 cows. With the `profile-harness` feature (paused clock) the run advances exactly 128 ticks.

Measured on one host, four native owners (one lane vs four lanes):

| Scene | 256 entities | 2,048 entities |
|---|---|---|
| Dropped items | 4.474 ms / 2.251 ms | 34.554 ms / 15.093 ms |
| Experience orbs | 6.867 ms / 2.032 ms | 33.925 ms / 17.306 ms |
| Leashes | 0.545 ms / 0.195 ms | 10.951 ms / 4.515 ms |

They justify the dispatch thresholds for those named scenes only, not a mixed full tick.

## How to change it

- Do not coalesce chunk owners into larger regions or add region workers here alone; the prerequisites are in `docs/plans/regionised-server-ticking.md`.
- A new producer for the plan must deduplicate and preserve a deliberate visit order. Keep `FollowArea` as producer until the tick loop gets its work from another production-consumed boundary.
- To make an entity phase owner-aware, add a typed batch to `MobSim` with an explicit source position and old serial slot, and have the tick loop consume it centrally. Never publish from a chunk owner. Grouping alone is not parity: interleaved owners in the serial list change packet order if applied owner-major.
- Never pass `MobSim`, a registry lock or mutable storage into `run_bounded_owner_jobs`. Cross-owner reads (leash holders, push neighbours) come from a tick-start snapshot.
- Never drain a scheduled queue from an owner worker. Keep the world-wide comparator as selector and one completion per selected owner.
- Keep burn planning after projectile impacts; moving it earlier delays ignition a tick.
- Moving entities across a chunk edge use the barrier in [entity ownership transfer](entity-ownership-transfer.md).
- Lifecycle: becoming resident only permits a later demand load. Call `ChunkSource::unload` only after the cache entry is removed and never under the cache lock. Use bounded ledgers, not a global acknowledgement history, so an old reply is stale.
- For every new boundary keep these controls: negative-coordinate, interleaved-owner, reversed-completion, missing-owner and duplicate-owner. Keep an independently built reference schedule (as `tests/tick_region_owner_parity.rs` does).
- To profile, run `just bench-chunk-owner-tick` first (it rejects a scene with a missing phase sample), then `just samply-chunk-owner-tick` for a call tree. The latter takes `--ticks <1..512>`, `--wall-deadline-secs <1..60>`, `--output-dir` and `--run-id`. A high phase cost with a missing or small `owner_work` count is a fixture failure, not evidence for parallelism.

## Configuration

- `simulation-distance` (`WorldStateHandle::set_simulation_distance`; default `chunk_store::DEFAULT_SIMULATION_DISTANCE` = 10, accepted range 1..=32). The browser build sets `chunk_store::BROWSER_SIMULATION_DISTANCE` = 4, a budget choice that has not been measured in a browser.
- Chunk ownership has no tunable size. Dispatch thresholds (128 entities; 256 leashes) and the four-lane cap are code constants chosen from the measurements above, not server settings.
- The profile example's 128-tick run is fixed so counters name the same workload. The `profile-harness` feature selects the paused clock; both `just` recipes enable it.

## Dependencies

- `tick_area::FollowArea` supplies the chunk set; `tick::run_tick_loop` consumes it.
- `chunk_store::ChunkStore` consumes lifecycle assignments (see [chunk lifecycle](chunk-lifecycle.md)).
- The profile scene drives `IntegratedServer`, `BlockEntityRegistry`, the scheduled-tick queues and `MobSim` through their ordinary production path.
