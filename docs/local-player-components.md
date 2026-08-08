# The local player as ECS components

## What it is

The local player's physics state, movement intent, submersion, free-fly flag,
hotbar selection, death state and the two outbound-movement edge-trackers, held
as `bevy_ecs` components on one `LocalPlayer` entity and advanced by `GameTick`
systems. This is Stage 2 of [`bevy-migration.md`](./bevy-migration.md).

`lodestone_shell::sim::Sim` holds **none** of it. The fields `player`, `input`,
`profile`, `prev_position`, `fluid_state`, `fly`, `selected_slot`,
`last_player_input`, `last_sprinting_sent` and `dead` are deleted; what is left
in their place is accessors that read and write the components. That is the
stage's authority test — with a second copy on `Sim`, a plugin's write to
`PhysicsState` would be a write to nothing.

It lives in three places, and the split is forced by the dependency graph rather
than chosen:

| where | what |
|---|---|
| `crates/lodestone-ecs/src/player.rs` | the components, the resources, `CollisionSource`, and `TickSet::Physics` |
| `crates/lodestone-controller/src/ecs.rs` | `RawInput` plus `TickSet::Input` and `TickSet::Send` |
| `crates/lodestone-shell/src/sim.rs` | the `Sim` struct, the two `CollisionSource` adapters, and the lock-scoped accessor layer every seam file reaches the `World` through |
| `crates/lodestone-shell/src/sim/step.rs` | the driver: the fixed-timestep loop, and draining the action queue |
| `crates/lodestone-shell/src/sim/collide.rs` | resolving this tick's collision (`tick_collision`, `live_collision`, `tick_nearby_entities`) |

`lodestone-controller` depends on `lodestone-client`, which depends on
`lodestone-ecs`. So `lodestone-ecs` can never name `InputState`, and the
input/egress systems have to live in the controller. That is also the right
home: the controller crate exists so native and browser share one held-keys →
`MovementInput` implementation, and putting the rule anywhere else would reopen
the movement fork it was extracted to close.

## How it works

### The tick

```
Sim::step(dt)
  apply_mouse                       ← per frame, like vanilla's MouseHandler
  Egress { in_world, live }         ← refreshed once per frame
  while accumulator >= TICK_DT:
      PlayerCollision ← Sim::tick_collision()
      run_schedule(GameTick):
          TickSet::Input     compute_movement_intent → tick_sprint_window
          TickSet::Physics   player_physics
          TickSet::Send      send_move_action → send_player_input
      drain ActionQueue → NetClient::send_action
      particles / HUD effects / title / action bar
      drive_interaction()           ← sprint edge + mining, still Sim methods
```

`TickSet::Send` running last is the point of the stage: a plugin adding a system
`.after(TickSet::Physics).before(TickSet::Send)` changes what the server is told
this tick. There is a test for exactly that
(`a_plugin_between_physics_and_send_changes_what_is_reported`), and a shell-level
one that a write through `Sim::ecs_mut()` reaches the wire
(`a_write_through_the_world_reaches_the_wire`).

### Two orderings inside the tick that are behaviour, not style

- **`compute_movement_intent` before `tick_sprint_window`.** The pre-Stage-2
  driver computed the intent, ran physics, and only then aged the double-tap
  window, so a tick's intent was read from the *un-aged* input. Swapping these
  moves the double-tap sprint window by one tick.
- **`tick_sprint_window` must stay in this fixed 20 Hz schedule.** Vanilla's
  `sprintTriggerTime` is counted in ticks (default 7), so ageing it per frame
  makes the double-tap window frame-rate dependent — wider at 144 fps than at 30.

### Movement intent is now per tick, not per frame

`movement_intent` used to be computed once per `Sim::step`, *outside* the
`while accumulator >= TICK_DT` loop, so a frame long enough to run several
catch-up ticks reused one decision for all of them. What changes observably:

- a double-tap sprint window that expires part-way through a multi-tick frame
  now stops applying on the tick it expires, not at the end of the frame;
- the submersion the swim-sprint exception reads is the previous *tick*'s, not
  the previous *frame*'s;
- **at any frame rate at or above 20 fps, nothing changes at all** — a frame runs
  at most one tick. The difference is confined to stalls.

The one-tick lag on submersion is deliberate and is vanilla's own: `baseTick`
computes submersion before `aiStep` reads it.

### The collision borrow, and why `CollisionSource` exists

A `Resource` must be `'static` and the workspace denies `unsafe_code`, so a
`&dyn CollisionView` cannot reach a scheduled system. `Arc<dyn CollisionView +
Send + Sync>` handles the live path — `Sim::live_collision` already returns an
owned snapshot — but not the offline demo world, whose adapter
`WorldCollision<'a>` borrows the world outright.

`lodestone_ecs::player::CollisionSource` inverts it:

```rust
pub trait CollisionSource: Debug + Send + Sync + 'static {
    fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView));
}
```

An implementor may therefore build a *borrowed* view over state it owns. The two
implementors are in `sim.rs`: `LiveCollisionSource(LiveCollision)` and
`DemoCollision(World)` (an owned clone, rebuilt lazily).

This is strictly better than `Arc<dyn CollisionView>` for a second reason. An
owned wrapper around `WorldCollision` would have to re-delegate all thirteen
`CollisionView` methods by hand, and a method added to the trait later would be
overridden by `WorldCollision` while silently falling back to the trait default
in the wrapper — the "two adapters, one of them subtly wrong" failure
`lodestone_shell::collision`'s module docs warn about. `with_view` constructs the
real adapter and asks it.

`LiveCollision: Send + Sync` was recorded as "likely, unverified" by Stage 1. It
**holds**, and is now pinned by
`both_collision_sources_are_send_sync_and_static` rather than left incidental.

### `PlayerCollision` has three variants and two of them freeze

| variant | meaning | pose updated? |
|---|---|---|
| `NoWorld` | no session *and* no offline terrain | **no** |
| `Pending` | live session, player's column not streamed in | yes |
| `View(_)` | collide against this | yes |

The pose asymmetry is inherited verbatim from the pre-Stage-2 code, where
`NoWorld` returned before `update_pose` and the live-`None` branch did not. It is
preserved deliberately — tidying it would change the eye height, and therefore
the camera, on the title screen while a key happens to be held — and pinned by
`pending_updates_the_pose_while_no_world_does_not`. It is a latent question, not
a settled one.

### Egress

Systems never touch a socket. `TickSet::Send` pushes into the `ActionQueue`
resource and `Sim::drain_action_queue` hands it to `NetClient` in order, once per
tick. The queue is drained even with no connection, so a disconnected session
cannot accumulate actions to deliver on reconnect.

`Egress { in_world, live }` is a *derived* gate the driver refreshes each frame
from `SessionPhase` and `Sim::is_live()` — not a second copy of them. It gates
the **latch**, not just the send: a `send_player_input` that ran while
disconnected would record the current input into `LastPlayerInput` as "already
sent", and the first real change after connecting would then be suppressed as a
redundant resend. There is a test (`a_closed_session_queues_nothing_and_latches_nothing`).

Session phase moved onto the local player in Stage 3 (`lodestone_ecs::session::Phase`,
see [`session-components.md`](./session-components.md)) and `Egress` **did not**
collapse into it: `in_world` is now derived from that component, but `live` is
`vanilla_atlas.is_some() && net.is_some()`, an asset/config fact rather than a
phase. The resource survives as the derived two-bit gate it already was.

## How to change it, and the gotchas

- **`lodestone-physics` stays a plain library.** `player_physics` reads
  components, calls `lodestone_physics::tick`, writes components back. Do not
  move the integrator into a system: it is bit-exact against a JVM oracle with
  golden traces, and a system that re-derived the integration would be
  re-deriving the oracle from the code under test
  ([`bevy-migration.md`](./bevy-migration.md) §8).
- **Anything that mutates `Sim.world` must clear `Sim.demo_collision`.** Today
  that is only `Sim::set_block_world`, which is the one write path after
  construction (live terrain lives in the net client's world). A missed
  invalidation is a player colliding against pre-edit geometry — "I mined the
  block but still cannot walk through it". The snapshot is rebuilt lazily rather
  than per tick because the clone is `O(loaded columns)`.
- **`spawn_local_player` inserts every component eagerly**, unlike the *observed*
  entity set in [`entity-components.md`](./entity-components.md) where absence
  encodes "the server has never mentioned this". Nothing about the local player
  is server-reported in that sense, so there is no three-state encoding to
  preserve. The one exception is `Dead`, a marker precisely so alive is the
  default.
- **Add a component? Add it to `reset_local_player` too.** That one function is
  what makes a quit-to-title behave like a first connection; a component added to
  the spawn and forgotten here leaks the old session's value into the new one.
  It deliberately does not despawn-and-respawn, because the `Entity` id is held
  by the driver and (later) by plugins.
- **A player starting from rest does not move on its first tick** — `tick` runs
  `move()` before applying gravity. A one-tick test therefore asserts nothing;
  burn the settle tick first. Two tests in `player.rs` carry that note.
- **This is the third `World` in the process** (the net thread's, the entity
  interpolator's, and `Sim`'s). It could not be the interpolator's: that `World`
  runs its own `GameTick` off its own accumulator inside
  `EntityInterpolator::update_with_view`, so registering the player systems there
  would tie the player's tick rate to the interpolator's — a behaviour change,
  not a refactor. Unification is [`bevy-migration.md`](./bevy-migration.md) §4.1.
  The consequence today: a plugin adding a `GameTick` system has to pick which
  `App`.

## Four driver-pushed resources (issues #201, #206, #208)

`PlayerCollision`, `Profile` and `NearbyEntities` were the original "the driver
knows this, physics needs it" resources. Four more joined them, all with the same
shape — the driver writes once per tick before `run_schedule(GameTick)`, a
`TickSet::Physics` system carries the value into `PlayerState`:

| resource | default | driver source | consumer |
|---|---|---|---|
| `AutoJump(bool)` | **`true`** | `Options::auto_jump` | `player_physics` → `PlayerState::auto_jump_enabled` |
| `GliderEquipped(bool)` | `false` | `Sim::glider_equipped` (chest armour slot) | `update_fall_flying_state` |
| `FireworkBoost(u32)` | `0` | `Sim::use_item_live` | `tick_firework_boost` |
| `ItemUseTicks(Option<u32>)` | `None` | `Sim::use_item_live` arms, `Sim::end_use_live` takes | `tick_item_use` |

### `AutoJump` is a bug fix, not a feature (#201)

Auto-jump was **already implemented** — `lodestone_physics::update_auto_jump` is
a full port of `LocalPlayer.updateAutoJump`, swept look-ahead probe, headroom
raycast and the `-0.15F` facing-vs-moving dot product included. Its one gate is
`PlayerState::auto_jump_enabled`, which defaults to `true` and whose only setter
(`with_auto_jump`) was called **from tests only**. Meanwhile `sim/step.rs` held a
*second*, deliberately simplified probe in front of it, correctly gated on the
option.

So the option suppressed the simplification and the real detector armed a jump
anyway: **auto-jump could not be turned off.** Two implementations, and the
ungated one won — the same shape as a hand-rolled command parser sitting in front
of a complete command tree.

The fix is this resource plus one line in `player_physics`, and the deletion of
the shell probe (and of `InputState::auto_jump_requested`, whose only producer it
was — an input bit with no producer is the island shape in reverse). The gate is
`lodestone-physics`' `tests/auto_jump_facing_gate.rs`, which brackets the `-0.15`
constant from either side, plus
`player::tests::the_auto_jump_option_off_really_stops_the_detector` here, which is
the assertion that was impossible to satisfy before.

`true` as the default is deliberate: it is both vanilla's option default and
`PlayerState`'s own field default, so every harness that adds `LocalPlayerPlugin`
without pushing the option is bit-identical to before. The *shell's*
`Options::auto_jump` defaults to `false`; that is the shell's choice, and it now
actually reaches physics.

### Glide state is client-authoritative on the way in and predicted on the way out (#206)

`update_fall_flying_state` runs **before** `player_physics` (vanilla does both
before `travel()`) and does two things:

- **start**: on the jump-key rising edge, `lodestone_physics::try_start_fall_flying`
  — `!isFallFlying() && canGlide() && !isInWater()`. Vanilla's client sets the
  shared flag itself and tells the server afterwards, which is what
  `send_fall_flying_command` does.
- **stop**: `lodestone_physics::update_fall_flying`, the `!canGlide()` branch of
  `LivingEntity.updateFallFlying`. Vanilla runs that **server-side only** and
  syncs the cleared flag back; this client has no server that tracks glide state,
  so it is predicted. Without it, landing leaves `fall_flying` set,
  `lodestone_physics::tick` keeps routing to `tick_elytra`, and the player can
  never walk again.

The edge uses its own `WasJumpingGlide` component rather than `WasJumping`,
because `apply_creative_flight_input` overwrites that one at the end of its body —
a later system in the same tick would see this tick's value and never observe an
edge. **Two consumers of one edge need two latches**, and the failure is silent.

`send_fall_flying_command` is registered at the **tail of the `TickSet::Physics`
chain**, not in `TickSet::Send`. Vanilla sends `START_FALL_FLYING` from inside
`aiStep`, before `sendPosition()`, so this reproduces the wire order — and
mechanically it *has* to be here, because it writes `ResMut<ActionQueue>` and this
crate cannot name `lodestone_controller`'s two `Send` writers to order against
(the controller depends on this crate). An unordered second `ActionQueue` writer
in `Send` fails `exactly_one_system_writes_movement_intent`'s ambiguity build,
which is how it was caught.

## What deliberately did not move, and why

- **`Mining` and `Placement`.** [`bevy-migration.md`](./bevy-migration.md) Stage 2
  lists them, but they are not player state — they are interaction predictors
  whose inputs (`Sim.target` from the raycast, `version_data`, the live block
  store, the particle emitter, direct demo-world edits) all belong to Stages 3
  and 4. Mirroring those into resources to make a system possible now is exactly
  the second-source-of-truth the migration exists to delete. Same for
  `send_is_sprinting_if_needed`, which needs `local_entity_id`.
- **`PlayerSnapshot` (`lodestone-client`'s `state.rs`).** Also named by Stage 2,
  and it is in a different crate and a different `World`. It folds the *server's*
  view of the player from `ClientEvent`s on the net thread; the components here
  are the *client's* prediction on the driver thread. Collapsing them is the
  `World` unification, not this stage.
- **`apply_mouse`.** Mouse-look is per-frame in vanilla too
  (`MouseHandler.turnPlayer` runs off the render loop, not the tick), so binding
  it to 20 Hz would make aiming feel stepped at high frame rates.

## Two fixes that came with the stage

- **`send_player_input` now reports the intent physics actually used.** It used to
  recompute `movement_intent(&input)` for itself, which vetoes sprint while
  sneaking — so a *submerged* player holding shift and sprint had physics
  swim-sprinting (`swim_adjusted_intent`) while the wire said `sprint: false`.
  Reading the `MovementIntent` component removes the disagreement.
- **`tool_mining_item` carries the `minecraft:tool` patch.** It used to build a
  fresh `ItemComponents::default()` (`ToolPatch::Inherited`), so an explicit wire
  override (`/give …[minecraft:tool={…}]`, or `[!minecraft:tool]`) resolved as if
  the item default applied — a custom-speed pickaxe dug at its vanilla rate, and
  a tool-stripped pickaxe dug like a real one. The canonical
  `lodestone_game::item::ItemStack` has carried the patch since `67ff7c3`; this
  reads it back. `Removed` is the direction that failed *unsafely* (predicting a
  break the server will not grant), so both are tested.

## Configuration

None. No feature flags, no env vars. `FLY_SPEED`, `SWIMMING_EYE_HEIGHT` and
`CROUCHING_EYE_HEIGHT` are `pub const`s in `lodestone_ecs::player`, moved there
from `sim.rs` so the systems that read them and the tests that assert them name
one definition.

## Dependencies

- `lodestone-ecs` → `bevy_app`, `bevy_ecs`, `parking_lot`, `lodestone-model`,
  `uuid`, and now **`lodestone-physics`** (`PlayerState`, `MovementInput`,
  `FluidState`, `CollisionView`, `PhysicsProfile`). Never a version crate.
- `lodestone-controller` → `lodestone-physics`, `lodestone-client`, and now
  **`lodestone-ecs`**, `lodestone-model`, `bevy_ecs`, `bevy_app`. All wasm-safe;
  the crate's own `no_wasm_trap_symbols_are_confined` guard still passes.
- `lodestone-shell` → unchanged (it already had both).
