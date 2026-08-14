# Plan: world state — time, weather, sleeping, border, rules, difficulty, spawn, dimensions (epic #340)

## What it is

The implementation plan for epic #340's eight children (#323–#330): the server-authoritative
world-state systems, each planned end-to-end from ECS placement through the wire to a named client
consumer. Written 2026-08-04 against a verified tree; every "X doesn't exist" below was re-grepped
tree-wide, and three children turned out substantially landed since their issues were written.

## Verified current state (read this before trusting any issue body)

Staleness corrections, each confirmed against the tree — the issue bodies predate them:

- **T1's time work is substantially landed.** The server sends `SET_TIME` once at join
  (`encode_set_time(0, Some(0))`, `serve_connection_inner` in `crates/lodestone-server/src/server.rs`)
  and every second (`serve_play`'s `time_sync_tick` arm in `server.rs`, monotonic game time with an
  empty day-clock map — the correct 26.2 shape). What remains is exactly three things: the
  `game_time` on the wire is **wall-clock-since-join, per connection** (same `time_sync_tick` arm)
  rather than the tick loop's own counter — `tick::run_tick_loop_with_weather`'s `game_tick` never
  reaches the wire, rule 1's island in its purest form; there is no `advance_time` gamerule gate;
  and nothing persists world age (the open persistence-wiring dependency's hook).
- **R1's storage half landed** (`34725eb`): `WorldAdminState.game_rules:
  HashMap<String, String>` (`crate::server::WorldAdminState`, `server.rs`), serverbound
  `SET_GAME_RULE` decoded (the `SET_GAME_RULE` arm of `ServerProtocol::decode` in
  `crates/protocol/v770/src/server_protocol.rs`), applied (`apply_game_rule_changed` in
  `server.rs`), confirmed via `encode_game_rule_values` (`ServerProtocol::encode_game_rule_values`
  in `crates/lodestone-server/src/protocol.rs`), gated by
  `a_set_game_rule_is_validated_and_a_renamed_key_is_refused` in
  `crates/lodestone-server/tests/serve_play.rs`.
- **The difficulty half of R2's storage + wire round-trip landed** (`34725eb`, `eb05ebe`):
  `CHANGE_DIFFICULTY`/`LOCK_DIFFICULTY` decoded (the corresponding arms of `ServerProtocol::decode`
  in `server_protocol.rs`), applied into `WorldAdminState` (`apply_difficulty_change` in
  `server.rs`).
- **But `WorldAdminState` is per-connection** — constructed fresh per serve (each
  `serve_connection*` wrapper in `server.rs`). Two LAN clients each get their own difficulty and
  game rules. This is a shipped violation of `docs/server-ecs.md`'s never-straddle invariant, and
  it is R1/R2's problem to fix, not inherit.
- **`IntegratedServer::bind` (LAN) spawns no tick loop at all** — only `open_in_memory_with_mobs`
  spawns `run_tick_loop`. World state would be frozen for LAN.
- **Persistence: the anvil codec work is CLOSED** — `lodestone-anvil` landed with working `.mca`
  and `level.dat` codecs (`129f0bb`, `bbf27af`). **Wiring it into the server's save/load path is
  the open dependency with zero commits.** Every persistence hook below cites that dependency, and
  this plan does not design the format.
- **The server sends zero registry data** (`begin_configuration` sends only
  FINISH_CONFIGURATION) and **`lodestone-worldgen` has zero Nether/End code**
  (grep for `nether|the_end|end_islands` over `crates/lodestone-worldgen/src` is empty). Both
  block D1.
- **Spawn is hardcoded `(8, 100, 8)`** inline in `V770ServerProtocol::begin_play`
  (`crates/protocol/v770/src/server_protocol.rs`), mirrored by the shell
  (`crates/lodestone-shell/src/net.rs`); `GameLogin` is all literals (seed 0, game_type 0).
- **Client-side wire coverage** (per-packet audit): time, weather, game-mode and difficulty
  events are all consumed. **Five decode paths route to nowhere**: all six world-border events,
  `GameRulesChanged`, and `SpawnPositionChanged` are decoded and dropped —
  `event.rs`'s `route()` function (`crates/lodestone-model/src/event.rs`) is the authoritative fork
  (ingest / session / shell stream), and its own doc warns: never flip a route without landing
  the consumer in the same commit.
- **`DESIGN.md` §12.88's write-only-events list is stale** — `WeatherChanged` now folds into the
  shell's `WeatherCell` (`forward`'s `WeatherChanged` arm in `net.rs`, read in
  `app.rs::WindowApp::redraw`).

## The 26.2 rework: constants and mechanics (all cites into `.cache/mc/26.2/src/`)

**26.2 restructured time and game rules wholesale. A plan written from 1.21 memory is wrong.**

### Time

- Day/night is a registry-driven clock system, not `dayTime` on level data: two `WorldClock`s
  exist, `minecraft:overworld` and `minecraft:the_end` (`WorldClocks`, `world/clock/WorldClocks.java`).
  State per clock: `ClockState(long totalTicks, float partialTick, float rate, boolean paused)`
  (`ClockState`, `world/clock/ClockState.java`, defaults `0.0/1.0/false` at `ClockState.CODEC`),
  persisted as server-global SavedData `world_clocks` via `ServerClockManager`.
- Advance (`ServerClockManager.ClockInstance.tick`, `world/clock/ServerClockManager.java`):
  `partialTick += rate; fullTicks = floor(partialTick); partialTick -= fullTicks;
  totalTicks += fullTicks` — gated on the **global** `advance_time` rule
  (`ServerClockManager.tick`), driven from `MinecraftServer.tickChildren`
  (`MinecraftServer.java`). A separate per-level monotonic game time survives:
  `ServerLevel.tickTime()` (`ServerLevel.java`).
- Sync: heartbeat every 20 ticks carrying **game time only, empty clock map**
  (`MinecraftServer.tickChildren`'s 20-tick branch, calling
  `MinecraftServer.forceGameTimeSynchronization`'s empty `Map.of()`, both `MinecraftServer.java`).
  Full clock sync is event-driven only: on join/dimension change (`PlayerList.sendLevelInfo`,
  `PlayerList.java`), on `modifyClock` (`ServerClockManager.modifyClock`, `ServerClockManager.java`),
  on `advance_time` flip (`MinecraftServer.onGameRuleChanged`, `MinecraftServer.java`). Paused or
  rule-off sends `rate = 0.0` so the client freezes (`ServerClockManager.ClockInstance.packNetworkState`).
- Overworld timeline (`Timelines.bootstrap`, `world/timeline/Timelines.java`): period **24000**;
  markers `WAKE_UP_FROM_SLEEP=0`, `DAY=1000`, `NOON=6000`, `NIGHT=13000`, `MIDNIGHT=18000`; night
  window 12600–23401 (same method).

### Weather

- Server-global SavedData `minecraft:weather` (`WeatherData`'s `CODEC`/`TYPE`,
  `world/level/saveddata/WeatherData.java`), not per-dimension.
- Rolls (`ServerLevel`'s `RAIN_DELAY`/`RAIN_DURATION`/`THUNDER_DELAY`/`THUNDER_DURATION` constants,
  `ServerLevel.java`, bounds inclusive): `RAIN_DELAY = UniformInt(12000,180000)`,
  `RAIN_DURATION = (12000,24000)`, `THUNDER_DELAY = (12000,180000)`,
  `THUNDER_DURATION = (3600,15600)`. Cycle: `advanceWeatherCycle` (`ServerLevel.java`), gated on
  `advance_weather`.
- Interpolation ±0.01F/tick clamped [0,1] (`ServerLevel.advanceWeatherCycle`); on load, levels
  snap to 1.0 (`ServerLevel.prepareWeather`).
- Broadcast (`ServerLevel.advanceWeatherCycle`): `RAIN_LEVEL_CHANGE`/`THUNDER_LEVEL_CHANGE` are
  dimension-scoped; `START_RAINING`/`STOP_RAINING` go to all dimensions.
  `ClientboundGameEventPacket` ids (`network/protocol/game/ClientboundGameEventPacket.java`):
  start rain 1, stop rain 2,
  change game mode 3, rain level 7, thunder level 8, `LEVEL_CHUNKS_LOAD_START` 13. Wire form:
  unsigned byte + float.

### Sleep

- `sleepersNeeded = max(1, ceil(activePlayers * pct / 100.0))`, spectators excluded
  (`SleepStatus.sleepersNeeded`, `SleepStatus.update`, `server/players/SleepStatus.java`);
  deep-sleep threshold 100 ticks (`Player.SLEEP_DURATION`, `Player.isSleepingLongEnough`,
  `world/entity/player/Player.java`).
- Skip (`ServerLevel.tick`'s sleep-skip block, `ServerLevel.java`): when enough players sleep *and*
  deep-sleep, call `moveToTimeMarker(WAKE_UP_FROM_SLEEP)` — advance **forward to the next multiple
  of 24000** and zero `partialTick` (`ClockTimeMarker.resolveTimeToMoveTo`,
  `world/clock/ClockTimeMarker.java`); wake all; clear weather only if `advance_weather` **and**
  currently raining.
- Bed entry has no dedicated packet — it is a use-item-on-bed interaction. Checks
  (`ServerPlayer.startSleepInBed`, `server/level/ServerPlayer.java`): distance (`TOO_FAR_AWAY`),
  obstruction, monsters within ±8.0 horizontal / ±5.0 vertical AABB (same method, skipped in
  creative). `BedSleepingProblem` is a record in 26.2, not an enum (`Player.BedSleepingProblem`,
  `Player.java`). Wake is serverbound `PLAYER_COMMAND` (StopSleeping) — **decoded and discarded by
  our server today**.

### World border

- `WorldBorder` extends SavedData. `MAX_SIZE = 5.999997E7` (**not** 1.21's `5.9999968E7`),
  `MAX_CENTER_COORDINATE = 2.9999984E7` (`WorldBorder`'s own constants,
  `world/level/border/WorldBorder.java`); `damagePerBlock = 0.2`, `warningBlocks = 5` (same file's
  field declarations); safe zone 5.0.
- **Warning-time discrepancy, recorded so nobody re-derives it:** the field initializer says
  `warningTime = 15` but `WorldBorder.Settings.DEFAULT` carries **300** and
  `WorldBorder.applyInitialSettings` always overwrites — `ServerLevel.getWorldBorder()` always
  calls it (`ServerLevel.java`). Effective default: **300**. The 15 is dead code.
- Lerp: `lerpSizeBetween(from, to, ticks, gameTime)` (`WorldBorder.lerpSizeBetween`) →
  `WorldBorder.MovingBorderExtent`. Centre is not clamped on set; clamping happens at read time in
  the extents (`MovingBorderExtent.getMinX`/`getMinZ`/`getMaxX`/`getMaxZ`).
- Damage is **players-only**: `max(1, floor(-dist * damagePerBlock))` past the safe zone
  (`LivingEntity.baseTick`, `world/entity/LivingEntity.java`).
  `onSetDamagePerBlock`/`onSetSafeZone` listeners are never sent to the client
  (`PlayerList.addWorldborderListener`, `PlayerList.java`) — those two fields are server-internal.
- Ticked first in `ServerLevel.tick`: border → weather → sleep check (same method, in that order).

### Game rules

- Moved to `world/level/gamerules/GameRules.java`, registry entries
  (`BuiltInRegistries.GAME_RULE`), snake_case, **59 rules**, only `BOOL` and `INT` types.
  Renames that silently break old names:

  | 1.21 name | 26.2 key | default | `GameRules` constant (`GameRules.java`) |
  |---|---|---|---|
  | `doDaylightCycle` | `advance_time` | true (release) | `ADVANCE_TIME` |
  | `doWeatherCycle` | `advance_weather` | true (release) | `ADVANCE_WEATHER` |
  | `playersSleepingPercentage` | `players_sleeping_percentage` | 100 | `PLAYERS_SLEEPING_PERCENTAGE` |
  | `spawnRadius` | `respawn_radius` | 10 | `RESPAWN_RADIUS` |
  | `naturalRegeneration` | `natural_health_regeneration` | true | `NATURAL_HEALTH_REGENERATION` |
  | `doMobSpawning` | `spawn_mobs` | true | `SPAWN_MOBS` |
  | `randomTickSpeed` | `random_tick_speed` | 3 | `RANDOM_TICK_SPEED` |
  | `keepInventory` | `keep_inventory` | false | `KEEP_INVENTORY` |
  | `mobGriefing` | `mob_griefing` | true | `MOB_GRIEFING` |
  | `doImmediateRespawn` | `immediate_respawn` | false | `IMMEDIATE_RESPAWN` |

- **`GAME_RULE_VALUES` is request/response, not broadcast.** Its only send site is
  `sendGameRuleValues()` (`ServerGamePacketListenerImpl.sendGameRuleValues`,
  `ServerGamePacketListenerImpl.java`), reachable solely via
  `ServerboundClientCommandPacket.REQUEST_GAMERULE_VALUES` (the corresponding case in
  `ServerGamePacketListenerImpl.handleClientCommand`). Nothing pushes rule changes to clients —
  not even to the setter. Our current confirm-on-set (`apply_game_rule_changed` in `server.rs`)
  diverges from vanilla; see #327 below.
- Serverbound values arrive as strings, parsed server-side; permission gate
  `Permissions.COMMANDS_GAMEMASTER`; unknown keys logged and skipped, parse failures silently
  dropped (`ServerGamePacketListenerImpl.handleSetGameRule`, `ServerGamePacketListenerImpl.java`).

### Difficulty

- Stored in `LevelSettings.DifficultySettings` (`PrimaryLevelData`'s difficulty accessors,
  `PrimaryLevelData.java`); hardcore force-pins HARD (`MinecraftServer.setDifficulty`,
  `MinecraftServer.java`). Both packet handlers gate on gamemaster
  permission OR singleplayer owner; `handleLockDifficulty` has no else-branch, so unauthorized
  lock attempts are silently dropped — an asymmetry with `handleChangeDifficulty` worth
  preserving deliberately or diverging from deliberately, not by accident.

### Spawn

- Initial world spawn: `setInitialSpawn` (`MinecraftServer.java`) — spiral of
  `Mth.square(11)` = 121 iterations clamped to a ±5-chunk box (same method).
- Respawn scatter: `PlayerSpawnFinder` (replaces 1.21's `PlayerRespawnLogic`):
  `ABSOLUTE_MAX_ATTEMPTS = 1024`, candidates `min(1024, (radius*2+1)^2)` with a coprime-stride
  permutation, radius clamped by border distance, **adventure mode skips scatter**, and the
  search is **async, chunk-ticket driven** (`TicketType.SPAWN_SEARCH`, returns
  `CompletableFuture<Vec3>`) — a concrete tie to the chunk-lifecycle plan's ticket system.

### Portals and dimension change

- `PortalForcer`: nether search radius 16, overworld 128 (`PortalForcer.NETHER_PORTAL_RADIUS`/
  `OVERWORLD_PORTAL_RADIUS`, `world/level/portal/PortalForcer.java`), POI-based
  (`PoiTypes.NETHER_PORTAL`). `PortalProcessor` per-entity: `portalTime++` until
  `getPortalTransitionTime` (0 creative / 80 default,
  `PortalProcessor.processPortalTeleportation`), decays −4/tick when out
  (`PortalProcessor.decayTick`, `world/entity/PortalProcessor.java`). Nether
  `coordinate_scale = 8.0` (`DimensionTypes.bootstrap`, `data/worldgen/DimensionTypes.java`);
  `getTeleportationScale = oldScale / newScale` (`DimensionType.getTeleportationScale`,
  `world/level/dimension/DimensionType.java`).
- **Dimension-change packet sequence** (`ServerPlayer.teleport`, `server/level/ServerPlayer.java`),
  what #330 must reproduce: `RESPAWN` (data-to-keep byte 3) → `CHANGE_DIFFICULTY` →
  `PLAYER_ABILITIES` → `sendLevelInfo` (`PlayerList.sendLevelInfo`, `PlayerList.java`):
  `INITIALIZE_BORDER` → full clock sync → `SET_DEFAULT_SPAWN_POSITION` → weather game events if
  raining → `GAME_EVENT(13)` (chunks-load-start) → tick rate → player info/effects. Same-dimension
  teleport is a fast path with **no** respawn packet (same `ServerPlayer.teleport` method).

## ECS placement (per `docs/server-ecs.md`, referencing `docs/plans/server-ecs-migration.md`)

The server-ECS decision is final (`f0d22a1`) and **unimplemented** — no
`bevy_ecs`/`lodestone-ecs` in `crates/lodestone-server/Cargo.toml`, and no phase issues exist on
GitHub yet; the migration plan is being written concurrently to
`docs/plans/server-ecs-migration.md`. This plan therefore names the migration **shape** each
child needs, not a phase number:

- **Shape A — "the tick thread runs a schedule over a server `World` with `Resource`s."** The
  minimum: a `World` owned by the tick task, no lock, `GameTick` schedule.
- **Shape B — "connected players are server-`World` entities"** (the same shape that migrates
  `VITALS_TICK_INTERVAL`, per `docs/server-ecs.md`'s simulation-vs-replication table).
- **Shape C — "packet-apply runs as `GameTick` systems behind an `Adjudicate` set"** (the plugin
  veto window).

| child | state | ECS shape | schedule position (mirrors `ServerLevel.tick` order: border → weather → sleep → time) |
|---|---|---|---|
| #323 time | `WorldClocks` resource (map of clock → `ClockState{total_ticks, partial_tick, rate, paused}`) + per-level `game_time: u64` | `Resource` (A) | `GameTick`, after sleep check |
| #324 weather | `Weather{raining, thundering, clear/rain/thunder timers, rain_level, thunder_level, rng}` — server-global like vanilla | `Resource` (A) | `GameTick`, after border, before sleep |
| #325 sleep | `Sleeping{bed_pos, since_tick}` **component** on player entities; vote derived per tick | Component (B) | `GameTick`, after weather, before time |
| #326 border | `WorldBorder{center, size, lerp{from,to,duration,start}, warning_blocks, warning_time, damage_per_block, safe_zone}` | `Resource` (A); damage system needs player positions (B, or interim per-connection) | `GameTick`, first |
| #327 rules | `GameRules` typed registry resource | `Resource` (A) | no system of its own; read by others |
| #328 difficulty | `Difficulty{value, locked}` | `Resource` (A) | no system of its own |
| #329 spawn | `LevelSpawn{pos, yaw}` resource; `RespawnPosition{dim, pos, yaw, forced}` **component** per player | `Resource` + component (A for world spawn, B for per-player) | event-driven, not per-tick |
| #330 dimensions | `Dimensions` registry resource + per-dimension chunk sources; player dimension as component | `Resource` + component (A + B) | portal systems in `GameTick` |

**Blocked-on-shape summary:** S1 and P2 (the per-player respawn half) hard-require shape B. B1's
damage system wants B but has an honest interim (per-connection player position is where vitals
already live). Everything else needs only shape A — and until shape A lands, every `Resource`
above can live as a **plain struct owned by `run_tick_loop_with_weather`** (its local state,
exactly like its existing `game_tick`/`block_ticks` locals, both in `tick.rs`): same no-lock,
tick-thread-owned discipline, mechanical move to `Resource` later. **Do not build new
`Arc<Mutex<_>>` shared state for any of this** — that is the shape the migration exists to
delete. Publication to connection tasks uses the established snapshot-feed idiom
(`LiveMobSource` in `crates/lodestone-server/src/mobs/mod.rs` / `BlockTickFeed` in `tick.rs`),
with their documented single-consumer caveat.

Two structural gaps belong to the migration plan, cited here as dependencies, not planned:
`bind()` spawning no tick loop (LAN world state frozen), and per-connection `WorldAdminState`
(the straddle R1/R2 must not inherit — see unit R2).

## Plugin visibility

The owner's standing rule: plugins are ordinary bevy plugins, and core functionality goes through
the plugin surface — no separate internal API. Choosing `Resource`s in the server `World` makes
T1/W1/B1/R1/R2's state **plugin-visible by construction** for native server plugins
(`Res`/`ResMut` + system ordering); the Bukkit `World.setTime`/`setStorm`/`WorldBorder`/
`GameRule` surface is exactly these resources. What that leans on per child:

- **Write etiquette / veto**: ordering against an `Adjudicate` set — priority convention, monitor
  tier, cancellation semantics. Shape C, not blocking any child landing.
- **Observation as events**: the plugin message bus (a `GameEvent` message when weather flips or
  the border moves).
- **S1 sleep**: the cancellable "player enters bed" verb is the plugin surface's
  cancelable-action wrapper.
- **D1 dimensions**: plugin-registered dimensions are a separate plugin-surface concern; so are
  custom generators.
- **WASM tier**: none of this is reachable from sandboxed plugins until the capability ABI names
  query/action surfaces for it — cite per-resource entries there once it lands.
- **Commands** (`/time`, `/weather`, `/gamerule`, `/difficulty`, `/worldborder`,
  `/setworldspawn`): consumers of this state via the command-registration system; out of scope
  here.

## Units

Conventions: every unit names its consumer and its gate's negative control. New server modules
each need exactly one `mod` line in `crates/lodestone-server/src/lib.rs` (choke point — broker
through the orchestrator; the patch is the single stated line). `server.rs`, `tick.rs`,
`protocol.rs`, `event.rs`, `net.rs`, `app.rs` are contended: re-read before writing, stage hunks,
pathspec commits.

| unit | child issue |
|---|---|
| R1 | #327 |
| T1 | #323 |
| W1 | #324 |
| R2 | #327 + #328 |
| B1 | #326 (server half) |
| B2 | #326 (client half) |
| S1 | #325 |
| P1 | #329 (minus per-player respawn) |
| P2 | #329 (per-player respawn half) |
| D1 | #330 |

### R1 — typed game-rule registry (start immediately)

- **Files:** new `crates/lodestone-server/src/gamerules.rs` (owner: one agent); `lib.rs` gets
  `mod gamerules;`.
- **What:** typed registry of all 59 rules (BOOL/INT only), snake_case 26.2 keys, defaults per
  the table above (source: `gamerules/GameRules.java`); `get_bool`/`get_int` lookups; write path
  validates key against the registry and parses the string value, unknown keys logged-and-skipped,
  parse failures dropped (vanilla `ServerGamePacketListenerImpl.handleSetGameRule` semantics).
  Replaces the raw `HashMap<String,String>` inside `WorldAdminState` (`crate::server::WorldAdminState`,
  `server.rs`).
- **Consumer:** `apply_game_rule_changed` (`server.rs`) validates through it; units T1,
  W1, S1 read it. Until R2, it stays inside `WorldAdminState` (still per-connection — R2 fixes
  that, not this unit).
- **Permission:** vanilla gates on gamemaster; we have no permission model — keep current
  accept-all behaviour with a doc comment naming the open permission-model dependency, do not
  invent an interim model.
- **Vanilla-divergence decision recorded:** vanilla never pushes rule values (request/response
  only, `ServerGamePacketListenerImpl.handleClientCommand`'s `REQUEST_GAMERULE_VALUES` case calling
  `sendGameRuleValues`). Our confirm-on-set stays (it is what our
  test gates and is harmless), but add the `REQUEST_GAMERULE_VALUES` client-command path when the
  serverbound `CLIENT_COMMAND` decode grows that variant — check
  `server_protocol.rs`'s Play arms before assuming it is absent.
- **Gate:** `serve_play.rs` — set `random_tick_speed` to 0 via `SET_GAME_RULE`, run N ticks,
  assert zero random-tick block changes reach the feed; set 300, assert the grass-spread event
  count over the same N lands in a predicted band (magnitude, not sign — a "changed at all"
  assert is the vacuous species). Negative controls: an unknown key must not enter the map (and
  the log line must appear); a non-integer value for an INT rule must leave the old value intact.
  What would make it vacuous: asserting only on the confirm packet (round-trips the string
  without proving a reader exists).

### T1 — connect the clock to the wire, gate on `advance_time` (the remainder)

- **Files:** `tick.rs` (add `WorldClocks` plain-struct state to `run_tick_loop_with_weather`,
  advance per `ServerClockManager.ClockInstance.tick`); `server.rs`'s `serve_play` time-sync arm
  (`time_sync_tick`).
- **The island to close, precisely:** `serve_play`'s `time_sync_tick` arm derives `game_time` from
  wall clock; `run_tick_loop_with_weather` already counts real ticks (`TickClock::tick_count`) and
  the connection task already holds the `Arc<TickClock>`. The fix is one substitution: the
  periodic `encode_set_time(ticks_since(play_start), None)` reads `clock.tick_count()` instead.
  Overrun semantics come free (forgiven backlog = time genuinely didn't advance).
- **Gate:** integration test — run the loop N virtual ticks, assert the broadcast game time
  equals `tick_count` exactly; with `advance_time` off, the day-clock rate resent is 0.0 and the
  client-visible anchor frozen (join sync sends `rate: 0.0`, matching
  `ServerClockManager.ClockInstance.packNetworkState`). Negative control: with the rule on, the
  frozen-assert must fail. Vacuous-risk: asserting "time increased" (sign) instead of "equals
  tick_count" (magnitude).
- **Client:** none — `SET_TIME` → `TimeChanged` → `SharedState::apply`
  (`crates/lodestone-client/src/state.rs`) → `WorldTime` → sky already works end-to-end.
- **Persistence hook:** world age and clock states load/save via the persistence-wiring dependency
  (`world_clocks` SavedData shape); until then tick 0 = process start, documented.
- **Suggested issue hygiene:** per the owner's triage comment on T1's issue, close it and let the
  persistence-wiring dependency, R1's issue, and this unit carry the remainder.

### W1 — weather state machine + `encode_game_event`

- **Files:** new `crates/lodestone-server/src/weather.rs` (state machine, pure, seeded-RNG
  constructor); `lib.rs` `mod weather;`; `tick.rs` (tick it before sleep; publish transitions
  into a `WeatherFeed` mirroring `BlockTickFeed`); `protocol.rs` + `v770/server_protocol.rs`
  (new trait method `encode_game_event(kind: u8, value: f32)`); `server.rs` sync arm forwards
  drained transitions.
- **Mechanics:** exactly `ServerLevel.advanceWeatherCycle` with the four `UniformInt` ranges
  (`ServerLevel`'s `RAIN_DELAY`/`RAIN_DURATION`/`THUNDER_DELAY`/`THUNDER_DURATION` constants),
  ±0.01F/tick interpolation clamped [0,1] (same method), `advance_weather` gate (reads R1). Keep
  the interpolated levels — the issue's own trap warning stands, and the client's `WeatherCell`
  consumes the ramp.
- **Consumer (already wired):** client decodes GAME_EVENT → `WeatherChanged` → `forward`'s
  `WeatherChanged` arm in `net.rs` fold → `WeatherCell` → `app.rs::WindowApp::redraw`. Rain
  *particles/overlay rendering* is the Tier-1 backlog client item, explicitly not this unit.
- **Gate:** (a) seeded-RNG unit test pinning exact transition ticks for a known seed (expected
  values derived by hand from the UniformInt sampling, not from our own code); (b) duty-cycle
  distribution band over ~1M simulated ticks against the range midpoints; (c) wire gate: force
  rain on, assert bytes are id 7 with float ramping exactly 0.01/tick over 100 ticks 0→1.0
  (magnitude), then id 1/2 on flips. Negative control: with `advance_weather` off the state must
  not change over N ticks — and flipping the rule on must make that same assert fail.

### R2 — world-level admin state (the straddle fix, joining R1's game rules with the difficulty state)

- **What:** move `WorldAdminState` (difficulty, lock, game rules) from per-connection (each
  `serve_connection*` wrapper in `server.rs`) to tick-thread-owned world state, broadcast changes
  to **all** connections. This is the natural **pilot for migration shape A** — coordinate with
  `docs/plans/server-ecs-migration.md` before starting; if shape A is imminent, land this as the
  first real `Resource` instead of a plain struct, and do not build it twice.
- **Files:** `server.rs` (state relocation + broadcast), `tick.rs` or the new `World` (owner
  decided by migration timing). Difficulty extras: hardcore→HARD pin has no server-side hardcore
  flag yet — note in code, cite the anvil-persistence wiring dependency for where the flag will
  come from.
- **Gate:** two-connection test — connection A sets difficulty, connection B receives the
  `CHANGE_DIFFICULTY` broadcast. **This gate fails against today's tree by construction** (B has
  its own `WorldAdminState`), which is the negative control demonstrating the straddle is real:
  run it before the fix, watch it fail, land the fix, watch it pass. Same for a game-rule set
  observed by B's behaviour (not by a confirm packet — B never gets one, matching vanilla).
- **Permissions:** still no permission model. Record vanilla's lock-difficulty silent-drop
  asymmetry (`ServerGamePacketListenerImpl`) in the doc comment when the permission-model work
  lands the check.
- **Client:** none — `HudState::apply` (`crates/lodestone-game/src/player_state.rs`)
  already consumes difficulty.

### B1 — world border state + wire (server half)

- **Files:** new `crates/lodestone-server/src/border.rs`; `lib.rs` `mod border;`; `tick.rs`
  (tick first, per `ServerLevel.tick` order); `protocol.rs` + `v770/server_protocol.rs`: encoders
  for `INITIALIZE_BORDER` (sent in `begin_play`'s join sequence, mirroring
  `PlayerList.sendLevelInfo`) and the five `SET_BORDER_*` deltas; `server.rs` forwards a
  `BorderFeed`.
- **State/defaults:** the resource row in the table above with 26.2 defaults — size
  5.999997E7, damage 0.2, safe zone 5.0, warning blocks 5, warning time **300** (the
  `WorldBorder.Settings.DEFAULT` value; the field's 15 is dead — discrepancy recorded above so
  nobody "fixes" it backwards). Lerp per `MovingBorderExtent`; centre clamped at read, not write.
- **Enforcement:** players-only damage `max(1, floor(-dist * damage_per_block))`
  (`LivingEntity.baseTick`) past safe zone. Interim (pre-shape-B): compute in the
  connection task against its own player position and vitals, exactly where vitals live today —
  classified simulation-state-in-the-wrong-place, migrating with vitals.
- **Gate:** shrink via lerp, sample broadcast size at ticks {0, d/4, d/2, d} against the exact
  linear formula (magnitude, outside-derived); a player at distance d outside takes exactly
  `max(1, floor(d*0.2))` per tick on the vitals wire; a player inside the safe zone takes 0 —
  and the control proving the 0-detector works: move the same player outside and the 0-assert
  must fail.

### B2 — border client consumer (client half; pairs with B1, same PR train)

> **Stale as of a recent island sweep — re-verify before following this bullet.** The
> border events did **not** route to SHELL and there is no `BorderCell`. They route
> `session` (`event.rs`'s own table, `ClientEvent::WorldBorder* => SESSION`) and fold into
> `lodestone_ecs::session::SessionWorldBorder` via `apply_world_border`, stamped off
> `FrameClock`. So the "route flip" this section's *Rule respected* line turns on **already
> happened, differently**, and no `net.rs` change is needed at all.
>
> What is genuinely still open is only the consumer, and it is bigger than "tint the screen
> edge red": vanilla's warning is a **cyan tint on the vignette** inside `Hud.extractVignette`
> (`Hud.java`), needing `misc/vignette.png` plus a **multiply** blend state that
> `lodestone-render/src/screen_effects.rs` does not have. The formula itself is ported and
> gated (`sim::session::border_warning`) and reaches the debug overlay today. See
> `docs/screen-overlays.md`'s "The world-border warning" section for the exact remaining list.

- **Files:** `event.rs`'s `route()` route table (six border events → SHELL — world state, so
  neither `ingest` nor `session`, per the established fork); `net.rs` fold into a new `BorderCell`
  (mirror `WeatherCell`, `forward`'s `WeatherChanged` arm in `net.rs`); consumer: warning vignette
  — when
  `distance_to_border < max(warning_blocks, speed * warning_time_ticks)` tint the screen edge
  red (`hud.rs`/`gpu.rs` — exact anchor chosen by the implementing agent with the orchestrator,
  since both files are contended). The animated wall render is a follow-up render feature, filed
  separately — the vignette is the pixel-reaching minimum that keeps this off the island list.
- **Rule respected:** the route flip and the `BorderCell` consumer land in the same commit —
  `event.rs`'s own warning.
- **Gate:** pixel gate — with a border 10 blocks away and warning_blocks 5, no vignette; at 4
  blocks, red coverage inside the screen-edge rect above a threshold **with a bounding-box
  failure report** (location, never frame average); negative control: route flip reverted →
  the same gate must fail. Vacuous-risk: asserting the cell updated (state) without the draw.

### S1 — sleep and the night-skip vote (blocked on shape B + T1 + W1 + R1)

- **Files:** new `crates/lodestone-server/src/sleep.rs`; `lib.rs` `mod sleep;`; `server.rs`
  use-item-on-bed arm (bed detection in `apply_use_item_on`);
  `server_protocol.rs` PLAYER_COMMAND arm stops discarding StopSleeping.
- **Mechanics:** entry checks per `ServerPlayer.startSleepInBed` (distance, obstruction, monster
  AABB ±8/±5, creative skip); vote per `SleepStatus.sleepersNeeded` with the 100-tick deep-sleep
  threshold; skip = clock jump to next multiple of 24000 via T1's clock (zero `partial_tick`),
  wake all, clear weather iff `advance_weather` && raining (`ServerLevel.tick`'s sleep-skip block).
- **Why blocked:** the vote is over **all connected players** — per-connection sleeping flags
  cannot express it honestly for LAN; it needs players as shared state (shape B). Do not build a
  per-connection approximation; that is the straddle again.
- **Client gap, flagged not planned:** `EntityPose::Sleeping` has zero readers in the renderer
  and there is no sleep overlay UI — file as a client follow-up when this lands; without it the
  *other* players' sleeping is invisible (metadata already decodes).
- **Gate:** the issue's own scripted scenario, sharpened: N connections, exactly
  `sleepersNeeded - 1` sleep → no skip after 200 ticks (and this no-skip assert's control:
  add the Nth sleeper and the same assert must fail); Nth sleeps → skip fires only after the
  100-tick deep-sleep threshold, to exactly `ceil(t/24000)*24000`. Use
  `lodestone-testsupport::unique_username` per connection (offline-mode UUID trap).

### P1 — world spawn point (minus per-player respawn)

- **Files:** new `crates/lodestone-server/src/world_spawn.rs` (spiral search per
  `MinecraftServer.setInitialSpawn`: 121 iterations, ±5-chunk box, heightmap-based); `lib.rs`
  `mod world_spawn;`; **`protocol.rs` `begin_play` signature grows a spawn-position parameter**
  (choke: the trait change touches all `ServerProtocol` impls — exactly one exists, `v770`) —
  deleting the `(8,100,8)` literal in `V770ServerProtocol::begin_play`
  (`crates/protocol/v770/src/server_protocol.rs`); `server.rs` threads the chosen spawn through;
  new `SET_DEFAULT_SPAWN_POSITION` encoder in the join sequence.
- **Client half (same-commit pairing):** `SpawnPositionChanged` currently routes NOWHERE. Route
  → SHELL, fold into the session cell the death/respawn screen will read; the concrete consumer
  today is the shell's respawn-position display (and the F3-style debug row if the screen isn't
  built) — implementing agent names the exact anchor before flipping the route, same rule as B2.
- **Gate:** fixed-seed spawn search vs a real 26.2 server's chosen spawn for the same seed,
  exact match (the issue's own oracle; run against `scripts/live-oracles/terrain.sh`'s world or
  a fresh vanilla boot). Negative control: a seed whose origin column is ocean must move the
  spawn (asserting the search actually searched); vacuous if the test seed's spawn happens to be
  the origin.
- **Dependency:** chunk availability during the search — cite `docs/plans/chunk-lifecycle.md`
  (spawn-chunk tickets, planned separately). The search itself can run against
  `ChunkSource::column` synchronously today; the *keep-loaded* half is that plan's.

### P2 — per-player respawn points (blocked on shape B + death flow)

- Bed/anchor interaction sets `RespawnPosition` with vanilla validation; respawn placement uses
  `PlayerSpawnFinder` semantics (1024-attempt cap, `respawn_radius` rule via R1, adventure
  skips scatter, border-clamped radius). The async ticket-driven search ties to the chunk-lifecycle
  ticket plan — cite, don't plan. Persistence of respawn points is the open persistence-wiring
  dependency's job. Blocked additionally on a death→respawn server flow (the death screen sends a
  client command our server must answer with respawn) — verify the serverbound CLIENT_COMMAND
  decode before scoping, per rule 2.
- **Gate:** legal/illegal bed placements from captured scenarios (issue's own verification),
  plus: destroy the bed, die → respawn falls back to world spawn with the vanilla message.

### D1 — multi-dimension plumbing + portal travel (last, and honestly blocked)

- **Blocked on, cited plainly:** the zero-registry-data gap (server sends zero registry data —
  clients cannot even be told a second dimension exists), Nether/End worldgen (**does not exist**
  in `lodestone-worldgen`; a second large worldgen project, explicitly not planned here), and a
  `RESPAWN` encoder (none exists server-side).
- **Thin slice that is real once that registry-data gap is closed:** dimension registry resource +
  per-dimension chunk sources (`worldgen_data.rs`'s `BUNDLED_WORLDGEN_SCOPE`/
  `bundled_worldgen_serves` currently expose overworld only — a void-generator second dimension is
  enough to prove plumbing); player-dimension component; portal-frame detection/ignition as a pure
  function (testable against known-valid/invalid frames, no worldgen needed);
  `PortalProcessor`-style per-player timer; `encode_respawn` + the exact packet sequence from
  `ServerPlayer.teleport` (order matters and is easy to get wrong: respawn → difficulty →
  abilities → border → clocks → spawn pos → weather → game-event 13 → chunks; same-dimension
  sends no respawn packet).
- **Client half: already done** — RESPAWN is fully consumed (dimension visuals, fog, sky landed;
  `fc6b6c6`). #330 must not re-implement any client piece; the client is the oracle for the
  server's sequence.
- **Gate:** captured-bytes comparison of the full teleport sequence against a real 26.2 server
  (order-sensitive assert on the packet id sequence, not a set-membership assert — the set
  passes with a wrong order, which is the vacuous version); portal-frame detection table tests
  with both polarities.

## Order and blockers

```
now (no ECS dependency):      R1 → T1 → W1 → B1+B2 → P1
pilot for ECS shape A:        R2 (coordinate with docs/plans/server-ecs-migration.md)
after shape B (players):      S1, P2, B1's damage moves in-World
after #275 + worldgen call:   D1
```

- R1 first: T1/W1/S1 all read rules; it is pure and cheap.
- T1/W1/B1 are parallelizable across agents (disjoint new files; shared touches in `tick.rs`/
  `server.rs`/`protocol.rs` brokered as one-line-anchored patches).
- B2/P1-client pair with their server units in the same commit train (route-flip rule).
- R2 must not race the ECS migration's first phase — same state, one owner; the orchestrator
  sequences whichever lands first.
- Persistence for everything here is **the open persistence-wiring dependency**
  (level.dat/world_clocks/weather/border SavedData hooks); the anvil codec work is closed and
  `lodestone-anvil` is ready to be consumed.

## Top risks

1. **26.2's time/gamerule rework.** Any agent implementing from 1.21 memory will build `dayTime`
   on level data, camelCase rule names, and broadcast-on-change rule sync — all three wrong for
   26.2. The constants tables above are the antidote; gates must use outside-derived expected
   values (jar cites or live oracle), never `decode(encode(x))`.
2. **Racing the server-ECS migration.** Every unit above can land pre-ECS as tick-thread-owned
   plain state, but R2 and shape-B-dependent units overlap the migration's own first steps.
   Mitigation: R2 is explicitly the migration's pilot candidate; the orchestrator brokers which
   plan moves first, and no unit builds new locked shared state.
3. **Client islands.** Six border events, `GameRulesChanged`, and `SpawnPositionChanged` decode
   to nowhere today; B1/P1 server work without the paired B2/P1-client commits ships invisible
   state, and `event.rs`'s route table makes the flip look done when nothing consumes it. The
   pairing rule (route flip + consumer, one commit) is load-bearing; gates are pixel/behaviour
   gates, never cell-updated gates.
4. *(Recorded, owned elsewhere)* LAN `bind()` has no tick loop, and `WorldAdminState` is
   per-connection — both are migration-plan work; this plan's units must not paper over either
   with new per-connection state.
