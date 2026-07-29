# Entity state as ECS components

## What it is

Every non-player entity's state — position, rotation, health, equipment, the item
a drop is made of, and the render-side interpolation that turns 20 Hz reports
into per-frame transforms — held as `bevy_ecs` components and folded by systems.
This is Stage 1 of [`bevy-migration.md`](./bevy-migration.md), which existed to
collapse the three-copy entity pipeline that doc's §1.1 measured.

It lives in two places for one reason:

| where | what | which `World` |
|---|---|---|
| `crates/lodestone-ecs/src/{entity,ingest}.rs` | network state + `NetIngest` systems | the net thread's, owned by `lodestone_client::state::SharedState` |
| `crates/lodestone-shell/src/entities.rs` | render/interpolation state + `Update`/`GameTick`/`Extract` systems | the shell's, owned by `EntityInterpolator` |

**Two `World`s is deliberate and temporary.** Unifying them is
[`bevy-migration.md`](./bevy-migration.md) §4.1; doing it early would put the
interpolation clock and the socket behind one lock. Nothing is duplicated across
them: the net side owns what the server said, the render side owns what is on
screen this frame, and the only thing crossing is `EntitySnapshot` (below).

## How it works

### The three-state encoding, which is the whole point of the component shape

`lodestone_model::Reported<T>` has three states and all three are load-bearing:

| `Reported<T>` | component |
|---|---|
| `Unreported` — the server has never mentioned the field | **component absent** |
| `Reported(None)` — the server explicitly cleared it | present, inner `None` |
| `Reported(Some(v))` | present, inner `Some(v)` |

`CustomName(Option<String>)` and `DisplayItem(Option<ItemStack>)` are the two
components that wrap an `Option` for this reason. Everything else that used to be
an `Option` field on `EntityView` (`flags`, `health`, `baby`, `pose`, `variant`,
`custom_name_visible`, `velocity`, `uuid`) is a newtype over the *inner* value,
because absence alone carries the "never reported" state.

**The defect this exists to prevent:** a dropped item sends its item id exactly
once, at spawn, and every later metadata packet is silent about it. An ingest that
spawned `DisplayItem(None)` as a default and re-inserted it per packet would blank
the drop one tick after it appeared — the item goes invisible. `apply_entity_spawn`
therefore inserts **no** `DisplayItem` and **no** `CustomName`, and
`apply_entity_metadata` inserts one only when `Reported::Reported(_)` says the
packet mentioned the field.

`Equipment` stays a `Vec<EntityEquipment>` rather than a fixed array of `Option`s
for the same class of reason one level down: a slot *absent* from the list is
"never mentioned", a slot present with `item: None` is an explicit clear.

### Ingest: `ClientEvent` → components

`SharedState::apply` routes by `lodestone_ecs::ingest::handles_event`:

```
net thread → SharedState::apply(event)
  ├─ TimeChanged            → WorldTime resource (Stage 0)
  ├─ handles_event(e)       → IngestQueue.push(e); world.run_schedule(NetIngest)
  └─ everything else        → Inner::apply (player, players, scoreboard, boss bars, menus)
```

Inside `NetIngest`: `IngestSet::Drain` (`drain_ingest_queue` moves the queue into
`IngestBatch`) → `IngestSet::Apply` (one system per event family, chained) →
`IngestSet::Index`.

`EntityIndex` maps server entity id → `Entity`. It is maintained *eagerly* by the
spawn/removal systems rather than rebuilt in `IngestSet::Index`, so a movement
event in the same batch as the spawn it follows can still resolve. `.chain()`'s
sync point is what applies the spawn's deferred `Commands` before the movement
system runs — there is a test for exactly that, because without the sync point the
move is silently dropped.

**Arrival order.** Each system walks the batch in order, so intra-family order is
exact. Cross-family order is the `.chain()` order — but `SharedState::apply`
submits **one event per schedule run**, so a batch never holds two events and the
two orders coincide. A future batching driver must revisit this; the only known
non-commutative pair is "despawn then respawn a reused id", and
`apply_entity_spawn` already handles that on its own by replacing whatever holds
the id.

### `EntityView` is now derived, not stored

`Inner.entities: HashMap<i32, EntityView>` and `Inner::apply`'s eight entity arms
and its `apply_metadata` helper are **deleted**. `EntityView` survives as a value
type only, rebuilt on demand by `state.rs`'s `entity_view()` from the components,
for `ClientHandle::entities()` and its tests.

That direction is the plan's one sanctioned intermediate: **components
authoritative, struct derived, never the reverse.** `entity_view` must stay an
exact inverse of the encoding above — reading component absence as
`Reported(None)` would tell a caller the server had cleared a field it has never
mentioned.

### Render side: interpolation as components and systems

One ECS entity per tracked mob, carrying `RenderKind`, `RenderScale`,
`InterpFrom`, `InterpTo`, `InterpClock`, `WalkAnim`, `RenderEquipment`, and —
only for `minecraft:item` — `ItemPhysics`. The absence of `ItemPhysics` is the
switch that keeps every other entity type on a pure position ease.

`EntityInterpolator::update_with_view` drives them in this order, and the order is
what the ~25 hermetic tests in `entities.rs` are written against:

1. `Update` / `FrameSet::Interpolate` → `advance_interp_clocks`
2. per 20 Hz tick: `tick_item_physics` then `GameTick` / `TickSet::Animate` →
   `tick_walk_animation`
3. `fold_snapshots` (this frame's `EntitySnapshot`s, then the prune)
4. `Extract` / `ExtractSet::Entities` → `extract_entity_draws`

`draws()` is then a plain read of the `ExtractedDraws` resource. It does not
re-extract: a `&self` method cannot run a schedule, and re-extracting per call
would let two reads in one frame disagree.

`EntityInterpolator::world()` / `world_mut()` expose the `World`, which is what
keeps the component set from being an island — a plugin can write `InterpFrom` on
a tracked entity and the next extract puts it on screen.

## How to change it, and the gotchas

- **Never spawn a component to make a query simpler.** For `DisplayItem` /
  `CustomName` that is the invisible-drop regression; for `Velocity` it erases
  "never reported" vs "reported zero", which is the difference between a drop
  arcing and a drop falling straight down.
- **Adding a field to `EntityView` without adding the component it reads from
  makes it a second source of truth by definition.** The struct has no storage.
- **`step_item_physics` and `lodestone_physics::move_entity` stay plain
  functions.** [`bevy-migration.md`](./bevy-migration.md) §8: the ECS owns state
  and scheduling, never verified math. A system calls them.
- **Two things are not systems yet, and both are blocked on the same thing.**
  `tick_item_physics` needs a `&dyn CollisionView` and `fold_snapshots` needs a
  `&[EntitySnapshot]`; a `bevy_ecs` `Resource` must be `'static`, and the
  workspace denies `unsafe_code`, so neither borrow can reach a system. The
  collision source becomes `'static` at §4.1(d) (the chunk world as a resource,
  Stage 4); the snapshot slice disappears when ingest writes the render
  components directly. Both functions carry that note at their definition.
- **The render order is clocks → ticks → fold, which is `Update` before
  `GameTick` and the fold after both** — inverted from the plan's `NetIngest` →
  `GameTick` → `Update` → `Extract`. That is behaviour, not style: every numeric
  expectation in the interpolation tests depends on it. Reordering belongs in the
  change that also deletes `EntitySnapshot`.
- **`RenderKind` (a path `String`) and `EntityKind` (a `ResourceKey`) are two
  components for the same fact**, because `EntitySnapshot` speaks the bare path
  the render model set is keyed by. They collapse when `EntitySnapshot` dies.
- **The item-physics gate's discriminating power was measured, not assumed.**
  Disabling the physics step fails exactly three tests —
  `item_pop_follows_a_ballistic_arc_not_a_flat_ease`,
  `item_pop_stops_at_a_real_floor_instead_of_sinking_through_it`, and the negative
  control `without_a_collision_view_the_same_pop_falls_through_the_floor_height`
  — and the same three, with the same 25 passing, before and after the port to
  components. The two "no apex" controls correctly keep passing when nothing
  moves; they are controls for the apex assertion, not for physics existing.

## Configuration

None. No feature flags, no env vars. `bevy_ecs` is
`default-features = false, features = ["std"]` workspace-wide — never
`multi_threaded`, so native and wasm run the same executor and the same system
order.

## Dependencies

- `lodestone-ecs` → `bevy_app`, `bevy_ecs`, `parking_lot`, `lodestone-model`,
  `uuid`. Never a version crate.
- `lodestone-client` → `lodestone-ecs` (the `World` behind `SharedState`).
- `lodestone-shell` → `lodestone-ecs` plus a direct `bevy_ecs`, because bevy's
  derive macros emit absolute `bevy_ecs::` paths. `lodestone-render` gains
  nothing: extract lives in the shell precisely so the GPU layer stays bevy-free.
