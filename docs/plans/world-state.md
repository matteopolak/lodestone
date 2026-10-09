# World state — time, weather, sleeping, border, rules, difficulty, spawn, dimensions

## What it is

Reference mechanics and constants for the server-authoritative world-state systems (clocks, weather, sleep, border, game rules, difficulty, spawn, portals), plus open work. The systems are implemented; this doc keeps the numbers a port must match.

## How it works

One `WorldStateHandle` (`crate::world_state`) holds difficulty, the lock flag, game rules, clocks and
spawn. Connection tasks, the tick loop and the host clone the same handle; never add a per-connection
copy. Weather and border transitions reach connections through `WeatherFeed` and `BorderFeed`, the
same snapshot-feed idiom as `BlockTickFeed`.

Modules: `game_rules.rs` (typed registry), `weather.rs`, `border.rs`, `sleep.rs`, `world_spawn.rs`,
`dimension.rs` and `portal.rs`. Per-tick order is border, weather, sleep, time.

### Time

- Two clocks, `minecraft:overworld` and `minecraft:the_end`, each with total ticks, a fractional carry,
  a rate and a paused flag. They persist server-globally under `world_clocks`.
- Advance per tick, gated on `advance_time`: `carry += rate; whole = floor(carry); carry -= whole;
  total_ticks += whole`. The per-level game-time counter advances regardless.
- A heartbeat every 20 ticks carries game time only, with an empty clock map. Full clock sync is sent on
  join, dimension change, direct clock edits and when `advance_time` flips. A paused or rule-off clock is
  sent with rate `0.0`.
- The overworld period is 24000. Markers: 0 wake-up, 1000 day, 6000 noon, 13000 night, 18000 midnight.
  The sleep "night" window is ticks 12600-23401.

### Weather

- Server-global, keyed `minecraft:weather`. Gated on `advance_weather`.
- Uniform rolls (inclusive): rain delay 12000-180000, rain duration 12000-24000, thunder delay
  12000-180000, thunder duration 3600-15600.
- Levels interpolate at 0.01/tick clamped to [0,1]; active weather on load snaps to 1.0.
- Game-event ids: start rain 1, stop rain 2, change game mode 3, rain level 7, thunder level 8,
  chunks-load-start 13. Level changes broadcast per dimension; start/stop to every dimension.

### Sleep

- Sleepers needed: `max(1, ceil(active_players * pct / 100.0))`; spectators are not active. A sleeper
  counts after 100 ticks in bed.
- Skip: clock jumps to the next multiple of 24000, carry resets, everyone wakes, weather clears only if
  `advance_weather` is on and it was raining.
- Bed entry is a use-item-on-bed interaction. Checks: distance, obstruction, monsters within +-8
  horizontal / +-5 vertical (skipped in creative). Wake is the serverbound stop-sleeping player command.

### World border

- Server-global. Max size `5.999997E7`, max centre `2.9999984E7`, damage 0.2 per block, warning
  distance 5 blocks, safe zone 5.0.
- Default warning time is **300**. A stray constant of 15 exists in the reference and is never read;
  do not port it.
- A resize is (from, to, duration ticks, start game time); current size derives from those and the
  current game time. The centre is clamped at read, not at set.
- Damage is players-only: `max(1, floor(-dist * damage_per_block))` past the safe zone, every tick.
  Damage and safe-zone values are never sent to clients.

### Game rules

59 rules, boolean and integer only, snake_case keys:

| old name | key | default |
|---|---|---|
| `doDaylightCycle` | `advance_time` | true |
| `doWeatherCycle` | `advance_weather` | true |
| `playersSleepingPercentage` | `players_sleeping_percentage` | 100 |
| `spawnRadius` | `respawn_radius` | 10 |
| `naturalRegeneration` | `natural_health_regeneration` | true |
| `doMobSpawning` | `spawn_mobs` | true |
| `randomTickSpeed` | `random_tick_speed` | 3 |
| `keepInventory` | `keep_inventory` | false |
| `mobGriefing` | `mob_griefing` | true |
| `doImmediateRespawn` | `immediate_respawn` | false |

- The reference sends rule values only on request; our confirm-on-set diverges deliberately.
- Serverbound sets arrive as strings, need gamemaster permission, skip unknown keys and silently drop
  parse failures.

### Difficulty

Difficulty and its lock live in level settings; hardcore pins Hard. Change and lock both require
gamemaster or the singleplayer owner. An unauthorised lock is silently dropped; preserve that
asymmetry deliberately.

### Spawn

- Initial spawn: 11x11 spiral (see [world-spawn](../world-spawn.md)).
- Respawn scatter: `min(1024, (radius*2+1)^2)` candidates by a coprime-stride permutation, radius clamped by border distance, skipped in adventure mode, asynchronous on chunk tickets.

### Portals and dimension change

- Search radius: Nether side 16, overworld side 128, found through points of interest. Portal timer
  reaches 80 ticks (0 in creative) and decays 4/tick outside. Nether scale 8.0; general scale is
  `old_scale / new_scale`.
- Dimension-change packet order: respawn (data-to-keep 3), difficulty, abilities, border, clocks,
  spawn position, weather events if raining, game event 13, tick rate, player info. Same-dimension
  teleport sends no respawn.

## How to change it

- Keep state tick-thread-owned or in `WorldStateHandle`; do not add new `Arc<Mutex<_>>` world state.
- A new rule needs a registry entry in `game_rules.rs` and a production reader; a stored but unread rule
  is an island.
- Test against outside-derived values (live oracle or hand arithmetic), not `decode(encode(x))`.

## Open work

- `respawn_radius` scatter and the asynchronous chunk-ticket respawn search (bed respawn itself works).
- Persistence of respawn points and per-player state beyond what `level.dat` carries.
- Renderer support for the sleeping pose and the sleep overlay, so other players' sleeping is visible.
- Plugin-visible hooks (veto window, events) for these resources.

## Dependencies

[Registries](../registries.md) for the 29 configuration-phase registries, [server ECS migration](./server-ecs-migration.md) for where this state is headed.
