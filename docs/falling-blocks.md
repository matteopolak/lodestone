# Falling blocks

## What it is

Sand, red sand and gravel become a real, broadcast `FallingBlockEntity` when their
support disappears — a temporary block-shaped entity that free-falls with vanilla's
own physics and becomes a block again where it lands. It spans the server (decision,
entity, physics), the wire (the `ADD_ENTITY` Object Data field) and the client (a
reusable moving-block-model render seam).

## How it works

Four stages, and the interesting parts are the boundaries between them.

### 1. Deciding to fall — one place, not two

`FallingBlock` has exactly two triggers in the jar and **both only schedule a
tick**:

| jar method | when | what it does |
|---|---|---|
| `FallingBlock.onPlace` | the block is placed | `scheduleTick(pos, this, getDelayAfterPlace())` |
| `FallingBlock.updateShape` | a neighbour changed | the same call, nothing else |

`lodestone_server::gravity_tick::ticks_after_place` serves the first (from
`server.rs`'s use-item-on path) and `lodestone_server::random_tick`'s gravity arm
serves the second. `getDelayAfterPlace` is `2`, so a placed sand block visibly hangs
for 100 ms before falling.

The fall itself happens in exactly one place: `tick.rs`'s scheduled-tick drain, on
`gravity_tick::TICK_GRAVITY`. That arm is `FallingBlock.tick` —
`random_tick::settle_gravity_at` answers "is it unsupported, and where would it land"
and nothing else.

**The neighbour route used to settle inline**, which was a second fall path that
skipped both the delay and (once the entity existed) the entity. Sand whose support
was removed by a neighbour teleported while placed sand fell properly, from the same
module. If you are adding a third trigger, schedule; do not act.

**The tick fires at the block's own position, and that is a trap.**
`NeighborPropagator::propagate(origin)` notifies the origin's six neighbours and
*not* the origin, so routing the settle through `propagate_and_react` settles the
sand's *neighbours* and leaves the sand hanging — with a scheduled tick that looks
entirely correct in a queue dump.

### 2. The entity, and the two orderings you cannot see

`MobSim::spawn_falling_block` and `MobSim::tick_falling_blocks` return a
`Vec<gravity_tick::FallingBlockEffect>` rather than acting, and the caller applies it
in order through `tick.rs`'s `apply_falling_block_effect`. That is not ceremony: the
two orderings a player can actually see are otherwise properties of the order a
caller wrote two statements in, which no test can observe.

| stage | jar | order | what the reverse looks like |
|---|---|---|---|
| spawn | `FallingBlockEntity.fall` | `ClearedOrigin` → `Spawned` | the block **and** its falling copy, both visible |
| land | `FallingBlockEntity.tick`'s landing branch | `Placed` → `Discarded` | **neither** a block nor an entity |

The landing shape is the same one `TAKE_ITEM_ENTITY` needed: `take` had to precede
`discard` or the client had nothing left to animate.

**A named transport gap.** `server.rs`'s connection loop drains block updates on its
`container_sync_tick` arm and runs the entity streaming pass on its `read_packet`
arm — two different `select!` arms at ~50 ms each — so the *wire* order of a block
update against an entity spawn/removal is unspecified within one tick. The server-side
order is exact and gated; fixing the wire order means giving the connection loop one
ordered outbound queue. Vanilla covers the same race in the renderer instead (see
"the unported guard" below).

### 3. The physics

`FallingBlockEntity.tick` runs `time++`, `applyGravity()`, `move(SELF, delta)`, the
landing decision, and — as the method's **last** statement — `delta *= getAirDrag()`.
With `getDefaultGravity() = 0.04` and `getAirDrag() = 0.98F`:

```text
v_n = 0.98 * v_(n-1) - 0.04,   v_0 = 0
```

**with the displacement applied before the drag.** That is the whole of
`gravity_tick::fall_step` and the part that is easy to get backwards: **tick one
moves exactly `0.04`, not `0.0392`.** The drag-first reading
(`v_n = 0.98 * (v_(n-1) - 0.04)`) gives `0.0392`, and the two differ by under 2% — so
no approximate assertion can separate them. The gate solves the recurrence by hand
(`a_n = 2 * (0.98^n - 1)`, cumulative `98 * (1 - 0.98^n) - 2n`) and requires the wrong
solution to fail at every one of 80 ticks.

`landing_y` is resolved **once**, at spawn, by `gravity_tick::find_landing_y` against
the live column. Vanilla instead asks `onGround()` every tick; `MobSim` holds its
world immutably and cannot see an edit made after it was built, so re-reading would
answer from a stale snapshot anyway. Named consequence: a block that appears
*underneath* a falling block mid-flight is fallen through rather than landed on.

### 4. The wire, and the field that is easy to miss

The imitated block state travels in the `ADD_ENTITY` **Object Data** field — vanilla's
name for the trailing per-type VarInt — as `Block.getId(blockState)`
(`FallingBlockEntity.getAddEntityPacket`).

**This is the only channel it ever travels on.**
`FallingBlockEntity.defineSynchedData` registers `DATA_START_POS` and nothing else, so
the state is never in a `SET_ENTITY_DATA` packet. A client that ignores the field
draws every falling block as whatever state id `0` resolves to, with nothing logged
anywhere — the same failure shape as a dropped item entity with no reported stack,
where every wire read green and the value travelling it was wrong.

The path, end to end:

```text
MobSim::snapshots  →  EntitySnapshot::object_data
  →  v770 encode_add_entity_body's trailing w.var_i32
    →  v770 handle_add_entity  →  ClientEvent::FallingBlockState
      →  lodestone_ecs::ingest::apply_falling_block_state  →  FallingBlockState component
        →  entities.rs's extract  →  EntityDraw::block_state
          →  gpu/moving_blocks.rs
```

`FallingBlockState` is a **separate event** emitted right after `EntitySpawned`, not a
field on it: the Object Data field means something different for every type that reads
it (a display block, an item-frame rotation), so one event claiming to carry "a block
state" for all of them would be wrong for most. It routes to **`ingest`** (per-entity
state), never `session`.

`EntityDraw::block_state` is `Option<u32>` and absence is the switch. It is not a
sentinel `0`, because state id `0` is a real state (`minecraft:air`) and a caller
could not tell "not a falling block" from "a falling block made of air".

## How to change it

### Adding another falling block

`gravity_tick::is_gravity_block` is the table, and it is deliberately three names.
Vanilla's other `FallingBlock` subclasses — `ConcretePowderBlock`, `AnvilBlock`,
`PointedDripstoneBlock` — are asserted **absent** in
`only_gravity_blocks_schedule_and_a_property_suffix_does_not_defeat_it`, so widening
the table is a visible decision rather than an accident. Each brings behaviour this
port does not have (concrete powder hardens in water, an anvil damages what it lands
on and degrades, dripstone breaks). Widen the table *and* the gate together.

### Changing the physics

`gravity_tick::fall_step` is the whole recurrence and its ordering is the fidelity
argument. If you touch it, re-derive the closed form in a separate script — do not
adjust the expected numbers to match. The gate that predicts the landing tick failed
on its first run because the arithmetic was done by hand and the plausible answer (19)
was wrong (18); the assertion is now the re-derivation.

### Adding a second moving-block producer (piston heads)

The render seam is `crates/lodestone-shell/src/gpu/moving_blocks.rs` and it exists to
have more than one producer. See [`moving-block-models.md`](./moving-block-models.md).

### The unported renderer guard

`FallingBlockRenderer.shouldRender` refuses to draw the entity when
`entity.getBlockState() == level.getBlockState(entity.blockPosition())` — the real
world block at the entity's cell is already the same block. That is what hides the
packet race at both ends of a fall in vanilla. It is **not ported**: the gpu layer has
no polled world block-state source (there is no such field on `RenderState`, unlike
`EntityLightSource`), so the guard has nothing to consult. Porting it means adding one
`sources.rs` source plus its setter and one call site in the app wiring.

## Configuration

No env vars, flags or game rules. Three constants, all vanilla's:

| constant | value | jar |
|---|---|---|
| `gravity_tick::DELAY_AFTER_PLACE` | `2` | `FallingBlock.getDelayAfterPlace` |
| `gravity_tick::FALLING_BLOCK_GRAVITY` | `0.04` | `FallingBlockEntity.getDefaultGravity` |
| `gravity_tick::FALLING_BLOCK_AIR_DRAG` | `0.98` | `Entity.getAirDrag` |
| `gravity_tick::MAX_FALL_TICKS` | `600` | `FallingBlockEntity.tick`'s `time > 600` |

One named numeric deviation: `getAirDrag` returns a **`float`** and `Vec3.scale` takes
a `double`, so the JVM widens `0.98F` to `0.980000019073486…`. The constant here is the
exact decimal `0.98`, matching `lodestone_entity::item_entity::ITEM_AIR_DRAG`. The
divergence is ~2e-9 blocks per tick — five orders of magnitude below the `f64` the wire
carries — and one convention in the tree is worth more than tracking it.

## Dependencies

* `lodestone-server`: `gravity_tick` (decision + physics), `mobs` (the registry and
  `snapshots`), `random_tick` (`settle_gravity_at`, the neighbour arm), `tick` (the
  drain, the per-tick step, `apply_falling_block_effect`), `protocol`
  (`EntitySnapshot::object_data`).
* `lodestone-data`: `block_states::state_id` resolves the imitated state to its
  protocol-776 global id, from the generated real-server dump.
* `lodestone-v770`: `encode_add_entity_body` writes the field; `handle_add_entity`
  reads it back and emits `ClientEvent::FallingBlockState`. **Only `v770`** — the three
  legacy families decode `ADD_ENTITY` and still discard the field.
* `lodestone-model`: the `ClientEvent` variant and its `route` entry.
* `lodestone-ecs`: `FallingBlockState` component, `apply_falling_block_state`.
* `lodestone-shell`: `EntityDraw::block_state`, `gpu/moving_blocks.rs`.
* `lodestone-render`: `mesh_moving_block_quads`, `CrackResolver::state_quads`.
