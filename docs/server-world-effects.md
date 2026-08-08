# Server-owned sounds, particles and level events

Issue [#530](https://github.com/matteopolak/lodestone/issues/530).

## What it is

The path by which the integrated server tells a client "play this here". Before
it, the `ServerProtocol` trait had **no sound encoder and no particle encoder** —
so the server emitted no `sound`, no `level_event` and no `level_particles`
packet, ever, and anything the client could not predict for itself was silent and
invisible. A player could beat a cow to death without a sound.

## How it works

Four pieces, in the order a sound travels:

```text
crate::effects                a version-free WorldEffect + the derivations
  ↓ pushed
BlockTickFeed's effect lane   the transport (see "Why not its own feed")
  ↓ drained by serve_play
ServerProtocol::encode_world_effect     dispatch to one of three encoders
  ↓
V770ServerProtocol            the real sound / level_event / level_particles bytes
```

### `crates/lodestone-server/src/effects.rs`

`WorldEffect` mirrors the three clientbound packets one-for-one rather than
abstracting over them. Beside it live the derivations — `block_destroyed`,
`block_placed`, `openable_toggled`, `mob_vocalisation` — which turn a world change
into an effect.

**Every derived sound name is checked against
`lodestone_data::sound_events`**, the jar-derived `minecraft:sound_event` registry,
before it is used. That is what makes deriving a per-material family by string
safe: `openable_toggled` tries `block.<block>.<action>`, then
`block.<material>_wood_<family>.<action>`, then the generic `block.wooden_door`
form, and takes the first one 26.2 actually has. `minecraft:iron_door` lands on its
own event, `minecraft:bamboo_door` on the per-wood-type one, and `minecraft:oak_door`
on the generic — three different answers from one chain, which is what
`openable_sounds_resolve_per_material` pins.

### The transport, and why not its own feed

The effect lane is a **third `Arc<Mutex<Vec<_>>>` inside `BlockTickFeed`**, not a
`WorldEffectFeed` of its own. A feed here is nine `serve_connection*` signatures
wide, and an effect is the same kind of thing as the block update in lane 0:
something the world tick did that this connection has no other way to learn about.
It is an *outbound* lane, so `BlockTickFeed::subscriber` splits it per-connection
exactly as it splits lane 0.

Same single-consumer caveat as every other feed (`ExplosionFeed`, `WeatherFeed`):
`drain_effects_for` is drain-all, so exactly one connection task may own an instance.

Each entry carries an `Option<Uuid>` — vanilla's `except` player, the first argument
of `Level.playSound(@Nullable Entity except, …)` (`Level.java:436`).
`drain_effects_for(viewer)` drains everything and returns only what `viewer` should
hear; `drain_effects_tagged` keeps the tag, for `IntegratedServer::bind`'s relay,
which re-publishes into each connection's own queue and so cannot resolve the
exclusion itself.

### Publishers

| effect | published from | vanilla |
|---|---|---|
| mob hurt / death sound | `MobSim::note_vocalisation`, drained by `run_tick_loop` | `LivingEntity.hurt` / `.die` |
| door / trapdoor / fence-gate open-close | `tick.rs`'s `publish_openable_sound`, at both scheduled-tick write sites | `DoorBlock.playSound` (`:247`) |
| grazed-block break particles | `run_tick_loop`'s graze drain | `EatBlockGoal`'s level event 2001 |
| a player's block break | `server.rs`'s `destroy_block`, excluding the breaker | `Level.destroyBlock`'s level event 2001 |
| a player's block place | `server.rs`'s `apply_use_item_on`, excluding the placer | `BlockItem.place` (`:87`) |

`MobSim` records vocalisations for the same structural reason it records
detonations: it holds the world immutably and owns no connection, so it can only
note the intent. `note_vocalisation` is called from **every** damage funnel
(`tick`'s melee hits, self-damage, `explode`, `attack`) rather than from
`SimMob::apply_damage`, because the queue lives on the sim and `apply_damage` holds
only the one mob — and always **before** the end-of-tick `retain`, or a killing
blow finds no mob to read the species and position from.

## How to change it, and the gotchas

**The double-trigger trap is the main one.** `lodestone-shell` predicts its own
block-break and block-place sounds locally (`block-sound-types.md`,
`break-particles.md`), so an effect the acting client would also predict must not
reach *that* client — it would play twice. Publish it with
`publish_effect_except(actor, effect)`, which is vanilla's own `except` argument, and
every other player still hears it. `publish_effect` is for effects with no acting
player at all (a mob's death, a redstone-opened door, a grazing sheep).

This is what unblocked the break and place sounds. `effects::block_destroyed` and
`block_placed` existed and were correct for a session before this, and were
deliberately left unpublished, because without the exclusion the only client that
could hear them was the one that had already played them itself.

Other things worth knowing:

* **Level event 2001 is a sound *and* a particle burst in one packet**, and its
  `data` is a block-state id. `Level.destroyBlock` sends exactly this and no
  separate sound, which is why a break needs one effect rather than two. Do not
  reach for a `LevelEvent.SOUND_*` constant for a door: `DoorBlock.playSound` is a
  real `level.playSound`, checked in the jar, not a level event.
* **`encode_sound`'s position is fixed-point**, `(int)(block * 8)`, not three
  `f64`s — and the sound rides as a `Holder<SoundEvent>` **registry reference**
  (`registryId + 1`), the same encoding `encode_explode` already uses.
* **Only argument-less particles are expressible.** `encode_level_particles`
  writes the type id and stops, which is correct for a `SimpleParticleType` and
  wrong for anything with options (`dust`, `block`, `item`) — those need their
  option bytes, so `simple_particle_registry_id` is named as a warning. Sending a
  truncated one is worse than sending none: the client reads it as a misparse of
  the *next* packet.
* **Pitch does not come from an RNG.** Vanilla draws it from the level random;
  doing that here would consume from the random-tick scheduler's generator, whose
  draw *sequence* is what `random_tick`'s parity gates pin. Both publishers cycle
  the pitch over the tick counter instead.
* Adding a fourth effect kind is a variant plus an arm in
  `encode_world_effect` plus one encoder — never a change at the drain site.

## Configuration

None. No feature gate, no env var.

## Dependencies

* `lodestone_data::sound_events` (name validation), `sound_types` (per-block
  break/place volume and pitch), `particle_types` (particle registry ids).
* `lodestone_model::{SoundCategory, Vec3, Vec3f, BlockPos}` for the version-free
  effect description.
* `crate::mobs::block_state_id_or_default` to turn a state string into the
  block-state id level event 2001 carries.
