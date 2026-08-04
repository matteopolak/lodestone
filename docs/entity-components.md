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
screen this frame.

**Update (issue #36 landed):** for `crate::sim::Sim` specifically, §4.1(c) went
further and put both tables' components in the *one* `World` `Sim` owns —
`Sim` runs its own `IngestPlugin` + `EntityInterpPlugin` pair in a single
`App`, rather than reading a second, already-released copy through
`lodestone_client::state::SharedState`. `EntitySnapshot`, the version-free
value type that used to ferry data between the two, is deleted:
[`fold_entities`](../crates/lodestone-shell/src/entities.rs) reads the ingest
components straight off that one `World`, inside its own write guard, instead
of taking a `&[EntitySnapshot]` argument. See "Update, and it changes the
plan" below for the change that made the deletion safe without also
reordering the schedule.

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
  ├─ session::handles_event → the same queue (Stage 3's session folds)
  └─ everything else        → Inner::apply (the local-player echo only)
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

### The local player is in the index too, and that took two guards

`apply_local_player_login` adds one more writer: **the local player's own id**,
from `ClientEvent::Login`. It has to, because *vanilla never sends an `AddEntity`
for yourself*, so the spawn-driven index never had our id and every id-addressed
system — `apply_entity_attributes` above all — silently `continue`d past our own
`update_attributes`. See
[`swimming.md`](./swimming.md) for what that cost downstream.

Three things about it are load-bearing:

- **It runs first in the `Apply` chain**, so a `Login` and an `update_attributes`
  for our own id in one batch still resolve, by the same sync-point mechanism as
  spawn-then-move.
- **The local player gets only `MinecraftEntityId` and `Attributes`** — no
  `EntityKind`/`Position`/`Rotation`/`HeadYaw`, which would duplicate
  `lodestone_ecs::player::PhysicsState`. `SharedState::entities()` therefore also
  filters `LocalPlayer` explicitly rather than relying on `entity_view` failing
  for want of those four: the shell maps `entities()` straight to render
  instances, so including the local player draws our own body at our own camera,
  and "it happens to be excluded because a component is missing" is exactly the
  accidental invariant that breaks the first time someone adds one.
- **`apply_entity_spawn` and `apply_entity_removal` skip an id held by a
  `LocalPlayer`.** Both `despawn` the previous holder of a reused id; with our own
  id in the index, either one firing would take `PhysicsState`, the HUD component
  set and the driver's `Sim.local` identity with it, and every
  `expect("the local player always carries …")` would panic a frame later.
  Vanilla sends neither for the local player, which is precisely why nothing else
  would have caught it. `the_same_guard_still_replaces_a_reused_id_for_an_ordinary_entity`
  is the control that the guard keys on `LocalPlayer` and has not blanket-disabled
  the replace path.

**Arrival order.** Each system walks the batch in order, so intra-family order is
exact. Cross-family order is the `.chain()` order — but `SharedState::apply`
submits **one event per schedule run**, so a batch never holds two events and the
two orders coincide. A future batching driver must revisit this; the only known
non-commutative pair is "despawn then respawn a reused id", and
`apply_entity_spawn` already handles that on its own by replacing whatever holds
the id.

### Session teardown must clear the index too — it didn't, and rejoining duplicated every entity

`EntityIndex` is populated by `apply_local_player_login` and `apply_entity_spawn`
and was, for a long time, cleared by **nothing**. `Sim::end_session`
(`lodestone-shell/src/sim.rs`) has always been meticulous about resetting session
state — entity tracks, the frame clock's accumulator, four resources, terrain, the
chunk store, the local player's whole component set, both halves of the
HUD/session set, the target and the input state — but the ingest-side entity set
this module owns was simply never in that list.

That is a **second, parallel** entity set from the one `reset_entity_tracks`
(`lodestone-shell/src/entities.rs`) clears. `reset_entity_tracks` only tears down
the *render*-side fold (`TrackIndex`, `ItemStacks`, `ExtractedDraws`) — the
entities `fold_entities` spawns from the ingest component set. It has no reach
into `EntityIndex` or the components this module documents, because those live
one layer upstream, in `SharedState`'s own `World`.

The bug this produced: quit, rejoin, and the new server hands out an entirely
different set of network ids (vanilla never guarantees reuse, and normally
never reuses at all). No `EntityRemoved` for the previous session's entities
ever arrives — nothing sends one; a disconnect just drops the socket — so they
were never despawned. They stayed indexed under ids nothing would ever
reference again, and `SharedState::entities()` (`lodestone-client/src/state.rs`)
enumerates `EntityIndex` **directly**, so it kept deriving `EntityView`s for
them every frame. The shell's render fold then treated each as a "new" entity
(its `TrackIndex` had just been cleared) and spawned a fresh render-side track
for it — frozen in its last-known pose, since nothing could ever address its
dead id again — sitting right beside the live duplicate the new session spawned
under its own fresh id for the same mob. One mob, drawn twice, one copy inert.

A single-session test cannot see this class of bug by construction: within one
session a reused id is already handled (`apply_entity_spawn`'s "replace the
previous holder" branch), and that branch was presumably why this went
unnoticed for so long. It only shows up **across** two sessions using
**different** ids for the same logical entity — which is exactly what a real
rejoin does and a same-id test does not.

The fix is `reset_ingest_entities` (`lodestone-ecs/src/ingest.rs`), called from
`end_session` right next to `reset_entity_tracks` (both run inside the same
`self.write` closure, over the one shared `World` — see `sim.rs`'s own comment
on `IngestPlugin`/`EntityInterpPlugin` sharing a `World` since §4.1(c)). It
despawns every entity `EntityIndex` points at **except** one carrying
`LocalPlayer` — the same guard `apply_entity_spawn`/`apply_entity_removal` use,
for the same reason: the local player's `Entity` id is held by the driver
across the reset and must survive it. It then clears `EntityIndex` in full,
local-player mapping included — that mapping is stale the instant the session
ends anyway, and `apply_local_player_login` re-adds it on the next login by
querying `With<LocalPlayer>` directly, never by reading the index, so clearing
it costs nothing. `EntityIndex::clear()` is the one new method this needed.

No other index required a matching fix: `ServerEntityId` (the local player's
own last-known network id, used to attribute mob effects) is a *component* on
the local player, not a separate map, and was already reset every session by
`insert_session_components`.

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
2. per 20 Hz tick, inside `GameTick`: `TickSet::Physics` → `tick_item_physics`,
   then `TickSet::Animate` → `tick_walk_animation`
3. `fold_entities` (this frame's ingest component state, then the prune)
4. `Extract` / `ExtractSet::Entities` → `extract_entity_draws`

`draws()` is then a plain read of the `ExtractedDraws` resource. It does not
re-extract: a `&self` method cannot run a schedule, and re-extracting per call
would let two reads in one frame disagree.

`EntityInterpolator::world()` / `world_mut()` expose the `World`, which is what
keeps the component set from being an island — a plugin can write `InterpFrom` on
a tracked entity and the next extract puts it on screen.

## How to change it, and the gotchas

- **A resource that indexes entities by network id needs an explicit line in
  `Sim::end_session`, or it survives a rejoin.** `EntityIndex` did not, for a
  long time, and every entity duplicated itself on reconnect — see "Session
  teardown must clear the index too" above. If you add another such index,
  clear it there too, and prove it with a test that spans **two** sessions
  using **different** ids for the same logical entity; a same-session or
  same-id test cannot see this class of bug.
- **Never spawn a component to make a query simpler.** For `DisplayItem` /
  `CustomName` that is the invisible-drop regression; for `Velocity` it erases
  "never reported" vs "reported zero", which is the difference between a drop
  arcing and a drop falling straight down.
- **Adding a field to `EntityView` without adding the component it reads from
  makes it a second source of truth by definition.** The struct has no storage.
- **`step_item_physics` and `lodestone_physics::move_entity` stay plain
  functions.** [`bevy-migration.md`](./bevy-migration.md) §8: the ECS owns state
  and scheduling, never verified math. A system calls them.
- **`tick_item_physics` is now a real system; `fold_entities` is the one thing
  left that is not.** `tick_item_physics` used to be blocked on the same
  `'static`-resource problem `fold_entities`' predecessor (`fold_snapshots`)
  was — a `bevy_ecs` `Resource` must be `'static`, and the workspace denies
  `unsafe_code`, so a borrowed `&dyn CollisionView` (whose owner was a local in
  `Sim::update_entities`) could not reach a system. `lodestone_ecs::player::
  CollisionSource` inverted that: the trait object is `'static` because an
  implementor owns whatever it borrows from, so `tick_item_physics` now runs
  in `GameTick`/`TickSet::Physics` against a `PlayerCollision` resource.
  `fold_entities` staying a hand-called function is now a deliberate choice,
  not a structural block: issue #36 deleted the `&[EntitySnapshot]` slice that
  used to be the obstacle (see "Widened, not deleted" below), but turning
  `fold_entities` into a scheduled system would still mean re-deriving the
  clocks → ticks → fold ordering the next bullet describes, which the #36
  fix deliberately did not touch.
- **The render order is clocks → ticks → fold, which is `Update` before
  `GameTick` and the fold after both** — inverted from the plan's `NetIngest` →
  `GameTick` → `Update` → `Extract`. That is behaviour, not style: every numeric
  expectation in the interpolation tests depends on it. **This did not change
  when `EntitySnapshot` was deleted** — see "Update, and it changes the plan"
  below for why the reorder issue #36's title implied turned out to be a
  separate, unneeded change.
- **`RenderKind` (a path `String`) and `EntityKind` (a `ResourceKey`) are still
  two components for the same fact**, even after `EntitySnapshot`'s deletion:
  `spawn_track` still populates `RenderKind` from `EntityFacts::type_path`, a
  bare `String`, because the render model set is keyed by path rather than by
  `ResourceKey`. Collapsing them is real follow-on debt this pass did not
  attempt — `EntitySnapshot` dying was necessary but not sufficient for it.
- **The item-physics gate's discriminating power was measured, not assumed.**
  Disabling the physics step fails exactly three tests —
  `item_pop_follows_a_ballistic_arc_not_a_flat_ease`,
  `item_pop_stops_at_a_real_floor_instead_of_sinking_through_it`, and the negative
  control `without_a_collision_view_the_same_pop_falls_through_the_floor_height`
  — and the same three, with the same 25 passing, before and after the port to
  components. The two "no apex" controls correctly keep passing when nothing
  moves; they are controls for the apex assertion, not for physics existing.

### Widened, not deleted (issue #36), and why

**This section said "issue #47" in three places and that was wrong** — GitHub #47
is "Command block edit screen". The deletion is tracked by **#36**, "Stage 1b:
delete `EntitySnapshot` and reorder the schedule". A reader following the old
number landed somewhere unrelated. Note also that
`crates/lodestone-server/src/protocol.rs:30` has an **unrelated homonym**
`EntitySnapshot`; the deletion must not touch it.

Issue #36 proposes deleting `EntitySnapshot` outright now that entity state
lives in one component set, reordering the schedule to `NetIngest` → `GameTick`
→ `Update` → `Extract` so ingest writes the render components directly. That
was weighed against simply widening `EntitySnapshot`/`EntityDraw` by three
fields (sheep wool's `variant`, dropped-item `count`) for issues #29/#53, and
widening won — deletion was **not** free enough to be worth doing as a side
effect of an unrelated widening pass:

- **The blocker is structural, not size.** Per the gotcha above,
  `fold_snapshots` is the one fold left that is not a system precisely because
  its input is a borrowed `&[EntitySnapshot]` slice `sim.rs` (held, and
  contested by other in-flight work this session) owns as a plain `Vec` — the
  same class of `'static`-resource problem `tick_item_physics` solved with
  `CollisionSource`, but with no equivalent adapter available here. Deleting
  the type requires ingest to write `InterpFrom`/`InterpTo`/`RenderEquipment`/
  `RenderWool`/`ItemStacks` directly, which only makes sense *after* that
  ownership move, not before.
- **The schedule reorder is a behaviour change, not a refactor.** Every
  numeric expectation across this module's ~30 hermetic tests (interpolation
  windows, walk-cycle sampling, the physics re-anchor) is written against the
  current clocks → ticks → fold order. Reordering to the plan's `NetIngest` →
  `GameTick` → `Update` → `Extract` would need re-deriving and re-verifying
  most of them in the same change — exactly the kind of change CLAUDE.md's
  "one working seam plus a clear list beats twelve half-done layers" argues
  against doing opportunistically.
- **Three added fields is cheap by comparison.** `EntitySnapshot::variant` and
  `::count`, `EntityDraw::wool` and `::count`, one new `TrackedStack` struct
  and one new `RenderWool` component — all additive, none of it touching the
  schedule or the fold's control flow, verified by the existing ~30 tests
  continuing to pass unmodified plus a handful of new ones for the two new
  fields.

Net: deletion is very likely still the better end state — collapsing
`RenderKind`/`EntityKind` and the three-copy pipeline this doc's intro
describes is real debt — but it is a separate, larger, schedule-reordering
change with its own verification burden, not something to fold into a
three-field widening. #36 stays open.

**Update, and it changes the plan.** A later architecture review found the
deletion does **not** require the schedule reorder, and that coupling the two
was the mistake. `EntitySnapshot` exists only to ferry data between two reads of
the *same* `World` — `fold_entities` resolves `entity_snapshots()` *before*
taking the write guard purely to obey the no-reentrancy rule. Reading the
components directly **inside** the fold's existing write guard, at the fold's
existing position in the frame, changes nothing about when an ease begins, so
the ~25 order-pinning tests survive unchanged and only their *setup* moves.

Writing at ingest time is what would break it, for two reasons worth recording:
ingest runs one schedule per event on the net thread, so re-anchoring would
happen per packet instead of once per frame, restarting the `INTERP_WINDOW` ease
under bursts; and `update_track` derives the new `InterpFrom` from the currently
*drawn* pose, which is only correct at a fixed point after
`advance_interp_clocks`, so an ingest-time write races the frame.

The precedent is already in production: `extract_entity_draws` bridges four
ingest components to render output via `EntityIndex`, in the same `World`, at
frame time. The new fold is that pattern applied to the rest.

**Landed.** `fold_entities`/`resolve_entity_facts` (`entities.rs`) and the
test-only `IngestSnap` builder are in `main`, `EntitySnapshot` and
`fold_entity_snapshots` are deleted, and every call site — `net.rs`,
`sim/net_apply.rs`, `sim/tests.rs`, `sim/audio.rs`'s `entity_sound_position`
(which now reads `Sim::entity_draws()` instead of a `NetClient` passthrough),
and the entity pixel gates under `crates/lodestone-shell/tests/` — was
migrated rather than left pointing at a deleted type. `NetClient::entities()`
survives as a thin passthrough to the raw, version-free `EntityView` list,
for the handful of live integration tests (`live_entity_render.rs`,
`live_dropped_item.rs`) that drive a bare `EntityInterpolator` with no
`IngestPlugin` of its own and so have to translate a view into ingest
components by hand (see those files' `apply_view`). The schedule stayed
clocks → ticks → fold, unreordered, exactly as predicted above. Issue #36 is
closed.

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
