# Item pickup animation

## What it is

The short flight a collected item makes toward whoever collected it, before it
vanishes — vanilla's `ItemPickupParticle`. Issue #365, reported from live play as
"picked-up items teleport into the inventory".

Companion docs: [Dropped items](./dropped-items.md) for the item-model draw path
this reuses wholesale, and [Entity rendering](./entity-rendering.md) for the
render-side track set it reads its start position from.

## What vanilla actually does, and what it does *not*

Read out of `.cache/mc/26.2/client-src`, because the obvious reading is wrong:

```java
// ClientPacketListener.handleTakeItemEntity, ~line 1013
EntityRenderState itemState = this.minecraft.getEntityRenderDispatcher().extractEntity(from, 1.0F);
this.minecraft.particleEngine.add(new ItemPickupParticle(this.level, itemState, to, from.getDeltaMovement()));
…
this.level.removeEntity(packet.getItemId(), Entity.RemovalReason.DISCARDED);
```

**The item entity is removed immediately.** It is *not* retargeted and lerped —
that is what issue #365's own summary says, and it is not what the jar does. What
flies is a **frozen copy** of the item's extracted render state, owned by a
particle. Two consequences fall straight out of that and both are visible:

- the copy's bob/spin phase is frozen at the instant of pickup (the render state is
  extracted once, never re-extracted);
- the animation survives the entity's removal, so it cannot be hung off the entity.

The numbers, all from `ItemPickupParticle` / `ItemPickupParticleGroup`:

| quantity | value | source |
|---|---|---|
| flight length | **3 ticks** (150 ms) | `ItemPickupParticle.LIFE_TIME = 3`; `tick()` removes at `life == 3` |
| easing | `t = (life + partialTick) / 3; t *= t` — **quadratic ease-in** | `ItemPickupParticleGroup.ParticleInstance.fromParticle` |
| position | `lerp(t, itemRenderState.pos, targetPos)` | same |
| target | `(target.getX(), (target.getY() + target.getEyeY()) / 2, target.getZ())` | `ItemPickupParticle.updatePosition` |
| target, resolved | `y + eyeHeight / 2` | `Entity.getEyeY()` is `position.y + eyeHeight` (`Entity.java:3798`) — an **absolute** Y |

`getEyeY()` being absolute is the trap in that table. Read as a relative offset the
midpoint becomes `y + (y + 1.62)/2`, which for a player standing at y = 64 aims 32
blocks underground.

The ease is quadratic in the age fraction, so **half the flight covers a quarter of
the distance** — the item leaves slowly and arrives fast. A linear lerp is visibly
different and is the reading a "did it move?" test cannot distinguish.

The collector is **any** `LivingEntity`, not just the local player: a fox or an
allay picking something up animates too.

## How it works

```text
TAKE_ITEM_ENTITY (v770 adapter)          -- already decoded and tested
  → ClientEvent::ItemPickup
  → net.rs `forward`                     -- THE ARM THAT WAS MISSING
  → NetUpdate::ItemPickup(ClientEvent)
  → Sim::poll_net
      → lodestone_game::mining::PickupFeed::apply      -- already existed, no caller
      → (end of poll_net) PickupFeed::drain
      → entities::begin_item_pickup(world, item_id, collector_id)
          reads ItemStacks[item_id] and the render track's *drawn* position
  → PickupAnimations                     (resource, in the one World)
  → GameTick / TickSet::Animate: tick_pickup_animations   life += 1, drop at 3
  → Extract / ExtractSet::Entities, after extract_entity_draws:
      extract_pickup_draws  →  push EntityDraw { type_path: "item", … }
  → RenderState::prepare_item_geometry   -- UNCHANGED
```

The last two lines are the point. The animation emits the *same* `EntityDraw` a live
dropped item emits, so the whole GPU side — `dropped_item_mesh`, the model pipeline,
the bob phase, the light sample — is reused with no change at all. There is no new
pass, no new shader and no new buffer.

### Where it was an island, and which router

Every piece except one existed and was tested: the `TAKE_ITEM_ENTITY` decode in
`crates/protocol/v770/src/adapter.rs`, and `PickupFeed` in
`crates/lodestone-game/src/mining.rs` with unit tests. `PickupFeed` had **no caller
anywhere in the tree**.

The missing hop was **`net.rs`'s `forward`** — the shell's own `ClientEvent` stream,
not either `handles_event` switch. Checked all three before adding anything:

| router | carries | has an `ItemPickup` arm? |
|---|---|---|
| `lodestone_ecs::ingest::handles_event` | per-entity ECS state | no, and it should not — the entity is being *destroyed* |
| `lodestone_ecs::session::handles_event` | local-player scalars | no, and it should not — the collector may be a mob |
| `net.rs`'s `forward` | the shell's `ClientEvent` stream | **now yes** |

That makes it the third island in that one function, after `BLOCK_EVENT` (chest
lids) — the terminal `_ => return Ok(())` is indistinguishable at the call site from
an arm that has nothing left to handle.

### The ordering that makes it work at all

`begin_item_pickup` is called from **inside `Sim::poll_net`**, and that is not
incidental. `Sim::step` runs `poll_net` → `fold_entities` → `Extract`, and
`fold_entities` prunes the render track (and the `ItemStacks` entry) of any entity
the server has stopped reporting — which, one packet after `take_item_entity`, is
this item. `begin_item_pickup` reads both. Moving the call one step later reads
nothing and draws nothing, silently.

It captures the item's **drawn** (interpolated) position, not its last reported one,
because that is the quantity `extractEntity(from, 1.0F)` gives vanilla.

### The collector's position is resolved every frame

`ItemPickupParticle.updatePosition()` re-reads the target every tick, so a pickup
made while walking chases the collector. `collector_target` therefore resolves it at
extract time, from two sources in order:

1. the **local player**, matched by `MinecraftEntityId` on the `LocalPlayer` entity,
   using its live `PhysicsState` eye height (so the swimming/crawling pose is
   correct);
2. otherwise the render-side track (`TrackIndex` → `InterpFrom`/`To`/`Clock`), with
   a constant `DEFAULT_EYE_HEIGHT` of 1.62.

**The first lookup is load-bearing and easy to miss.** The local player has no render
track at all — that absence is deliberate, and keeps a self-model off the render path
(see `lodestone_ecs::ingest::apply_local_player_login`). Resolving only through
`TrackIndex` would silently animate nothing for every pickup *the player* makes,
i.e. all the ones that matter, while working perfectly for a mob.

## How to change it, and the gotchas

- **`PickupAnimations` is a resource, not a component**, and it has to be: the entity
  it belongs to is despawned before the first frame of the animation.
- **`extract_pickup_draws` must run `.after(extract_entity_draws)`**, which clears
  `ExtractedDraws`. Without the ordering constraint bevy may run it first and erase
  every pickup draw in the frame it was written — a system that runs, passes its own
  unit test, and reaches zero pixels.
- **The bob does not freeze quite as hard as vanilla's.** `EntityDraw::anim.age_ticks`
  is frozen at capture (correct), but `dropped_item_mesh` derives the *hover* from
  the position it is given, which is moving. Vanilla's frozen render state also
  freezes the hover offset. The divergence is sub-block over 150 ms.
- **No sound.** Vanilla plays `entity.item.pickup` at 0.2 volume with a
  `(rand - rand) * 1.4 + 2.0` pitch (and `entity.experience_orb.pickup` at 0.1 /
  `* 0.35 + 0.9` for an orb) from the same packet handler. Deliberately out of scope
  here; it needs a *local* sound emit, not the positional `NetUpdate::Sound` path.
- **Experience orbs are not covered.** Vanilla animates them through the same
  particle but never removes the orb entity from `handleTakeItemEntity`; orbs are not
  decoded as a distinct render kind on this side yet.
- **Partial pickups are not modelled.** Vanilla shrinks the stack by `amount` and
  only removes the entity if it empties. `PickupFeed::Pickup::amount` carries the
  number; nothing reads it, because the animation is a copy either way and the
  authoritative stack change arrives separately.
- **This is not an inventory update, ever.** The stack that lands in the player's
  inventory arrives as `set_player_inventory` / `container_set_slot` and is folded by
  `Menus`. `PickupFeed`'s own module doc explains why folding a count out of this
  path would be a second, silently-diverging source of truth for the same stacks.

## Configuration

None. `PICKUP_LIFE_TICKS`, `PICKUP_TARGET_EYE_FRACTION` and
`REMOTE_COLLECTOR_EYE_HEIGHT` are vanilla constants in
`crates/lodestone-shell/src/entities.rs`.

## Dependencies

- `crates/protocol/v770` — the `TAKE_ITEM_ENTITY` decode (unchanged).
- `lodestone-game` — `mining::{PickupFeed, Pickup}` (unchanged; this is its first
  caller).
- `lodestone-physics` — `player::DEFAULT_EYE_HEIGHT`.
- `lodestone-ecs` — `FrameClock` for the partial tick, `player::{LocalPlayer,
  PhysicsState}` for the local collector.
- `lodestone-render` — nothing new; the flight goes through the existing dropped-item
  geometry path.

## Gates

In `crates/lodestone-shell/src/entities.rs`'s test module, driven through the real
`EntityInterpPlugin` schedules (`EntityInterpolator`), so the systems and their
ordering are exercised rather than the functions called by hand:

- `the_pickup_ease_is_quadratic_not_linear` — the interpolant at the midpoint is
  `0.25`, not `0.5`.
- `a_pickup_draws_the_item_in_flight_toward_its_collector` — **the magnitude gate.**
  One tick in, progress is `(1/3)² = 1/9`, so with the collector 4 blocks away the
  item must be `4/9` along `x`. A linear ease predicts `4/3` from the same
  constants; the assertion lands on the right one to 1e-3. The first poll uses
  `dt == 0.0` deliberately so `interp_alpha` is exactly `0.0` and the expected value
  is arithmetic rather than a range — a `0.016` there leaves `alpha == 0.32` and
  moves the answer to `0.19`.
- `without_a_pickup_event_the_collected_item_simply_disappears` — the **executed
  negative control**: the identical two polls with no `begin_item_pickup` leave no
  item draw at all, so the positive gate cannot be satisfied by a track that merely
  failed to be pruned.
- `a_pickup_animation_expires_after_exactly_three_ticks` — the draw count per tick is
  `[1, 1, 0, 0, 0]`.
- `a_pickup_for_an_unknown_or_stackless_item_starts_nothing` and
  `a_pickup_with_no_resolvable_collector_draws_nothing_and_still_expires` — the two
  give-up paths draw nothing and, in the second case, still age out rather than
  leaking one entry per out-of-range pickup for the session.

**No pixel gate.** The flight emits the same `EntityDraw` that
`crates/lodestone-shell/tests/dropped_item_pixels.rs` already renders end to end, at
a different position; a new GPU gate would be re-measuring that path rather than
this change. Stated as a deliberate scope decision, not as verification performed.
