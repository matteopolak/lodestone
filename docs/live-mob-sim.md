# Live mob simulation (issue #217)

## What it is

The production wiring that turns `lodestone-server`'s `MobSim` (AI-driven mob
motion, computed server-side) into mobs a real client actually watches move.
Before this, `mobs.rs`'s own module doc admitted "no encoder wiring exists" —
but the encoders (`V770ServerProtocol::encode_add_entity`/
`encode_entity_update`/`encode_remove_entity`) had in fact already shipped and
were proven against a real client. The actual gap was one level up: nothing in
production ever *constructed* a `MobSim` or *ticked* it. `IntegratedServer`'s
singleplayer path called `open_in_memory` (no entities), never
`open_in_memory_with_entities`.

## How it works

```text
IntegratedServer::open_in_memory_with_mobs(protocol, source, world_source, mob_area, mob_center, mob_count, view_radius)
  ├─ spawns the existing connection task (unchanged): serve_connection over `source`,
  │  diffing `LiveMobSource::snapshots()` against what the connection last sent —
  │  this diff/encode/decode/client-fold chain already existed and was already
  │  proven live (tests/entity_streaming_live.rs), just never fed by a real sim.
  └─ spawns a second task: mobs::run_mob_tick_loop(world_source, mob_area, mob_center, mob_count, out)
       ├─ ChunkWorld::from_source(world_source, ..) — a *second*, independent
       │  snapshot of the same deterministic terrain the connection streams
       │  (same seed ⇒ identical terrain; see the function's own doc comment
       │  for why two instances rather than one shared one)
       ├─ seed_demo_mobs(..) — spawns a small fixed zombie population with
       │  RandomStroll + RandomLookAround goals, once, at startup
       └─ loop: tick.tick().await (50ms) → sim.tick() → out.publish(snapshots)
```

`LiveMobSource` is an `Arc<Mutex<Vec<EntitySnapshot>>>` behind `EntitySource`
— the same shape `entity_streaming_live.rs`'s test-only `SharedSnapshotSource`
already used to prove the read side works, now backing a real simulation
instead of a hand-mutated `Vec`.

`crates/lodestone-shell/src/net.rs`'s `run()` calls
`open_in_memory_with_mobs` for `Origin::Integrated` (singleplayer), with a
small fixed chunk radius (`view_radius.clamp(1, 3)`) around the join spawn
(chunk `(0,0)`, matching `V770ServerProtocol::begin_play`'s hardcoded
`spawn_x`/`spawn_z` = 8) — independent of the client's own (possibly larger)
view radius, since a handful of wandering mobs do not need the whole streamed
view.

## The bug this wiring found live

`MobSim::new` starts entity ids at `1` — and so does `V770ServerProtocol`'s
`LOCAL_PLAYER_ENTITY_ID`. A real client never spawns "itself" as a separate
`ADD_ENTITY` (id `1` *is* self), so the very first mob a fresh `MobSim` ever
spawned silently failed to appear: `tests/live_mob_sim.rs` consistently
observed exactly 2 of 3 seeded mobs, never 3, until `run_mob_tick_loop`
started calling the new `MobSim::set_next_id(1000)` before seeding. This is
recorded on `set_next_id`'s own doc comment. `MobSim::new`'s default (`1`) is
unchanged — every existing hermetic test (`tests/mob_sim.rs`) keeps its
already-asserted ids stable; only a caller sharing a wire id space needs to
call `set_next_id`.

## How to change it, and the gotchas

- **`Goal: Send` is what unblocked this.** `entity_streaming_live.rs`'s
  original doc comment (still worth reading) explains that `MobSim` used to be
  `!Send` (`Box<dyn Goal>` had no `Send` bound), which made it unusable as a
  `tokio::spawn`ed `EntitySource`. That bound landed
  (`crates/lodestone-entity/src/ai/goal.rs`) before this issue was picked up —
  re-verified directly (`assert_send::<MobSim<'static>>()` in `mobs.rs`
  compiles) rather than taken on the comment's word, per `CLAUDE.md`'s
  "re-verify before routing around 'X doesn't exist yet'" rule. Do not
  reintroduce a non-`Send` field on `MobSim`/`SimMob`/`GoalSelector` without
  checking this call site.
- **The `ChunkWorld` snapshot is static for the tick task's whole lifetime.**
  Nothing re-queries `world_source` after `run_mob_tick_loop` starts, so a mob
  cannot path across a chunk boundary outside the initial `mob_area`. Widening
  this to grow with the player's position is future work; see the function's
  own doc comment.
- **No natural spawning.** `SpawnCandidateSource` (biome/light-aware natural
  spawn selection) has no production implementation — every existing impl in
  `mob_spawn.rs` is a test mock. `seed_demo_mobs` seeds a small fixed
  population once instead, purely so #217's actual subject (AI motion
  reaching the wire) has something to move. A caller that wants real spawning
  swaps in `MobSim::run_spawn_cycle` once a real `SpawnCandidateSource` exists.
- **No despawn pass.** `MobSim::despawn_pass` needs a player position the tick
  task has no way to learn (`EntitySource` is deliberately read-only,
  one-directional). A long singleplayer session keeps the same fixed demo
  population forever rather than vanilla's cap-driven churn.
- **`wasm32` gets no live mob sim.** `run_mob_tick_loop` needs
  `tokio::time::interval`, unavailable there (same class of gap as
  `PlayerVitals` already has on that target — see `server.rs`'s
  `serve_play`/wasm32 split). `open_in_memory_with_mobs` is
  `#[cfg(not(target_arch = "wasm32"))]`; `net.rs` falls back to the old
  mob-free `open_in_memory` there.
- **`mob_spawn.rs` has an independent, duplicate `MobCategory`/`check_despawn`**
  from whatever `lodestone-entity`'s own spawn module has (noted by the
  entity-islands agent while building `ProjectileRegistry`/`ItemEntityRegistry`,
  `docs/entity-tick-drivers.md`). This wiring does not fold them together —
  `seed_demo_mobs` does not touch spawn categories at all — but a future
  natural-spawning pass will need to pick one.
- **Done: `ProjectileRegistry`/`ItemEntityRegistry` now share this loop**
  (issues #211/#215). `MobSim` owns both as fields, ticks them inside
  `MobSim::tick()`, and `run_mob_tick_loop` publishes `sim.snapshots()`
  (mobs + projectiles + items) instead of just the mobs. See
  `docs/entity-tick-drivers.md`'s "Production wiring" section for the shape;
  this bullet used to ask for it as follow-up and is corrected now that it
  exists, rather than left to mislead the next reader.

## Configuration

No feature flags. Constants live at the call site:

- `net.rs`: mob count (`6`), spawn center (`(8, 8)`, matching `begin_play`'s
  hardcoded spawn), and `mob_area` radius (`view_radius.clamp(1, 3)`).
- `mobs.rs`: `seed_demo_mobs`'s ring radius (`6.0` blocks), `MOB_TICK_INTERVAL`
  (`50ms`, one vanilla tick), and the `1000` starting id in
  `run_mob_tick_loop`.

## Dependencies

- `lodestone-entity`'s `ai` module (`Goal`, `GoalSelector`, `NavigatingMob`,
  `RandomStrollGoal`, `RandomLookAroundGoal`) — the AI this wiring drives, not
  authors.
- `crates/protocol/v770`'s `V770ServerProtocol` for the encoder half
  (`encode_add_entity`/`encode_entity_update`/`encode_remove_entity`) and for
  `tests/live_mob_sim.rs`, the acceptance gate (`#[ignore]`d — real wall-clock
  timing; run with `-- --ignored --nocapture`).
- `tokio::time` (native only) for the tick interval.
