# Lightning

## What it is

Vanilla's thunderstorm lightning: per-chunk strike-target selection during a
thunderstorm, the `LightningBolt` entity's life-cycle, and its entity-facing
effects (damage, ignition, and species transformations) — wired end to end,
storm to screen. `crates/lodestone-server/src/lightning.rs` is the pure
decision layer; `crates/lodestone-server/src/mobs/lightning.rs` is the
producer that turns a decision into a real, network-visible entity;
`crates/lodestone-server/src/tick.rs`'s `run_tick_loop_with_weather` is the
driver that calls the per-chunk gate every tick and applies the one effect
`MobSim` cannot apply itself (fire ignition, since `MobSim::world` is a frozen
pathfinding snapshot, not the live world).

## How it works

`should_attempt_strike` is `ServerLevel.tickThunder`'s outer gate — gated on the
**thunder** state, not merely on rain, short-circuiting the RNG draw exactly as
vanilla's `&&` chain does. `block_random_pos` is `Level.getBlockRandomPos`'s own
in-place LCG (distinct from every `java.util.Random`-exact stream in this crate).
`find_lightning_target_around` is `findLightningTargetAround`: a nearby lightning rod
wins outright, otherwise a random living/sky-visible entity in a generous column,
otherwise the terrain heightmap. `tick_thunder_for_chunk` is the whole per-chunk
decision, called once per ticking chunk per tick from `run_tick_loop_with_weather`
(unconditionally, matching vanilla's own `ServerLevel.tickChunk` — only
`should_attempt_strike`'s short-circuit keeps a clear world from drawing
anything), gated on `weather.thundering`. A decided `Strike` is handed straight to
`MobSim::spawn_lightning_bolts` rather than round-tripping through `LightningFeed`'s
`Arc<Mutex<..>>`, since the whole path is synchronous within one tick loop
iteration; `LightningFeed` stays as the tested publish/drain type for a future
caller that genuinely needs it across threads.

`BoltState`/`tick_bolt` port `LightningBolt.tick`'s `life`/`flashes` countdown.
`mobs/lightning.rs`'s `LiveBolt` sidecar (a `HashMap<i32, LiveBolt>` beside the
existing `orbs`/`item_state` maps — no `NavigatingMob`/`GoalSelector` body, since a
bolt has no box and no AI) holds one per live bolt; `MobSim::tick_lightning` runs
`tick_bolt` for each, every tick, and `MobSim::snapshots()` streams each live bolt as
`minecraft:lightning_bolt` with **empty metadata** — `LightningBolt` overrides
`defineSynchedData` with an empty body, so there is genuinely nothing to send. The
wire entity-type registry already had this at network id 77; `EntitySnapshot` →
`ADD_ENTITY` in `crates/protocol/v770/src/server_protocol.rs` is fully generic, so no
protocol change was needed to reach the client — `lodestone-shell`'s `net.rs` already
had a live `ClientEvent::EntitySpawned` arm for `lightning_bolt` calling into weather
rendering.

`resolve_effect` is the `thunderHit` dispatch table, verified against the 26.2 jar:

| species | effect | applied by this crate |
|---|---|---|
| (default) | 5.0 damage, 8-second ignite | damage only — see "What is not modelled" |
| creeper | the default, plus becomes charged | damage only, no charge |
| pig | converts to a zombified piglin (not on Peaceful) | full conversion |
| villager | converts to a witch (not on Peaceful) | full conversion |
| mooshroom | swaps red/brown, guarded per-bolt | guard only, no variant to flip |
| turtle | **overrides** the default with a lethal hit — no ignite | full |

Two corrected misconceptions, both checked against the jar rather than recalled:
there is **no turtle-egg interaction** (`TurtleEggBlock` has no lightning hook at
all), and the **"skeleton horse trap" is not a `thunderHit` transformation** — it runs
backwards. A naturally-spawned `SkeletonHorse` can be flagged as a trap
(`should_be_skeleton_trap`, this module's one consumer of
`crate::regional_difficulty::DifficultyInstance`); later, when a player approaches, the
horse's own AI goal casts a *cosmetic* lightning bolt plus summons a skeleton posse —
lightning does not turn a horse into a trap. (The skeleton-horse spawn itself is not
implemented — only the roll that would flag one.)

Pig/villager conversion uses a new minimal "despawn and respawn" primitive
(`mobs/lightning.rs`'s `convert_species`): it did not exist anywhere in this crate
before (`grep`ping for "convert"/"ConversionParams" was empty), and this version does
**not** preserve health, equipment, age or leash state — a faithful `ConversionParams`
carry-over is a larger unit than one lightning strike.

## What is not modelled, and why

* **Fire ignition attempts are collected, not applied, inside `MobSim`.**
  `MobSim::world` is a frozen pathfinding snapshot, so `tick_lightning` records
  candidate positions in `pending_lightning_fires`, drained by
  `MobSim::take_lightning_fires` and applied against the *live* world in
  `run_tick_loop_with_weather` (`crate::fire::can_survive` + `state_for_placement`,
  the same gate `LightningBolt.spawnFire` uses).
* **The lightning-rod power pulse, copper waxing reset, and
  `GAME_EVENT(LIGHTNING_STRIKE)`/thunder sounds are not applied.** `BoltTickEffects`
  carries real flags for all three; nothing consumes them yet.
* **`Creeper.DATA_IS_POWERED` is not streamed**, and no "powered" state is recorded at
  all — the wire index lives in `crates/protocol/v770/src/server_protocol.rs`, and
  `SimMob` has no field for it (`CREEPER_EXPLOSION_RADIUS`'s own doc already discloses
  the doubled-explosion half of this same gap). A charged creeper still takes the
  default damage.
* **A struck mob is not set alight.** `MobSim` has no burn state for a mob at all —
  see `crate::burning`'s own module doc — so `thunderHit`'s ignite half has nowhere to
  land; the damage half is real.
* **The mooshroom variant never actually flips.** No species in this crate models a
  red/brown variant. The per-bolt guard (`SimMob::last_lightning_bolt`) is real and
  landed now specifically so a future variant field cannot double-toggle on day one.
* **Players are not candidates for `thunderHit`.** `MobSim` knows player *positions*
  only, not their entity ids or `PlayerVitals` (which live per-connection) — the same
  pre-existing seam every other mob-on-player damage path in this crate has.
* **The lightning-rod POI search and `#minecraft:lightning_rods` tag lookup are
  approximated** — `nearby_lightning_rod` is always `None` (no POI manager exists),
  and the single block `minecraft:lightning_rod` stands in for the tag.

## How to change it

`crates/lodestone-server/src/lightning.rs` owns every rule (target selection, the
`life`/`flashes` state machine, the `thunderHit` table); `mobs/lightning.rs` owns the
sidecar, spawn/tick driver, and entity-effect application; `tick.rs`'s
`run_tick_loop_with_weather` owns the per-chunk gate call and fire-ignition
application. To add a new per-species effect: extend `LightningEffect` and
`resolve_effect` in `lightning.rs` (pure, jar-verified), then add the matching arm in
`mobs/lightning.rs`'s `apply_lightning_hits`. To make the lightning-rod pulse or
copper reset real: read `BoltTickEffects::power_lightning_rod`/`clear_copper` in
`MobSim::tick_lightning` and hand the position off through a new `pending_*` field,
the same shape `pending_lightning_fires` already establishes.

## Configuration

Two fixed `SpawnRng` seeds in `crate::lightning`: `LIGHTNING_STRIKE_SEED` (per-chunk
target selection, consumed in `tick.rs`) and `LIGHTNING_BOLT_SEED` (a bolt's own
`life`/`flashes`/ignition draws) — kept separate so a strike decision can never shift
which roll a bolt's own state machine sees, following `crate::mobs::orbs::ORB_BEHAVIOR_SEED`'s
precedent of a fixed literal until a per-world seed store exists.

## Dependencies

`crate::chunk::ChunkSource` (block reads), `crate::mob_spawn::SpawnRng` (RNG draws),
`crate::regional_difficulty::DifficultyInstance` (the trap-chance roll),
`crate::fire` (ignition's `can_survive`/`state_for_placement`), `crate::weather`
(the `raining`/`thundering` state the per-chunk gate reads).
