# Primed TNT

## What it is

The `minecraft:tnt` entity — the fuse-countdown, gravity-affected block that
TNT becomes once ignited — plus every producer that ignites one. A port of
`PrimedTnt` (`.cache/mc/26.2/src/net/minecraft/world/entity/item/PrimedTnt.java`)
and `TntBlock`'s ignition methods, living in
`crates/lodestone-server/src/mobs/tnt.rs`.

Before this, igniting TNT by any means did nothing: there was no primed-TNT
entity anywhere in the crate (`mobs/mod.rs`'s own doc history and
`crate::fire`'s and `crate::redstone_dispenser`'s module docs all named the
same gap independently). Detonation itself was not new — `MobSim::explode`
(entity damage) and `crate::explosion_blocks`/`crate::block_drops` (block
destruction and drops) already existed and were already wired for a creeper's
own fuse — so this feature is a second *producer* into an existing pipeline,
not a new consumer.

## How it works

**The entity.** `TrackedTnt` (`mobs/mod.rs`) is a plain `HashMap<i32,
TrackedTnt>` sidecar on `MobSim`, the same shape `TrackedVehicle`/
`TrackedFallingBlock` use: no AI, no attributes, no `SimMob` goal machinery,
because none of it applies to an entity with no box that matters and no
behaviour beyond falling and counting down.

**Motion.** `MobSim::tick_tnt` transcribes `PrimedTnt.tick` in vanilla's own
order: gravity (`0.04`/tick), then a real collision move through
`lodestone_physics::entity::move_entity` — the same shared integrator
`MobSim::tick_vehicles` uses, so a primed TNT resolves collision through
identical code to a boat or a player rather than a second copy — then air drag
(`0.98`), then, on the tick it lands, the `(0.7, -0.5, 0.7)` bounce/friction
multiply that gives TNT its characteristic hop. The fuse decrements last;
`fuse <= 0` triggers detonation.

**Detonation** calls `MobSim::explode` (entity damage/knockback) and pushes a
`Detonation` onto `MobSim::pending_detonations` — exactly the two calls
`MobSim::tick` already makes for a creeper. `tick::run_tick_loop`'s existing
`take_detonations` drain (`crate::explosion_blocks::destroy_blocks` /
`crate::block_drops::drop_explosion_loot_in_blast`) needs no TNT-specific call
site at all.

**Ignition, four producers:**

| producer | where | fuse |
|---|---|---|
| flint and steel / fire charge, clicked on a TNT block | `crate::server::apply_use_item_on` | `DEFAULT_FUSE_TIME` (80) |
| redstone signal (`hasNeighborSignal`) | `crate::random_tick::react_to_notification` schedules `tnt::TICK_TNT_PRIME`; `tick::run_tick_loop`'s scheduled-tick drain spawns | 80 |
| fire consuming a TNT neighbour (`FireBlock::checkBurnOut`) | `crate::fire::check_burn_out` reports the position; `tick::run_tick_loop`'s `TICK_FIRE` arm spawns | 80 |
| chain reaction — a TNT block destroyed by another blast (`TntBlock::wasExploded`) | `crate::block_drops::drop_explosion_loot_in_blast` reports the position; `tick::run_tick_loop`'s detonation drain spawns via `MobSim::spawn_tnt_short_fuse` | `tnt::random_short_fuse(80, rng)`, drawn from `MobSim::tnt_rng` |

A dispenser holding TNT also spawns a primed entity directly
(`TntDispenseItemBehavior`, wired in `tick.rs`'s `TICK_DISPENSER_FIRE` arm) —
a fifth producer, not one of the four above, closing a gap
`crate::redstone_dispenser`'s own table used to name.

All four/five gate on the `tnt_explodes` game rule
(`crate::game_rules::GameRules::tnt_explodes`), matching vanilla's own
`GameRules.TNT_EXPLODES` check in `TntBlock::prime`, except the flint-and-steel
arm — see that arm's own comment for why (no `WorldStateHandle` in scope at
that call site, the same gap the portal-ignition arm beside it already has).

**Wire.** Every live TNT streams through `MobSim::snapshots()` as an ordinary
`minecraft:tnt` entity, carrying `MetadataField::TntFuse` — `PrimedTnt
.DATA_FUSE_ID`, index 8. Index 8 has **five** real claimants in the committed
jar dump (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`):
`ExperienceOrb.DATA_VALUE`, `PrimedTnt.DATA_FUSE_ID`,
`FishingHook.DATA_HOOKED_ENTITY`, `VehicleEntity.DATA_ID_HURT` and a display
entity's interpolation delay, plus `ItemEntity.DATA_ITEM` under a
self-identifying different serializer — so the *producer*, not a census
column, is what disambiguates them: only `MobSim::snapshots`' TNT loop ever
builds a `MetadataField::TntFuse`.

## How to change it, and the gotchas

- **The launch velocity and fuse are record constants, not guesses.**
  `DEFAULT_FUSE_TIME` is 80, the horizontal launch magnitude is `0.02`, the
  vertical component is the fixed `0.2` (vanilla's `0.2F` widened to
  `double` is not reproduced — see `tnt.rs`'s own module doc for why that
  specific gap is inert here, unlike `FALLING_BLOCK_AIR_DRAG`'s).
- **Every ignition producer converges on `MobSim::spawn_tnt`/
  `spawn_tnt_short_fuse`.** Do not hand-roll a second TNT constructor; the
  random launch direction is drawn from `MobSim::tnt_rng`, an isolated RNG
  stream, and bypassing it would either desync that stream or reuse another
  behaviour's draws.
- **`react_to_notification` (redstone) and `check_burn_out` (fire) cannot
  spawn an entity.** Both operate over a bare `ChunkColumn`/`ChunkSource`
  with no `MobSim` in scope, so both only *report* a position
  (`tnt::TICK_TNT_PRIME` schedule / `primed_tnt: &mut Vec<BlockPos>` output)
  for `tick::run_tick_loop` to act on — the same handoff shape
  `crate::redstone_dispenser::TICK_DISPENSER_FIRE` already uses. This costs
  up to one tick (50 ms) of latency versus vanilla's synchronous prime,
  accepted for the same reason `MobSim::pending_detonations` already is.
- **Deliberately not modelled**: the fluid-current push
  (`Entity.updateFluidInteraction`'s `applyCurrentTo` — TNT still falls,
  collides and settles in water, it just does not drift with a current),
  `handlePortal`, per-block status effects, the client smoke particle, and
  the `EntityReference<LivingEntity>` owner. See `tnt.rs`'s own module doc for
  the reasoning behind each cut.
- **Mining an `unstable` TNT block** (`TntBlock::playerWillDestroy`) is not
  wired: this crate's block-breaking path never sets the `unstable`
  block-state property, so there is nothing for that arm to key off yet.

## Configuration

- `tnt_explodes` game rule — see the ignition table above for exactly which
  producers gate on it.

## Dependencies

- `lodestone_physics::entity::move_entity` — the shared collision integrator.
- `crate::explosion_blocks`/`crate::block_drops` — the existing block-half and
  loot pipeline, reused unmodified.
- `lodestone_data::entity_dimensions`/`entity_types` — the real `0.98 x 0.98`
  hitbox and the `minecraft:tnt` registry entry.
- `crates/protocol/v770/src/server_protocol.rs`'s `MetadataField::TntFuse`
  encoder — the wire half.

## What remains

~~Minecarts do not exist as entities and have no rail-following physics.~~
**Landed** — see [`docs/minecart.md`](./minecart.md): all five
`AbstractMinecart` subclasses, `OldMinecartBehavior`'s rail-following
physics (straight, sloped and curved rails, powered-rail boost/brake),
riding, and a TNT minecart's own detonation, which reuses this module's
`MobSim::explode`/`pending_detonations` pipeline exactly as this doc's own
"reusing the explosion machinery" section describes.

~~And nothing draws a primed TNT either.~~ **Landed since** — see
`lodestone-shell`'s `merge_primed_tnt` (`gpu/moving_blocks.rs`), which poses
a primed TNT as a literal block model (vanilla's own `TntRenderer` draws it
that way) rather than through the baked-rig `entity_models()` corpus;
`model_for_type("tnt")` correctly stays `None`, because that route never
needed the corpus at all. A minecart's own renderer is a real cart-frame
mesh, not a bare block, so it needs the *other* kind of fix (a corpus entry,
the same shape a boat's hull already has) — still missing, and it is why a
spawned minecart still draws zero pixels. See
[`docs/minecart.md`](./minecart.md)'s own "what does not draw yet" section.
