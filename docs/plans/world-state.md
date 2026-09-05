# World state — time, weather, sleeping, border, rules, difficulty, spawn, dimensions

## What it is

The implementation plan for the server-authoritative world-state systems, each planned end-to-end
from ECS placement through the wire to a named client consumer.

## Verified current state

This plan is the detailed implementation reference for the rule registry, shared world state, and
clock synchronization. The remaining statements here identify production consumers and gaps that
still need a gate.

- **R1 (typed game-rule registry) is implemented and consumed, not merely stored.** `game_rules.rs`
  (896 lines) is a real typed registry — every 26.2 rule, jar-checked defaults, dozens of typed
  accessors (`GameRules::advance_time`, `::mob_griefing`, `::random_tick_speed`, etc.) — reachable
  through `crate::commands::gamerule` (`crates/lodestone-server/src/commands/gamerule.rs`) instead of
  an unvalidated per-connection `HashMap<String, String>`.
- **R2 uses one world-level state handle.** `WorldStateHandle`
  (`crate::world_state::WorldStateHandle`, `world_state.rs`, 893 lines) — a cheap-clone shared handle,
  the same shape as `BlockEntityHandle`, with **no per-connection copy**: `open_in_memory_with_mobs_using`
  constructs one `WorldStateHandle` and clones it into the connection task, the tick loop, and the host.
  Two LAN clients now share one difficulty and one game-rule set.
- **T1's periodic broadcast** sends
  `encode_set_time(game_time, Some(day_time))` reading `WorldStateHandle::tick_time`'s real counters,
  not wall-clock-since-join. `day_time` advances only when `advance_time` is on, `game_time`
  unconditionally — the reference asymmetry is implemented
  (`WorldStateHandle::tick_time`, `world_state.rs`).
- **`IntegratedServer::bind` (LAN) spawns a tick loop.** `bind` delegates to `open_to_lan`
  (`integrated.rs`), which starts the same `run_tick_loop` used by singleplayer. A running
  singleplayer world can be published in place, which is the pause menu's Open to LAN path.
- **W1 (weather) has a real consumer.** `weather.rs` (539 lines) implements the reference
  four bounded-random delay/duration ranges and ±0.01/tick interpolation, and a `WeatherFeed` carries
  transitions into `serve_play`'s sync arm exactly like `BlockTickFeed`. Client side, `WeatherChanged`
  folds into `WeatherCell` (`net.rs`, read in `app.rs::WindowApp::redraw`).
- **B1 (world border, server half) implements state, enforcement, and timed resize.**
  `border.rs` (742 lines) is a faithful `WorldBorder` port — centre, lerp, damage, warning fields — sent
  on join via `encode_initialize_border` and enforced via `PlayerVitals::apply_border_damage`
  (`max(1, floor(-dist * damage_per_block))`, matching vanilla's per-tick border-damage rule).
  `commands/worldborder.rs` registers `/worldborder set` and `add`; their optional duration calls
  `WorldBorder::lerp_size_between` through the shared `BorderFeed`. The tick loop calls
  `border.with(WorldBorder::tick)` before the remaining world tick, so a timed resize advances in
  production and reaches every client using that feed.
- **B2 (border client consumer) is wired through pixels:** the six border events route to SESSION
  (`event.rs`'s `route()`), fold into `SessionWorldBorder`, and feed
  `sim::session::border_warning`. One sampled tuple drives both the debug overlay and the required
  `misc/vignette.png` multiply-blend draw through `ScreenEffects`.
- **S1 (sleep) includes the wake packet.** `sleep.rs`
  (479 lines) implements the world-global vote (`SleepVote`) and the tick-owned skip arithmetic
  (`SleepState`), and `ServerBound::PlayerCommand`'s `STOP_SLEEPING` arm (action 0) is handled, not
  `Ignored`; the handler is the production consumer of the wake action.
- **P1 (world spawn) uses `world_spawn.rs` (1290 lines).** It replaces a hardcoded `(8, y, 8)`
  position with the reference 121-iteration spiral search
  (`find_initial_spawn`, mirroring the vanilla server's initial-spawn search), stored in
  `WorldStateHandle` and persisted to `level.dat`'s `spawn` compound, not re-derived per connection.
  `SpawnPositionChanged` routes to SESSION and reaches the debug overlay alongside the border fold.
- **P2 (per-player respawn) implements the read half.** `resolve_bed_respawn` implements the
  bed-respawn branch, walking the twelve bed
  stand-up offsets in the reference order — `PERFORM_RESPAWN` consults the stored `RespawnPoint`
  instead of using the world spawn unconditionally. The `respawn_radius` scatter and the async
  chunk-ticket search remain open and need shape B.
- **D1 includes production End terrain selection.** The server
  sends Configuration-phase registry data — all 29 of the registries the vanilla server
  synchronizes at Configuration phase ([Registries](../registries.md)), not zero. `lodestone-worldgen`
  has a real Nether generator (`crates/lodestone-worldgen/src/nether/`) wired into the server
  (`worldgen_data::nether_generator`, consumed by `chunk.rs`'s Nether column adoption), and
  `crates/lodestone-server/src/dimension.rs` plus `portal.rs` (1585 lines) implement multi-dimension
  chunk sources, 8:1 coordinate scaling, and portal travel. `encode_respawn` now
  exists server-side (`crates/lodestone-server/src/protocol.rs`, `crates/versions/26.2/src/server_protocol.rs`).
  `with_nether`'s sibling factory selects `worldgen_data::end_chunk_source(seed)` for
  `Dimension::End`; the resulting `EndChunkSource` is memoized as the dimensional sibling and
  receives its own tick loop on first use. The completed End portal frame triggers the travel path
  to this production source.
- **Client-side wire coverage:** all six world-border events, `GameRulesChanged`, and
  `SpawnPositionChanged` route to SESSION (`event.rs`'s `route()`) and fold into `SessionWorldBorder`/
  `SessionGameRules`/`SessionSpawnPoint`, reaching the debug overlay and (for `doImmediateRespawn`) real
  behaviour — `drive_ui_from_session` skips the death screen entirely when the rule is off.

## 26.2 constants and mechanics (behavior verified against the 26.2 vanilla server; naming below is descriptive, not a source citation)

**26.2 uses a clock registry and snake_case game rules. A plan written from 1.21 memory is wrong.**

### Time

- Day/night is a registry-driven clock system now, not a single elapsed-time field on the level: two
  clocks exist, keyed `minecraft:overworld` and `minecraft:the_end`. Each clock's state is a
  total-tick counter, a fractional per-tick carry, a rate multiplier, and a paused flag, defaulting to
  `0` ticks / `1.0` rate / not paused, persisted as server-global state (not part of any one dimension)
  under the key `world_clocks`.
- Advance, once per server tick, gated on the **global** `advance_time` rule: the fractional carry
  accumulates the rate, the whole number of ticks it crosses is extracted and folded into the
  total-tick counter, and the remainder carries forward — `carry += rate; whole = floor(carry);
  carry -= whole; total_ticks += whole`. A separate, per-level monotonic game-time counter survives
  independently of whether the day/night clock itself is paused.
- Sync: a heartbeat every 20 ticks carries the monotonic game-time counter only, with an **empty
  clock map**. Full clock sync is event-driven only: on join or dimension change, whenever a clock is
  modified directly (an admin/command action), and whenever the `advance_time` rule is flipped. A
  paused clock, or one with the rule off, is sent with rate `0.0` so the client-side clock visibly
  freezes.
- The overworld timeline has period **24000** ticks, with named markers at tick 0 (the
  wake-up-from-sleep point), 1000 (day start), 6000 (noon), 13000 (night start), and 18000
  (midnight); the "night" window for sleep purposes spans ticks 12600–23401.

### Weather

- Weather state is server-global persisted state keyed `minecraft:weather`, not per-dimension.
- The four roll ranges (bounds inclusive), each a uniform-random pick within its range: rain delay
  12000–180000 ticks, rain duration 12000–24000, thunder delay 12000–180000, thunder duration
  3600–15600. The whole cycle is gated on the `advance_weather` rule.
- The rain/thunder intensity levels interpolate at ±0.01/tick, clamped to [0,1]; on world load, an
  already-active weather state snaps straight to 1.0 rather than ramping in.
- Rain-level and thunder-level changes broadcast per-dimension; start/stop-raining broadcast to
  every dimension. On the wire, the generic "game event" packet carries a byte id plus a float value;
  the ids used here: start rain = 1, stop rain = 2, change game mode = 3, rain level = 7, thunder
  level = 8, chunks-load-start = 13.

### Sleep

- The number of sleepers needed to skip the night is `max(1, ceil(active_players * pct / 100.0))`,
  where spectators don't count toward `active_players`; a sleeper only counts once they've been in bed
  for the deep-sleep threshold of 100 ticks.
- Skip: once enough players are sleeping and past the deep-sleep threshold, the clock jumps forward
  to the next multiple of 24000 ticks (the next wake-up-from-sleep marker) and its fractional carry
  resets to zero; every sleeping player wakes; weather clears, but only if `advance_weather` is on and
  it was actually raining.
- Bed entry has no dedicated packet — it is a use-item-on-bed interaction. Entry checks: distance to
  the bed (too far away is one rejection reason), obstruction above the bed, and nearby monsters
  within a ±8.0 horizontal / ±5.0 vertical box (skipped in creative). The rejection-reason value
  changed shape in 26.2 — it now carries structured data rather than being a plain enum tag, which
  matters for anyone porting the exact wire encoding. Wake is the serverbound "stop sleeping"
  player-command action — **decoded and discarded by our server today**.

### World border

- Border state is server-global persisted state (not per-chunk). Max size is **5.999997E7** (**not**
  1.21's `5.9999968E7` — this changed between versions), max center coordinate is 2.9999984E7;
  default damage-per-block is 0.2, default warning distance is 5 blocks, safe zone is 5.0.
- **Warning-time discrepancy, recorded so nobody re-derives it:** one internal default constant
  reads 15, but the actual default settings record carries **300**, and every border is always
  initialized from that settings record — nothing ever reads the 15. Effective default: **300**; the
  15 is dead code in the vanilla source and should not be ported.
- A resize is expressed as (from-size, to-size, duration-in-ticks, start-game-time), and the current
  size at any moment is derived from those four values plus the current game time. The center is not
  clamped when it's set; clamping happens at read time, when computing the border's current min/max
  extents.
- Damage is **players-only**: `max(1, floor(-dist * damage_per_block))` once past the safe zone,
  applied every tick in the general per-entity tick update. Changes to the damage-per-block and
  safe-zone values are never sent to the client — those two fields are server-internal only.
- Ticked in this order, every server tick: border → weather → sleep check.

### Game rules

- Game rules are now registry entries (rather than a flat class of static fields), snake_case,
  **59 rules**, only boolean and integer types. Renames that silently break old (1.21-era) names:

  | 1.21 name | 26.2 key | default |
  |---|---|---|
  | `doDaylightCycle` | `advance_time` | true (release) |
  | `doWeatherCycle` | `advance_weather` | true (release) |
  | `playersSleepingPercentage` | `players_sleeping_percentage` | 100 |
  | `spawnRadius` | `respawn_radius` | 10 |
  | `naturalRegeneration` | `natural_health_regeneration` | true |
  | `doMobSpawning` | `spawn_mobs` | true |
  | `randomTickSpeed` | `random_tick_speed` | 3 |
  | `keepInventory` | `keep_inventory` | false |
  | `mobGriefing` | `mob_griefing` | true |
  | `doImmediateRespawn` | `immediate_respawn` | false |

- **The game-rule-values packet is request/response, not broadcast.** It is sent only in reply to an
  explicit client request for current rule values, part of the generic client-command packet. Nothing
  pushes rule changes to clients proactively — not even back to whoever changed the rule. Our current
  confirm-on-set (`apply_game_rule_changed` in `server.rs`) diverges from the reference behavior and
  must remain an explicit compatibility decision.
- Serverbound rule-set values arrive as strings, parsed server-side; gated on gamemaster-level
  permission; unknown keys are logged and skipped, and parse failures are silently dropped.

### Difficulty

- Difficulty and its lock flag are part of the level's stored settings; hardcore worlds force-pin
  difficulty to Hard regardless of what's set. Both the change-difficulty and lock-difficulty packet
  handlers gate on gamemaster permission or being the singleplayer world's owner; the lock handler has
  no fallback branch for a rejected request, so an unauthorized lock attempt is silently dropped — an
  asymmetry between the two handlers worth preserving deliberately or diverging from deliberately, not
  by accident.

### Spawn

- Initial world spawn search: a spiral search of 11² = 121 iterations, clamped to a ±5-chunk box.
- Respawn scatter (the search logic was rewritten between 1.21 and 26.2, though the shape of the
  result is the same): an absolute cap of 1024 attempts, candidate count `min(1024, (radius*2+1)^2)`
  visited via a coprime-stride permutation (not a plain scan), radius clamped by border distance,
  **adventure mode skips scatter entirely**, and the search runs **async, driven by the
  chunk-loading-ticket system** and returns a future — a concrete tie to the chunk-lifecycle plan's
  ticket system.

### Portals and dimension change

- Portal search radius: nether side 16 blocks, overworld side 128 blocks; portal frames are found via
  the point-of-interest system, not a raw block scan. Per-entity portal timer: a tick counter
  increments while standing in a portal until it reaches the transition time (0 in creative, 80 by
  default), and decays by 4/tick once the entity leaves the portal. The nether's coordinate scale is
  8.0 (blocks travelled in the overworld per block in the nether); the general teleport coordinate
  scale between any two dimensions is `old_dimension_scale / new_dimension_scale`.
- **Dimension-change packet sequence:** `RESPAWN` (data-to-keep byte 3) →
  `CHANGE_DIFFICULTY` → `PLAYER_ABILITIES` → then, as part of the same join-info sequence used for
  joining a level: `INITIALIZE_BORDER` → full clock sync → `SET_DEFAULT_SPAWN_POSITION` → weather
  game events if raining → `GAME_EVENT(13)` (chunks-load-start) → tick rate → player info/effects.
  Same-dimension teleport is a fast path with **no** respawn packet.

## ECS placement (per [The integrated and dedicated server](../dedicated-server.md), referencing `server-ecs-migration.md`)

This plan names the ECS migration **shape** each subsystem needs. The migration plan is
[`server-ecs-migration.md`](./server-ecs-migration.md); use it to verify current implementation
status before moving state into an ECS schedule:

- **Shape A — "the tick thread runs a schedule over a server `World` with `Resource`s."** The
  minimum: a `World` owned by the tick task, no lock, `GameTick` schedule.
- **Shape B — "connected players are server-`World` entities"** (the same shape that migrates
  `VITALS_TICK_INTERVAL`, per [The integrated and dedicated server](../dedicated-server.md)'s simulation-vs-replication table).
- **Shape C — "packet-apply runs as `GameTick` systems behind an `Adjudicate` set"** (the plugin
  veto window).

| subsystem | state | ECS shape | schedule position (reference per-tick order: border → weather → sleep → time) |
|---|---|---|---|
| time | a clock-registry resource (map of clock id → per-clock state: total ticks, fractional carry, rate, paused) + per-level `game_time: u64` | `Resource` (A) | `GameTick`, after sleep check |
| weather | `WeatherState{raining, thundering, clear/rain/thunder timers, rain_level, thunder_level, rng}` (already exists as a plain struct in `weather.rs`) — server-global like the reference | `Resource` (A) | `GameTick`, after border, before sleep |
| sleep | `Sleeping{bed_pos, since_tick}` **component** on player entities; vote derived per tick | Component (B) | `GameTick`, after weather, before time |
| border | `WorldBorder{center, size, lerp{from,to,duration,start}, warning_blocks, warning_time, damage_per_block, safe_zone}` | `Resource` (A); damage system needs player positions (B, or interim per-connection) | `GameTick`, first |
| rules | `GameRules` typed registry resource | `Resource` (A) | no system of its own; read by others |
| difficulty | `Difficulty{value, locked}` | `Resource` (A) | no system of its own |
| spawn | `LevelSpawn{pos, yaw}` resource; `RespawnPosition{dim, pos, yaw, forced}` **component** per player | `Resource` + component (A for world spawn, B for per-player) | event-driven, not per-tick |
| dimensions | `Dimensions` registry resource + per-dimension chunk sources; player dimension as component | `Resource` + component (A + B) | portal systems in `GameTick` |

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

Use "Verified current state" to identify which unit requirements remain. Each unit names a
production consumer, an exact gate, and a negative control; keep only requirements whose consumer
or gate is still absent.

Conventions: every unit names its consumer and its gate's negative control. New server modules
each need exactly one `mod` line in `crates/lodestone-server/src/lib.rs` (choke point — broker
through the orchestrator; the patch is the single stated line). `server.rs`, `tick.rs`,
`protocol.rs`, `event.rs`, `net.rs`, `app.rs` are contended: re-read before writing, stage hunks,
pathspec commits.

| unit | scope |
|---|---|
| R1 | typed game rules |
| T1 | clocks and time synchronization |
| W1 | weather |
| R2 | shared rules and difficulty |
| B1 | border server half |
| B2 | border client half |
| S1 | sleep |
| P1 | world spawn |
| P2 | per-player respawn |
| D1 | dimensions and portals |

### R1 — typed game-rule registry (start immediately)

- **Files:** new `crates/lodestone-server/src/gamerules.rs` (owner: one agent); `lib.rs` gets
  `mod gamerules;`.
- **What:** typed registry of all 59 rules (BOOL/INT only), snake_case 26.2 keys, defaults per
  the table above; `get_bool`/`get_int` lookups; write path validates key against the registry and
  parses the string value, unknown keys logged-and-skipped, parse failures dropped (matching
  vanilla's set-game-rule semantics). Replaces the raw `HashMap<String,String>` inside
  `WorldAdminState` (`crate::server::WorldAdminState`, `server.rs`).
- **Consumer:** `apply_game_rule_changed` (`server.rs`) validates through it; units T1,
  W1, S1 read it. Until R2, it stays inside `WorldAdminState` (still per-connection — R2 fixes
  that, not this unit).
- **Permission:** vanilla gates on gamemaster; we have no permission model — keep current
  accept-all behaviour with a doc comment naming the open permission-model dependency, do not
  invent an interim model.
- **Vanilla-divergence decision recorded:** vanilla never pushes rule values (request/response
  only — the client explicitly asks for current values and the server replies). Our confirm-on-set
  stays (it is what our test gates and is harmless), but add the request-values client-command path
  when the serverbound `CLIENT_COMMAND` decode grows that variant — check `server_protocol.rs`'s
  Play arms before assuming it is absent.
- **Gate:** `serve_play.rs` — set `random_tick_speed` to 0 via `SET_GAME_RULE`, run N ticks,
  assert zero random-tick block changes reach the feed; set 300, assert the grass-spread event
  count over the same N lands in a predicted band (magnitude, not sign — a "changed at all"
  assert is the vacuous species). Negative controls: an unknown key must not enter the map (and
  the log line must appear); a non-integer value for an INT rule must leave the old value intact.
  What would make it vacuous: asserting only on the confirm packet (round-trips the string
  without proving a reader exists).

### T1 — connect the clock to the wire, gate on `advance_time` (the remainder)

- **Files:** `tick.rs` (add `WorldClocks` plain-struct state to `run_tick_loop_with_weather`,
  advance per the vanilla per-tick clock formula above); `server.rs`'s `serve_play` time-sync arm
  (`time_sync_tick`).
- **The island to close, precisely:** `serve_play`'s `time_sync_tick` arm derives `game_time` from
  wall clock; `run_tick_loop_with_weather` already counts real ticks (`TickClock::tick_count`) and
  the connection task already holds the `Arc<TickClock>`. The fix is one substitution: the
  periodic `encode_set_time(ticks_since(play_start), None)` reads `clock.tick_count()` instead.
  Overrun semantics come free (forgiven backlog = time genuinely didn't advance).
- **Gate:** integration test — run the loop N virtual ticks, assert the broadcast game time
  equals `tick_count` exactly; with `advance_time` off, the day-clock rate resent is 0.0 and the
  client-visible anchor frozen (join sync sends `rate: 0.0`, matching vanilla's own frozen-clock
  encoding). Negative control: with the rule on, the frozen-assert must fail. Vacuous-risk:
  asserting "time increased" (sign) instead of "equals tick_count" (magnitude).
- **Client:** none — `SET_TIME` → `TimeChanged` → `SharedState::apply`
  (`crates/lodestone-client/src/state.rs`) → `WorldTime` → sky already works end-to-end.
- **Persistence hook:** world age and clock states load/save via the persistence-wiring dependency
  (`world_clocks` state shape); until then tick 0 = process start, documented.
- **Persistence boundary:** the persistence-wiring dependency owns durable clock state; until it
  lands, tick 0 is process start.

### W1 — weather state machine + `encode_game_event`

- **Files:** new `crates/lodestone-server/src/weather.rs` (state machine, pure, seeded-RNG
  constructor); `lib.rs` `mod weather;`; `tick.rs` (tick it before sleep; publish transitions
  into a `WeatherFeed` mirroring `BlockTickFeed`); `protocol.rs` + `v26-2/server_protocol.rs`
  (new trait method `encode_game_event(kind: u8, value: f32)`); `server.rs` sync arm forwards
  drained transitions.
- **Mechanics:** exactly the vanilla weather cycle described above — the four bounded-random
  delay/duration ranges, ±0.01/tick interpolation clamped [0,1], gated on `advance_weather` (reads
  R1). Keep the interpolated levels — abrupt state changes break the client ramp consumed by
  `WeatherCell`.
- **Consumer (already wired):** client decodes GAME_EVENT → `WeatherChanged` → `forward`'s
  `WeatherChanged` arm in `net.rs` fold → `WeatherCell` → `app.rs::WindowApp::redraw`. Rain
  *particles/overlay rendering* is the Tier-1 backlog client item, explicitly not this unit.
- **Gate:** (a) seeded-RNG unit test pinning exact transition ticks for a known seed (expected
  values derived by hand from the roll ranges above, not from our own code); (b) duty-cycle
  distribution band over ~1M simulated ticks against the range midpoints; (c) wire gate: force
  rain on, assert bytes are id 7 with float ramping exactly 0.01/tick over 100 ticks 0→1.0
  (magnitude), then id 1/2 on flips. Negative control: with `advance_weather` off the state must
  not change over N ticks — and flipping the rule on must make that same assert fail.

### R2 — shared rules and difficulty

- **Invariant:** `WorldStateHandle` owns one difficulty, lock flag, and typed rule registry for
  the world; connection tasks, the tick loop, and the host clone the same handle. Never introduce
  a per-connection copy while moving this state into ECS shape A.
- **Gate:** in a two-connection test, changing difficulty or a rule through connection A must be
  observable through connection B's shared world state and the appropriate wire behavior. A
  confirm packet alone is not a consumer. `HudState::apply` already consumes difficulty.
- **Permissions:** the future permission model must preserve the silent rejection of unauthorized
  difficulty-lock attempts.

### B1 — world border state + wire (implemented)

- **Production path:** `commands/worldborder.rs` registers the command surface; `set` and `add`
  select immediate `set_size` or timed `lerp_size_between` on the shared `BorderFeed`. `tick.rs`
  advances that feed with `WorldBorder::tick`; `protocol.rs` and `v26-2/server_protocol.rs` encode
  the join state and five deltas, and `server.rs` forwards the feed.
- **State/defaults:** the resource row in the table above with 26.2 defaults — size
  5.999997E7, damage 0.2, safe zone 5.0, warning blocks 5, warning time **300** (the real
  effective default; the field-initializer value of 15 is dead — discrepancy recorded above so
  nobody "fixes" it backwards). Lerp per the moving-extent formula above; centre clamped at read,
  not write.
- **Enforcement:** players-only damage `max(1, floor(-dist * damage_per_block))` (vanilla's
  per-tick damage rule) past safe zone. Interim (pre-shape-B): compute in the connection task
  against its own player position and vitals, exactly where vitals live today — classified
  simulation-state-in-the-wrong-place, migrating with vitals.
- **Gate:** shrink via lerp, sample broadcast size at ticks {0, d/4, d/2, d} against the exact
  linear formula (magnitude, outside-derived); a player at distance d outside takes exactly
  `max(1, floor(d*0.2))` per tick on the vitals wire; a player inside the safe zone takes 0 —
  and the control proving the 0-detector works: move the same player outside and the 0-assert
  must fail.

### B2 — border client consumer

- **Current state:** border events route to `session` and fold into
  `lodestone_ecs::session::SessionWorldBorder` via `apply_world_border`, stamped from
  `FrameClock`. The formula in `sim::session::border_warning` is sampled once per redraw; its
  strength feeds `ScreenEffects::border_warning_strength`, while the full tuple is reused by the
  debug overlay. `WarningBlocks` and `WarningTime` preserve the signed wire values while making
  the two units distinct after this fold; only `blocks()` and `seconds()` expose their values to
  the warning formula. No event-route or `net.rs` change is required.
- **Visual consumer:** positive strength draws `misc/vignette.png` through a dedicated multiply
  pipeline in `lodestone-render::screen_effects`; `RenderStats::border_warning_overlay_drawn`
  exposes whether the pass actually ran. The animated wall and ambient-light vignette are separate
  rendering work.
- **Gate:** formula tests distinguish the static and moving thresholds. Pure geometry tests pin the
  full-screen quad and clamped tint, a direct GPU test distinguishes multiply from alpha blending,
  and the shell pixel route compares positive strength against an executed zero-strength mutation
  while printing a differing-pixel bounding box on failure.

### S1 — sleep and the night-skip vote (blocked on shape B + T1 + W1 + R1)

- **Files:** new `crates/lodestone-server/src/sleep.rs`; `lib.rs` `mod sleep;`; `server.rs`
  use-item-on-bed arm (bed detection in `apply_use_item_on`);
  `server_protocol.rs` PLAYER_COMMAND arm stops discarding StopSleeping.
- **Mechanics:** entry checks per the vanilla bed-sleep rules above (distance, obstruction,
  monster AABB ±8/±5, creative skip); vote per the sleepers-needed formula above with the 100-tick
  deep-sleep threshold; skip = clock jump to next multiple of 24000 via T1's clock (zero
  `partial_tick`), wake all, clear weather iff `advance_weather` && raining.
- **Why blocked:** the vote is over **all connected players** — per-connection sleeping flags
  cannot express it honestly for LAN; it needs players as shared state (shape B). Do not build a
  per-connection approximation; that is the straddle again.
- **Client gap, flagged not planned:** `EntityPose::Sleeping` has zero readers in the renderer
  and there is no sleep overlay UI — file as a client follow-up when this lands; without it the
  *other* players' sleeping is invisible (metadata already decodes).
- **Gate:** N connections, exactly one fewer than
  the sleepers-needed count sleep → no skip after 200 ticks (and this no-skip assert's control:
  add the Nth sleeper and the same assert must fail); Nth sleeps → skip fires only after the
  100-tick deep-sleep threshold, to exactly `ceil(t/24000)*24000`. Use
  `lodestone-testsupport::unique_username` per connection (offline-mode UUID trap).

### P1 — world spawn point

- **Files:** new `crates/lodestone-server/src/world_spawn.rs` (the vanilla spiral search: 121
  iterations, ±5-chunk box, heightmap-based); `lib.rs`
  `mod world_spawn;`; **`protocol.rs` `begin_play` signature grows a spawn-position parameter**
  (choke: the trait change touches all `ServerProtocol` impls — exactly one exists, `v26-2`) —
  deleting the `(8,100,8)` literal in `V770ServerProtocol::begin_play`
  (`crates/versions/26.2/src/server_protocol.rs`); `server.rs` threads the chosen spawn through;
  new `SET_DEFAULT_SPAWN_POSITION` encoder in the join sequence.
- **Client half:** `SpawnPositionChanged` routes to SESSION and reaches the debug overlay. Any new
  respawn screen must consume that session state rather than adding a second route.
- **Gate:** fixed-seed spawn search vs a real 26.2 server's chosen spawn for the same seed,
  exact match (run against `scripts/live-oracles/terrain.sh`'s world or
  a fresh vanilla boot). Negative control: a seed whose origin column is ocean must move the
  spawn (asserting the search actually searched); vacuous if the test seed's spawn happens to be
  the origin.
- **Dependency:** chunk availability during the search — cite `docs/plans/chunk-lifecycle.md`
  (spawn-chunk tickets, planned separately). The search itself can run against
  `ChunkSource::column` synchronously today; the *keep-loaded* half is that plan's.

### P2 — per-player respawn points (blocked on shape B + death flow)

- Bed/anchor interaction sets `RespawnPosition` with vanilla validation; respawn placement uses
  the vanilla respawn-scatter semantics described above (1024-attempt cap, `respawn_radius` rule
  via R1, adventure skips scatter, border-clamped radius). The async ticket-driven search ties to
  the chunk-lifecycle ticket plan — cite, don't plan. Persistence of respawn points is the open
  persistence-wiring dependency's job. Blocked additionally on a death→respawn server flow (the
  death screen sends a client command our server must answer with respawn) — verify the
  serverbound CLIENT_COMMAND decode before scoping, per rule 2.
- **Gate:** legal/illegal bed placements from captured scenarios,
  plus: destroy the bed, die → respawn falls back to world spawn with the vanilla message.

### D1 — multi-dimension plumbing and portal travel (implemented)

- **Production path:** configuration registry synchronization, the Nether and End generators,
  dimension chunk sources, coordinate scaling, portal travel, and `RESPAWN` encoding are wired.
  `with_nether`'s sibling factory selects `end_chunk_source(seed)` for `Dimension::End`, and the
  completed End-portal-frame trigger travels to that memoized sibling source. The required packet
  sequence is `respawn → difficulty → abilities → border → clocks → spawn position → weather →
  game-event 13 → chunks`; same-dimension travel sends no respawn packet.
- **Client half:** RESPAWN is already consumed for dimension visuals, fog, and sky. Do not
  re-implement client handling; use it as the consumer for the server sequence.
- **Gate:** captured-bytes comparison of the full teleport sequence against a real 26.2 server
  (order-sensitive assert on the packet id sequence, not a set-membership assert — the set
  passes with a wrong order, which is the vacuous version); portal-frame detection table tests
  with both polarities.

## Order and blockers

```
now (no ECS dependency):      R1 → T1 → W1 → B1+B2 → P1
pilot for ECS shape A:        R2 (coordinate with docs/plans/server-ecs-migration.md)
after shape B (players):      S1, P2, B1's damage moves in-World
End selection and portal travel:   D1
```

- R1 first: T1/W1/S1 all read rules; it is pure and cheap.
- T1/W1/B1 are parallelizable across agents (disjoint new files; shared touches in `tick.rs`/
  `server.rs`/`protocol.rs` brokered as one-line-anchored patches).
- B2/P1-client pair with their server units in the same commit train (route-flip rule).
- R2 must not race the ECS migration's first phase — same state, one owner; the orchestrator
  sequences whichever lands first.
- Persistence for everything here is **the open persistence-wiring dependency**
  (level.dat/world_clocks/weather/border state hooks); the anvil codec work is closed and
  `lodestone-anvil` is ready to be consumed.

## Top risks

1. **26.2's time/gamerule rework.** Any agent implementing from 1.21 memory will build a single
   elapsed-time field on the level instead of the clock-registry model, camelCase rule names
   instead of snake_case, and broadcast-on-change rule sync — all three wrong for 26.2. The
   constants tables above are the antidote; gates must use outside-derived expected values (a
   live oracle), never `decode(encode(x))`.
2. **Racing the server-ECS migration.** Every unit above can land pre-ECS as tick-thread-owned
   plain state, but R2 and shape-B-dependent units overlap the migration's own first steps.
   Mitigation: R2 is explicitly the migration's pilot candidate; the orchestrator brokers which
   plan moves first, and no unit builds new locked shared state.
3. **Client islands.** A session update is not a rendered feature. The border path closes through a
   concrete vignette draw and pixel gate; game-rule and spawn work must meet the same standard rather
   than stopping at a cell update.
