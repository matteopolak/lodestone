# Lightning

## What it is

Vanilla's thunderstorm lightning: per-chunk strike-target selection during a
thunderstorm, the `LightningBolt` entity's life-cycle, and its entity-facing
effects (damage, ignition, and species transformations).
`crates/lodestone-server/src/lightning.rs` is a pure decision module — it decides a
strike should happen, where, and what an already-spawned bolt should do on a given
tick, but it cannot spawn anything: the live-entity tracker (`MobSim`, in
`crates/lodestone-server/src/mobs/`) is out of this change's reach. Nothing
server-side produces a lightning-bolt spawn yet, though the client is already wired to
receive one (`lodestone-shell`'s `net.rs` has a `ClientEvent::EntitySpawned` arm for
`lightning_bolt` calling into weather rendering).

## How it works

`should_attempt_strike` is `ServerLevel.tickThunder`'s outer gate — gated on the
**thunder** state, not merely on rain, short-circuiting the RNG draw exactly as
vanilla's `&&` chain does. `block_random_pos` is `Level.getBlockRandomPos`'s own
in-place LCG (distinct from every `java.util.Random`-exact stream in this crate).
`find_lightning_target_around` is `findLightningTargetAround`: a nearby lightning rod
wins outright, otherwise a random living/sky-visible entity in a generous column,
otherwise the terrain heightmap. `tick_thunder_for_chunk` is the whole per-chunk
decision, and `LightningFeed` is where it publishes a `Strike` for a future spawner to
drain — the same publish/drain idiom `crate::weather::WeatherFeed` already
establishes.

`BoltState`/`tick_bolt` port `LightningBolt.tick`'s `life`/`flashes` countdown, and
`resolve_effect` is the `thunderHit` dispatch table, verified against the 26.2 jar:

| species | effect |
|---|---|
| (default) | 5.0 damage, 8-second ignite |
| creeper | the default, plus becomes charged |
| pig | converts to a zombified piglin (not on Peaceful) |
| villager | converts to a witch (not on Peaceful) |
| mooshroom | swaps red/brown, guarded per-bolt |
| turtle | **overrides** the default with a lethal hit — no ignite |

Two corrected misconceptions, both checked against the jar rather than recalled:
there is **no turtle-egg interaction** (`TurtleEggBlock` has no lightning hook at
all), and the **"skeleton horse trap" is not a `thunderHit` transformation** — it runs
backwards. A naturally-spawned `SkeletonHorse` can be flagged as a trap
(`should_be_skeleton_trap`, this module's one consumer of
`crate::regional_difficulty::DifficultyInstance`); later, when a player approaches, the
horse's own AI goal casts a *cosmetic* lightning bolt plus summons a skeleton posse —
lightning does not turn a horse into a trap.

## How to change it

This module is deliberately entity-free. To make a strike reach the screen, three
things need building in `crate::mobs` (off limits to this change; see the handoff
notes in the change that added this doc for the exact proposed hunk):

1. A `LightningBolt` sidecar on `MobSim` (a `HashMap<i32, BoltState>`, following the
   existing `orbs`/`item_state` pattern — not `spawn_species`'s AI/attribute
   machinery, which a boxless, AI-less entity does not need).
2. Draining `LightningFeed` inside `MobSim::tick`/`tick_with_terrain` to spawn one,
   and including it in `MobSim::snapshots()` so it reaches the wire (the entity-type
   registry already has `minecraft:lightning_bolt` at network id 77; no protocol
   change needed).
3. Per-species state (a creeper's `DATA_IS_POWERED`, a mooshroom's "last bolt hit"
   guard, and a conversion primitive for pig→zombified-piglin / villager→witch —
   none of which exist yet) to actually apply `resolve_effect`'s table.

The per-chunk gate itself (`tick_thunder_for_chunk`) has a natural home in
`crate::tick::run_tick_loop_with_weather`'s existing per-chunk random-tick loop
(mirroring `fire_rng`'s construction pattern), reading entity candidates via the
already-public `mobs.with(|sim| sim.snapshots())`. That wiring was not made in this
change: it requires a new `LightningFeed` parameter threaded through a heavily-shared,
actively-edited function, which was judged too wide a blast radius for a hunk-level
edit while other agents were mid-flight on the same file.

## Configuration

None yet — see "How to change it" for the missing seed/RNG-stream plumbing a real
driver would add (following `crate::fire::FIRE_BEHAVIOR_SEED`'s precedent of a fixed
literal until a per-world seed store exists).

## Dependencies

`crate::chunk::ChunkSource` (block reads), `crate::mob_spawn::SpawnRng` (RNG draws),
`crate::regional_difficulty::DifficultyInstance` (the trap-chance roll).
