# World events

## What it is

Server-driven world state not tied to one player: rain and thunder weather with lightning, the regional-difficulty scalar, Bad-Omen raids and pillager patrols, and the ender dragon and wither fights. It lives in `lodestone-server`, with a thin render consumer in `lodestone-render`/`lodestone-shell` for weather and lightning visuals.

## How it works

### Weather

The server owns weather and sends four `GAME_EVENT` codes (`START_RAINING`, `STOP_RAINING`, `RAIN_LEVEL_CHANGE`, `THUNDER_LEVEL_CHANGE`) carrying rain and thunder levels in `0.0..=1.0`. The client folds them into `net::WeatherCell` (two atomics, latest wins). `WeatherTracker::state()` is read once per frame and drives angled rain and snow quads in the block pass, sky, horizon and fog darkening (`desired_fog` in `app.rs`), the lightmap `sky_darken` lane, and a lightning flash (250 ms, 5 ticks) triggered by an `ADD_ENTITY` for `lightning_bolt`.

Rain versus snow is per column from the biome climate (`has_precipitation`, `temperature`, `downfall`, decoded into `ClientEvent::BiomeClimates`): height-adjusted temperature `>= 0.15` rains. Raining is `rain > 0.2`, thundering `thunder > 0.9`, and the effective thunder level is `raw_thunder x rain`, never the raw wire value.

Rain ambience reaches `ShellAudio` through the exposed landing sample (`SoundCategory::Weather`; a missing or covered landing suppresses it). Local splash impacts are predicted per 20 Hz tick: a known `MOTION_BLOCKING` landing at or below the camera whose biome resolves to rain. Unknown chunks, covered spots, snow biomes and faint weather give none; accepted impacts use the normal particle path.

Constants: rain and snow max alpha `1.0`/`0.8`; distance fade `lerp(min(d^2/r^2, 1), max_alpha, 0.5) * intensity`; sky rain darken `x(1 - 0.5r, 1 - 0.5r, 1 - 0.4r)`; thunder darken `x(1 - 0.5t)`; `SKY_LIGHT_FACTOR` floor `0.24`; flash lerps `0.22` toward `(204, 204, 255)` and forces `SKY_LIGHT_FACTOR` to `1.0`. All colour maths is in gamma space; linear light washes the image out.

### Lightning

`should_attempt_strike` is checked once per ticking chunk per tick, gated on `weather.thundering`. `find_lightning_target_around` prefers a nearby lightning rod, then a random living sky-visible entity in a column, then the heightmap. A strike spawns a live bolt streamed as `minecraft:lightning_bolt` with empty metadata.

`resolve_effect` is the hit table: default 5.0 damage plus 8 s ignite; creeper default only (no charged state); pig becomes zombified piglin; villager becomes witch; mooshroom guarded but never flips; turtle takes a lethal hit with no ignite. The skeleton-horse trap is a flag rolled at horse spawn (reading `effective_difficulty()`), later turned into a cosmetic bolt plus skeleton posse by an AI goal. Not modelled: rod power pulse and copper-wax reset, setting struck mobs alight, players as candidates, and rods themselves (no POI manager, so the search misses).

### Regional difficulty

`DifficultyInstance` is a pure scalar of roughly `0.0` to `6.75`, recomputed per query by `run_tick_loop` from world difficulty, world time, chunk inhabited time and moon phase. Peaceful is `0.0`. Otherwise: base `0.75`, plus up to `0.25` as world time passes a 72,000-tick offset (capped at 1,440,000), plus a local term from inhabited time (cap 3,600,000; weight `1.0` Hard, `0.75` otherwise, halved on Easy), plus a moon term clamped by the global term, not by `1.0`, indexed `(day_time / 24000) % 8` with day 0 full moon; the total is multiplied by the difficulty ordinal (Peaceful 0 to Hard 3).

Chunk inhabited time is not tracked (saved as `0`), so the local term understates. Consumers: zombie and skeleton gear chance, door-breaking, zombie reinforcements and the horse-trap roll. Spawn caps are not difficulty-scaled.

### Patrols and raids

**Patrols** (`MobSim::run_patrol_spawn_cycle` plus `PatrolRouteGoal`): a pillager group spawns near a random player about every 12000 to 13200 ticks once `MobSim::tick_count` passes `PATROL_TIMELINE_GATE` (120000). Members path to a waypoint rotated 90 degrees about Y, shrunk to two-fifths and recentred (a loose-line wobble). The leader is slower (`0.595` vs `0.7`) and repicks a target in `-500..500` within 10 blocks. Followers pull the nearest leader's target once per tick in `MobSim::feed_perception` (no census primitive). Group size is Easy 2, Normal 3, Hard 4.

**Raids** (`MobSim::start_raid`/`tick_raids`): a `bad_omen` carrier within 64 blocks of an occupied village POI becomes `raid_omen` at the same amplifier (`absorb_raid_omen`, clamped `1..=5`); on its last tick `create_or_extend_raid` averages occupied POIs into a centre and extends a raid within 96 blocks or starts one. The occupied-POI signal sees only claimed villager beds (session-only), so a village with no claimed bed will not trigger. Ominous bottles are not modelled; testing needs `/effect give`.

Waves: Peaceful 0, Easy 3, Normal 5, Hard 7. Per-wave spawns: pillager `[0, 4, 3, 3, 4, 4, 4, 2]`, vindicator `[0, 0, 2, 0, 1, 4, 2, 5]`, plus a difficulty bonus roll (random of 2 Easy, flat 1 Normal, flat 2 Hard, then random up to bonus). Waves clear on live health checks, advance on a 300-tick cooldown and reach Victory 40 ticks after clearing. Only pillagers and vindicators exist. Raid loss is not ported (states are Ongoing or Victory); the 48000-tick timeout is. The boss bar reuses `BOSS_EVENT` with wave-count progress. Captain and leader banners are not drawn (no server-side equipment).

### Dragon fight

`crates/lodestone-server/src/dragon/` is pure (no world, entity or packet): `dragon::phase` is an eleven-phase machine in wire-value order (`PhaseManager::tick` takes `DragonInputs`, returns an optional `PhaseEffect`); `dragon::crystal` heals 1.0 HP every 10 ticks while a live nearest crystal exists; `dragon::fight` holds persisted flags, the world-load scan, boss-bar value, exit-portal geometry and the respawn sequence `Start`, `PreparingToSummonPillars`, `SummoningPillars`, `SummoningDragon`, `End`. The one substitution: the reference drives phase changes from a path search over a 12-node ring, which our ground-oriented flyer AI cannot do, so "flight leg finished" is a per-tick input. All other numbers match.

Wiring: `MobSim::spawn_end_dragon_fight` spawns ten end crystals atop the generated pillars (two caged), the dragon, and returns the inactive exit podium for the join path to write, behind an atomic `claim_dragon_fight_start` (process-lifetime, so a restart re-places the arena). `tick_dragons` drives phases; melee and arrows route through `damage_dragon` (the dragon lives outside `self.mobs`). A kill places the egg (first only), activates the exit portal and pops a shuffled gateway slot into a real `end_gateway`. Outer-island gateways carry exact destination block entities consumed on contact (same-dimension teleport, view recentred); an inexact exit searches for safe standing space, a missing one is inert; cooldown 40 ticks; mounted players stay put. Missing: the summoning beam target and a darken-screen bit on `BossBarSnapshot`. Fight state is not saved.

### Wither fight

`crate::wither` is the pure machine: a 220-tick invulnerable emergence (10.0 HP heal every 10 ticks, then 1.0 every 20), a power-7.0 blast when it ends, a powered-armour gate blocking arrow and wind-charge damage at `health <= max/2`, and skull numbers (8.0 damage with a living owner, power-1.0 blast, 5.0 owner heal on a kill, Normal/Hard wither-effect durations). `crate::mobs::wither` integrates: a structure matcher (soul sand or soil plus three skulls), `tick_withers` and `damage_wither`. The wither is a `HashMap<i32, TrackedWither>` that never moves. Missing: side-head aim, a darken bit, a skull hitting another wither, and real difficulty threading for the effect duration (always Normal).

In dense native scenes (128 or more withers, at most four lanes) the world-free state transition runs on the bounded region executor with cloned tick-start state; skull targeting, shared RNG, blasts, projectile allocation and commit stay in entity-id order on the central writer. Smaller scenes and browser builds stay serial. `measure_dense_wither_owner_workers` (ignored) measures planning; the owner-batch parity gate proves one-lane and four-lane results match.

## How to change it

- **Weather**: geometry in `lodestone-render/src/weather.rs` (pure), pass in `weather_pipeline.rs` plus `shaders/weather.wgsl`, wire to state in `net.rs` (`forward`/`WeatherCell`), per-frame composition in `app/weather.rs` and `app/redraw.rs`. The pass must not write depth, uses reversed-Z, and animates from the tick clock. Do not fix the `0.0`/`1.0` polarity of `START_RAINING`/`STOP_RAINING`; the next `RAIN_LEVEL_CHANGE` corrects it. Always read the composed thunder level, or a stale join value blacks out a clear sky.
- **Lightning**: `lightning.rs` (targets, bolt machine, hit table, pure), `mobs/lightning.rs` (live-bolt sidecar and effects), `run_tick_loop_with_weather` (chunk gate and fire ignition against the live world). Add effects via `LightningEffect`/`resolve_effect` and an arm in `apply_lightning_hits`.
- **Difficulty**: the gap is upstream chunk inhabited-time tracking.
- **Raids**: widening the occupied signal needs a live range query for workstation claims like beds have; health-based boss progress needs per-wave starting health.
- **Dragon/wither**: re-verify against the pinned decompile. Phase changes need a scripted-sequence transition test.

## Configuration

- `lodestone_render::DEFAULT_WEATHER_RADIUS` 10 (441 columns, below `HALF_RAIN_TABLE_SIZE`); `textures/environment/{rain,snow}.png` from `client.jar` (absent means no droplets).
- `LIGHTNING_STRIKE_SEED`/`LIGHTNING_BOLT_SEED` in `crate::lightning`, separate so a strike decision never shifts bolt rolls.
- Game rules `spawn_patrols` and `raids` (default `true`); `PATROL_TIMELINE_GATE`, `PATROL_SPAWN_SEED`, `PATROL_COMPANION_RANGE` (16) in `mobs.rs`; `RAID_ROLL_SEED` in `mobs/raid.rs`.
- Dragon and wither constants (`DRAGON_SPAWN_Y = 128`, `INVULNERABLE_TICKS = 220`, timers, powers) are named `const`s.

## Dependencies

- Weather: `lodestone-render` (`fog`, `light`, `Camera`), `lodestone-assets`, `lodestone-shell` (`net`, `resources`), `GAME_EVENT`/`ADD_ENTITY` on the v26 families.
- Lightning: `ChunkSource`, `SpawnRng`, `DifficultyInstance`, `crate::fire`, `crate::weather`.
- Raids and patrols: `MobSim::spawn_species`, `ChunkWorld::surface_y`, `BossBarSnapshot`, `PatrolRouteGoal`, `ai::roster::ranged::PILLAGER`; see [mob AI](mob-ai.md) and [villagers](villagers.md).
- Dragon: `lodestone_model::BlockPos` only. Wither: `lodestone_model::{BlockPos, Vec3, Difficulty}` and the shared projectile plumbing; see [projectiles](projectiles.md) and [entity physics](entity-physics.md).
