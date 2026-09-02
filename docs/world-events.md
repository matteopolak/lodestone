# World events

## What it is

Server-driven world state that is not tied to any one player: rain/thunder weather and
lightning strikes, the regional-difficulty scalar that scales spawns and damage over
time, Bad-Omen-triggered raids and pillager patrols, and the ender dragon and wither
boss fights. All of it lives in `lodestone-server`, with a thin render-side consumer in
`lodestone-render`/`lodestone-shell` for weather and lightning's visible effects.

## How it works

### Weather

The server owns weather entirely and sends four `GAME_EVENT` codes —
`START_RAINING`/`STOP_RAINING`/`RAIN_LEVEL_CHANGE`/`THUNDER_LEVEL_CHANGE` — carrying
two scalars in `0.0..=1.0` (rain level, thunder level). Vanilla's own weather-cycle
advance
ramps the rain level by ±0.01 per tick, so the client folds updates into a shared cell
(`net::WeatherCell`, two atomics) rather than queuing them — latest wins, never queue,
the same shape as `net::SharedHandle`. `WeatherTracker::state()` is read once per frame
and drives angled rain/snow quads (inside the block pass), sky/horizon/fog darkening
(`app.rs`'s `desired_fog`), the existing lightmap `sky_darken` lane, and a lightning
flash — sky tint plus full-bright lightmap for 250 ms (5 ticks @ 20 tps) — triggered by
an `ADD_ENTITY` for `lightning_bolt` bumping a sequence number.

Rain vs. snow is decided per column from the block's biome climate
(`has_precipitation`/`temperature`/`downfall`, decoded off `registry_data` into
`ClientEvent::BiomeClimates`) against vanilla's threshold: height-adjusted temperature
`>= 0.15` is rain, below is snow (vanilla's own warm-enough-to-rain check). `isRaining`/`isThundering`
are `rain > 0.2`/`thunder > 0.9`; the effective thunder level used everywhere is
`raw_thunder × rain` (vanilla's own effective-thunder-level accessor), never the raw wire value alone.

Rain/snow droplets, fog/sky/lightmap darkening and the lightning flash all reach
pixels. Not reaching pixels: rain ambience sound (no play hook on `Sim`'s audio), the
bolt's own procedural geometry (a seeded branching quad strip — unbuilt), and
locally-predicted rain splash particles (a server-sent one draws fine; a local one
needs per-column terrain height/`canSeeSky` this client doesn't track).

Load-bearing constants: rain/snow max alpha `1.0`/`0.8`; distance fade
`lerp(min(d²/r², 1), max_alpha, 0.5) * intensity`; sky rain darken `×(1 − r·0.5, 1 −
r·0.5, 1 − r·0.4)`; sky thunder darken `×(1 − t·0.5)` all channels; `SKY_LIGHT_FACTOR`
floor `0.24`; lightning flash lerps `0.22` toward `(204, 204, 255)` and forces
`SKY_LIGHT_FACTOR` to `1.0`. All colour ops are in **gamma** space — doing them in
linear light pulls every factor toward 1.0 and washes the image out.

### Lightning

`should_attempt_strike` (vanilla's own per-tick-thunder outer gate) is checked once per
ticking chunk per tick, gated on `weather.thundering` before the per-chunk loop runs.
`find_lightning_target_around` picks a nearby lightning rod if one exists, otherwise a
random living/sky-visible entity in a column, otherwise the terrain heightmap. A
decided strike spawns a live bolt (life/flash countdown ported from
vanilla's own per-tick bolt update) streamed as `minecraft:lightning_bolt` with empty metadata
(vanilla's own `defineSynchedData` is empty), reaching the client's existing spawn arm
that drives the sky flash above.

`resolve_effect` is the `thunderHit` dispatch table: default 5.0 damage + 8s ignite;
creeper takes the default only (no "charged" state modelled); pig → zombified
piglin; villager → witch; mooshroom variant guarded per-bolt but never flips (no
red/brown variant exists); turtle takes a lethal hit, no ignite. The "skeleton horse
trap" is *not* a `thunderHit` effect — it's a flag rolled at horse spawn time
(reading `effective_difficulty()` directly) that a later AI goal turns into a
cosmetic bolt plus a skeleton posse. Not modelled: the lightning-rod power pulse and
copper-waxing reset, setting a struck mob alight (no burn state on `SimMob`), players
as `thunderHit` candidates, and lightning rods themselves (no POI manager exists, so
the rod search always misses).

### Regional difficulty

`DifficultyInstance` is a pure formula computing a scalar roughly `0.0`–`6.75`,
resolved fresh once per tick by `run_tick_loop` from world difficulty, elapsed world
time, a chunk's inhabited time, and moon phase — nothing persists an instance, only a
per-query recomputation. Distinct from **world difficulty** (`/difficulty`,
peaceful/easy/normal/hard), which it's derived from rather than being a setting
itself. `calculate_difficulty`: Peaceful is always `0.0`. Otherwise start from base
scale `0.75`, add up to `0.25` as total world time passes a 72,000-tick offset (capped
at 1,440,000 ticks), add a local term from the chunk's inhabited time (capped at
3,600,000 ticks, weighted `1.0` on Hard / `0.75` otherwise, halved again on Easy), add
a moon-phase term **clamped by the global term, not by `1.0`** (the one clause easy to
get backwards, indexed by `(day_time / 24000) % 8`, day 0 full moon), then multiply
the total by the difficulty's ordinal (Peaceful 0 … Hard 3).

Chunk inhabited time is not tracked anywhere yet — it's hardcoded to `0` on save, so
the local term always understates the true result. Real consumers: zombie/skeleton
spawn-with-gear chance and door-breaking coin flip, zombie reinforcement calls, and
the skeleton-horse-trap roll. Spawn-cap arithmetic is **not** difficulty-scaled in
vanilla at all — there's no real formula to port there.

### Raids and patrols

**Patrols** (`MobSim::run_patrol_spawn_cycle` + `LongDistancePatrolGoal`) are the
simpler, always-on half: a pillager group spawns near a random player roughly every
12000–13200 ticks once `MobSim::tick_count` passes 120000 (vanilla's
`can_pillager_patrol_spawn` timeline keyframe, transcribed as the constant
`PATROL_TIMELINE_GATE` rather than a general timeline engine). One member is the
leader; every tick with idle navigation, a member paths toward a waypoint rotated 90°
around Y from itself to its target, shrunk to two-fifths and re-centred — the lateral
wobble that keeps a march looking like a loose line. The leader is the *slower* of the
two speeds (`0.595` vs. a follower's `0.7`), so stragglers can close the gap, and
repicks a far-off target (`-500..500` on both axes) once within 10 blocks of its
current one. Vanilla's followers track the leader via a nearby-entity census; this
crate's `MobController` seam has no such primitive, so followers instead *pull* the
nearest leader's long-distance target once per tick (`MobSim::feed_perception`) — a
looser cluster that doesn't go stale outside a radius. Group size uses a fixed table
per `Difficulty` (Easy 2, Normal 3, Hard 4) rather than vanilla's continuous
`ceil(effective_difficulty) + 1` (no accumulated difficulty or moon phase tracked at
the patrol site).

**Raids** (`MobSim::start_raid`/`tick_raids`) are the escalating, triggered half. The
trigger chain is real end to end: a `minecraft:bad_omen` carrier within 64 blocks of an
occupied `#village` POI is converted to `minecraft:raid_omen` at the same amplifier
(`absorb_raid_omen`, clamped `1..=5`); on Raid Omen's last tick,
`MobSim::create_or_extend_raid` averages the occupied POIs into a centre and either
extends an ongoing raid within 96 blocks (vanilla's own raid-lookup radius) or starts a new
one. The occupied-POI signal currently only sees claimed villager beds (an in-memory,
session-only ledger) — job sites and the meeting bell aren't a live query yet, so a
village with claimed workstations but no claimed bed won't trigger a raid. Granting
Bad Omen from an ominous bottle isn't modelled, so a raid today needs a debug
`/effect give`.

Wave counts are copied verbatim from vanilla's own wave-group-count formula (Peaceful 0 / Easy 3 / Normal
5 / Hard 7) and its own per-raider-type spawns-per-wave-before-bonus table: pillager `[0, 4, 3, 3, 4, 4, 4,
2]`, vindicator `[0, 0, 2, 0, 1, 4, 2, 5]`, indexed by wave, plus a difficulty-scaled
bonus roll (`nextInt(2)` Easy, flat `1` Normal, flat `2` Hard, then `nextInt(bonus +
1)`). Each wave clears on live health checks, advances on a 300-tick cooldown, and
reaches Victory with vanilla's 40-tick post-clear delay. Only pillagers and
vindicators spawn — no other raider species exist in this crate's roster.
Vanilla's own raid-loss status (needing the village section-distance tracker) is not ported, so a raid
can only reach Ongoing or Victory; the 48000-tick (40-minute) overall timeout is
ported, so an abandoned raid still stops being tracked. The boss bar reuses the
existing `BOSS_EVENT` path; its progress is wave count, not vanilla's living-raider
health sum. The captain marker is data-only — no mob here carries server-side
equipment state, so neither the captain's nor the patrol leader's banner is drawn.

### Dragon fight

The state machine (`crates/lodestone-server/src/dragon/`) is pure — no world, entity,
or packet. `dragon::phase` is an eleven-phase machine matching `EnderDragonPhase`'s
wire-value ordering (`PhaseManager::tick` takes a `DragonInputs` struct and returns an
optional `PhaseEffect`); `dragon::crystal` heals 1.0 HP once every 10 ticks while a
live nearest crystal exists and health is below max (a proc, not a smeared rate,
rescanning on a `nextInt(10) == 0` roll); `dragon::fight` holds the persisted flags,
world-load scan, boss-bar value, exit-portal geometry and the four-stage respawn
sequence `Start → PreparingToSummonPillars → SummoningPillars → SummoningDragon →
End`. The one deliberate substitution: vanilla drives most phase transitions off a
node-graph path search over a fixed 12-node ring, which this codebase's
ground-oriented flying-mob AI can't do; "has the current flight leg finished" is
instead a per-tick input. Every other condition — health thresholds, crystal counts,
timers, RNG rolls, hurt amounts — uses vanilla's own numbers.

The whole chain is wired: `MobSim::init_end_dragon_fight` spawns the ten end crystals
atop seed-derived spike positions (two of ten iron-bars-caged, for any seed), spawns
the dragon, and returns the arena's block writes for the join path to apply — gated
by an atomic `claim_dragon_fight_start` so only one of several racing connections
performs the init. This gate is
**process-lifetime only** — a restart re-arms it and the arena is placed again.
`tick_dragons` drives phase/crystal ticking every tick; player melee/arrows route
into `damage_dragon` (a dragon lives outside `self.mobs`, so it needs its own attack
branch, same shape the wither uses); an actual kill places the egg (first kill only),
activates the exit portal, and pops a shuffled gateway slot to place a real but
non-functional (no teleport-on-contact) `minecraft:end_gateway`. The boss bar needs
no new wire path. Missing: the summoning beam (`EndCrystal.DATA_BEAM_TARGET` streams
as `None`, nothing computes a real target) and a darken-screen bit on
`BossBarSnapshot`. Fight state and the gateway pool are process-lifetime only, not
saved.

### Wither fight

Mirrors the dragon's split. `crate::wither` is the pure state machine: a 220-tick
invulnerable "emerging" phase (`10.0` HP heal every 10 ticks, `1.0` HP every 20 ticks
once active), a `7.0`-power emergence blast the tick invulnerability ends, a
powered-armor gate blocking arrow/wind-charge damage outright at `health <= max/2`,
and the skull's own numbers (`8.0` damage with a living owner, `1.0`-power impact
blast on any surface, `5.0` HP owner heal on a kill, Normal/Hard wither-effect
durations). `crate::mobs::wither` is the `MobSim` integration: a structure matcher
over soul sand/soil plus three wither skulls (its own module, since the wither's
block alphabet doesn't fit the golem's cell enum), `tick_withers` (emergence
countdown, heal ticks, a single skull-firing schedule), and `damage_wither`.

The wither is a plain `HashMap<i32, TrackedWither>`, not a goal-driven `SimMob` — same
shape and reason as the dragon (no aerial pathfinder) — but unlike the dragon it
doesn't move at all. The summon ritual, damage routing, the skull's full impact
chain and the boss bar are all wired to production the same way as the dragon fight.
Missing: the two side heads' aim state (the invulnerability flag itself is wired), a
darken-screen bit on `BossBarSnapshot`, a skull hitting a *different* live wither
applying the effect or healing its shooter, and real difficulty threading for the
wither-effect duration (currently always Normal).

## How to change it

* **Weather**: constants/geometry in `lodestone-render/src/weather.rs` (pure, no GPU);
  the pass in `weather_pipeline.rs` + `shaders/weather.wgsl`; wire→state in `net.rs`'s
  `forward`/`WeatherCell`; per-frame composition in `app/weather.rs`'s
  `WeatherTracker`/`ShellWeatherProbe` and `app/redraw.rs`'s `desired_fog`. The pass
  must not write depth (overlapping columns would punch holes in each other) and uses
  reversed-Z, matching the rest of this renderer. The animation phase must be driven
  by the tick clock, never frame time. Don't "fix" `START_RAINING`/`STOP_RAINING`'s
  `0.0`/`1.0` polarity — it's vanilla's own inversion, corrected by the next
  `RAIN_LEVEL_CHANGE`. Always read the *composed* thunder level (`raw × rain`), never
  the raw wire field, or a stale value sent on every join blacks out a clear sky.
* **Lightning**: `lightning.rs` owns target selection, the bolt state machine and the
  `thunderHit` table (pure); `mobs/lightning.rs` owns the live-bolt sidecar and
  entity-effect application; `run_tick_loop_with_weather` owns the per-chunk gate call
  and fire-ignition application (against the *live* world, since `MobSim::world` is a
  frozen snapshot). Add a species effect via `LightningEffect`/`resolve_effect` plus a
  matching arm in `apply_lightning_hits`.
* **Regional difficulty**: the formula shouldn't need touching; the one real gap is
  upstream — chunk inhabited-time tracking.
* **Raids/patrols**: widening the occupied-POI signal past beds needs a live range
  query for workstation claims, mirroring the one bed claims already have. Real
  health-based boss-bar progress needs `Raid` to record each wave's starting health.
* **Dragon/wither fights**: each function cites the vanilla symbol it ports by class
  and method — re-verify against the decompile under `.cache/mc/26.2/` rather than
  trusting a paraphrase. A new phase/emergence transition needs a transition-table
  test (a scripted input sequence asserting the resulting *sequence*), not a single
  "some phase changed" assertion.

## Configuration

* Weather: `lodestone_render::DEFAULT_WEATHER_RADIUS` (10 columns each way = 441
  columns; must stay below `HALF_RAIN_TABLE_SIZE`). `textures/environment/{rain,
  snow}.png` from `client.jar` — absent means no droplets, but darkening still works.
* Lightning: `LIGHTNING_STRIKE_SEED` / `LIGHTNING_BOLT_SEED` (`crate::lightning`) —
  fixed `SpawnRng` seeds, kept separate so a strike decision never shifts a bolt's
  own life/flash/ignition rolls.
* Regional difficulty: no configuration surface — pure arithmetic over values the
  caller already has.
* Raids/patrols: `spawn_patrols` / `raids` game rules (`game_rules.rs`, both `true`
  by default). `PATROL_TIMELINE_GATE` (120,000 ticks), `PATROL_SPAWN_SEED`,
  `PATROL_COMPANION_RANGE` (16 blocks) in `mobs.rs`; `RAID_ROLL_SEED` in
  `mobs/raid.rs`.
* Dragon/wither: no configuration surface — every constant (`DRAGON_SPAWN_Y = 128`,
  the dragon's heal/respawn-stage timers and sitting-damage threshold; the wither's
  `INVULNERABLE_TICKS = 220`, heal intervals, blast powers, skull damage and
  wither-effect durations) is a vanilla value transcribed as a named `const` citing
  the field it came from.

## Dependencies

* Weather: `lodestone-render`'s `crate::fog`/`crate::light`/`crate::Camera`,
  `lodestone-assets`; `lodestone-shell`'s `crate::net`/`crate::resources`; protocol
  `GAME_EVENT`/`ADD_ENTITY` on v770.
* Lightning: `crate::chunk::ChunkSource`, `crate::mob_spawn::SpawnRng`,
  `crate::regional_difficulty::DifficultyInstance` (the trap-chance roll),
  `crate::fire`, `crate::weather`.
* Regional difficulty: `lodestone_model::Difficulty` only.
* Raids/patrols: `MobSim::spawn_species`, `ChunkWorld::surface_y`,
  `crate::protocol::BossBarSnapshot`/`MobSim::boss_bars`,
  `lodestone_entity::ai::goals::LongDistancePatrolGoal`, `ai::mob::MobController`,
  `ai::roster::ranged::PILLAGER`. See [`docs/mob-ai.md`](./mob-ai.md) and
  [`docs/villagers.md`](./villagers.md) for the goal/roster and villager-POI sides.
* Dragon fight: `lodestone_model::BlockPos` only — no dependency on
  `lodestone-entity`'s goal/pathfinder stack, `lodestone-world`, or any
  `crates/protocol/*` crate, which keeps the module testable in isolation.
* Wither fight: `lodestone_model::{BlockPos, Vec3, Difficulty}`,
  `lodestone_entity::projectile::Projectile`/`DamageFlags`,
  `ai::mob::ProjectileKind::WitherSkull` (shared projectile plumbing). See
  [`docs/projectiles.md`](./projectiles.md) and
  [`docs/entity-physics.md`](./entity-physics.md).
