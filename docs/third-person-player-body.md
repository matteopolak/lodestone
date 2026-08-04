# The third-person player body

## What it is

The render-side path that turns the local player's own state into a drawable
third-person avatar — full rig, both skin layers, walking/head-look animation —
reusing the same code every other humanoid entity draws through. Landed in
`81f4cc4` (`crates/lodestone-render/src/entity.rs`,
`crates/lodestone-shell/src/gpu.rs`). **Pose path only**: the camera and input
side that would make this visible do not exist yet — see "Zero pixels" below.

## How it works

### Almost all of the machinery already existed

`player_model()` already built the full rig (head+hat, body+jacket,
arms+sleeves, legs+pants, both layers, both `player_wide` and `player_slim`),
and `Skeleton::pose`'s humanoid branch already computed the walk cycle, head
look and arm swing generically — every zombie and every remote player already
animates through it. The actual gap was narrower than "build a player
renderer": the local player is never a tracked network `EntityDraw` (nothing
sends the client its own entity-movement packets), so nothing ever constructed
one for it, and nothing selected `player_slim` over the hard-coded
`player_wide` default. Closing that gap is the entire content of this change.

### The bridge: `ThirdPersonBodyState` → `EntityDraw`

`ThirdPersonBodyState` (`gpu.rs`) is the shell's-eye view of "what does the
local player's avatar look like right now": `feet`, `body_yaw_deg`, an
`AnimInput` (head look, walk cycle, idle age — built the same way
`entities.rs`'s `Track::render_anim` builds one for a network entity),
`scale`, `slim` (rig choice), and `equipment` (`MainHand`/`OffHand` only, same
as every other entity's equipment).

`ThirdPersonBodyState::into_draw` converts this into an ordinary `EntityDraw`
with a reserved id, `LOCAL_PLAYER_DRAW_ID = -1` (real entity ids are
server-assigned non-negative `VarInt`s, so this can never collide with a
tracked mob), and `type_path` set to `player_model_name(slim)` — a literal
`"player_wide"` / `"player_slim"` that
`lodestone_render::entity::canonical_model_name` resolves through its existing
corpus-name fallback with no new plumbing. Once it is an `EntityDraw`, it goes
through the *exact* resolve → cull → pose → upload path
(`RenderState::prepare_entities`) and the *exact* held-item path
(`RenderState::merge_held_items`) every other humanoid already uses — not a
second copy of either.

### Where it's spliced in: `render_inner`

`RenderState::render` polls `self.third_person_body.sample()`
(a `ThirdPersonBodySource`, the same polled-closure idiom as
`EntityLightSource`/`SkyDarkenSource`) once per frame. `Some(state)` clones the
incoming `entities: &[EntityDraw]` slice, pushes `state.into_draw()` onto the
copy, and uses that extended slice for the rest of the frame (entity
preparation, item-geometry preparation for held items, and so on).
`stats.third_person_body_drawn` records whether this happened, surfaced on
`RenderStats` for the debug overlay.

`None` — true for every caller today, since nothing calls
`set_third_person_body_source` — reproduces prior behaviour exactly: `entities`
passes straight through unmodified and the first-person arm draws
unconditionally, below.

### The deliberate split from the first-person arm

The first-person arm (`first_person_arm_chain` / `first_person_arm_pose`,
`crates/lodestone-render/src/entity.rs`) and the third-person body **must never
share a pose function**, and this is not an accident of scope — it was called
out before this change existed. `first_person_arm_pose`'s own doc comment says
why:

> `AvatarRenderer.renderHand` calls `arm.resetPose()` and then forces
> `zRot = ±0.1F`, so the arm **part** is drawn from its authored rest pose with
> one rotation replaced — never from the third-person `setupAnim` result. That
> is why this is a separate function from `EntityInstance::part_transforms` and
> must stay one: the third-person player body needs the animated chain, and
> sharing a code path would silently give one of the two the other's pose.

Concretely: the arm part's pose is *authored rest pose, one rotation swapped
in* — it never bends with a walk cycle or looks with the head. The body's pose
is the *animated* `Skeleton::pose` result every mob uses — it must swing its
arms when walking and turn its head to look around. Point one function at the
other's job and either the first-person arm starts animating (wrong — vanilla
never does this) or the third-person body freezes into the arm's static rest
pose (wrong — it would not visibly walk).

**The arm swing does not change this, and is worth understanding as the
example.** Since [`arm-swing-animation.md`](./arm-swing-animation.md) both paths
*are* driven by the same swing scalar — `Sim::hand_swing_progress`, on
`AnimInput::attack_anim` for the body and via `HandSwingSource` for the arm — and
they still share no code, because vanilla puts the swing in two structurally
different places:

- **first person**: the swing is in the *camera-space chain* that a **rested**
  arm part hangs off (`first_person_arm_chain`'s five `attackValue` terms). The
  part pose stays rest.
- **third person**: the swing is in the *arm part* inside an otherwise ordinary
  body (`HumanoidModel.setupAttackAnimation`, i.e. `Skeleton::pose`'s
  `attack_anim`). The chain is the ordinary world model matrix.

So the sameness stops at the scalar. Feeding either pose function the other's
chain produces a plausible-looking wrong arm — the failure this section exists to
prevent, now with a concrete instance rather than a hypothetical one.

**The two are also mutually exclusive on screen, by construction, not by
convention.** `render_inner` skips `prepare_first_person_arm` entirely on any
frame `third_person_body_drawn` is `true`:

```rust
let first_person_arm = if stats.third_person_body_drawn {
    None
} else {
    self.prepare_first_person_arm(device, queue, camera)
};
```

The reason is structural, not aesthetic: **the arm has no world position at
all.** It is drawn in a second camera-space pass with no section origin and no
instance matrix — vanilla's own first-person hand is drawn relative to the
camera, not placed in the world. Drawing it alongside a third-person body
would not "double up the arms"; it would composite a camera-locked hand in
front of a body that itself does not exist from that vantage point in vanilla
either. There is no `RenderState` field for "draw both" because the source of
truth (`third_person_body: ThirdPersonBodySource`) is a single `Option`, not
two independent toggles — the mutual exclusion is the type, not a runtime
check that could be gotten wrong.

### Determinant signs needed no new derivation, but got wider coverage

The chain reuses `entity_model_matrix` (determinant `+1`, since `S(-1,-1,1)` —
Minecraft's usual X/Y flip for its model convention — composes two negative
scale factors) with the same rigid local matrices every mob's pose already
uses. Nothing new needed deriving. What changed is the *test surface*: it is
now verified for every part of the real player mesh, including both overlay
layers (`hat`, `jacket`, `right_sleeve`, `left_sleeve`, `right_pants`,
`left_pants`), across both rigs (`player_wide`/`player_slim`) — see
`third_person_body_state_resolves_through_the_real_corpus`
(`crates/lodestone-shell/src/gpu.rs`) — rather than for the first-person arm
alone, which is what the pre-existing winding gates covered.

## Wired: the camera mode, the pullback, and the source (reaches pixels now)

The two things the section below used to describe as missing now exist, both
in `crates/lodestone-shell`:

1. **The camera-mode toggle** is `Sim::third_person: bool`
   (`crates/lodestone-shell/src/sim.rs`), flipped by `Sim::toggle_third_person`
   and bound to `F5` in `app.rs`'s `WindowEvent::KeyboardInput` arm — vanilla's
   own key for the same toggle. Exactly one bool, per
   `ThirdPersonBodySource`'s own design note quoted below: there is still no
   richer "camera mode" enum anywhere, because `Sim::render_camera` and
   `Sim::third_person_body_state` both read this one flag and a `None`/`Some`
   split from the latter *is* the toggle as far as `gpu.rs` is concerned.
2. **The collision-aware pullback** is
   `crate::camera_rig::{third_person_camera, collision_pullback}`
   (`crates/lodestone-shell/src/camera_rig.rs`). `collision_pullback` marches
   voxel-by-voxel along the camera's own backward view direction (the same
   grid-DDA traversal `raycast.rs` uses for block targeting) and, at each
   voxel, ray/AABB-intersects the *real* per-state collision boxes
   (`CollisionView::collision_boxes`) rather than the coarse `is_solid`
   occlusion predicate — `LiveCollision::is_solid`'s own doc comment warns that
   method stopped being the collision answer, so a pullback built on it would
   pull the camera in a full block early on every slab and could clip straight
   through a barrier (collides, occludes nothing). `Sim::render_camera` builds
   the `CollisionView` the exact way `Sim::update_target` already does — live
   session → `LiveCollision` snapshot (falling back to an empty
   [`NoCollision`] if the player's own column has not streamed in yet, so
   "no data" pulls back the full desired distance rather than jamming the
   camera against nothing), offline fixture → `WorldCollision`. Vanilla's
   default zoom (`4.0` blocks, `camera_rig::THIRD_PERSON_DISTANCE`) is the
   desired distance; a hit shaves an extra `0.1`-block margin
   (`camera_rig::COLLISION_MARGIN`) so the eye stops just short of the surface
   rather than sitting exactly on it.
3. **The source** is `Sim::third_person_body_state`, called fresh every frame
   from `app.rs`'s `redraw()` and handed to
   `render.set_third_person_body_source(move || body_state.clone())` right
   there — a cheap `Option<ThirdPersonBodyState>` clone per frame, not a
   captured borrow of `Sim` (which the closure's `'static` bound would not
   allow anyway, since `Sim` and `RenderState` are sibling fields on the same
   `WindowApp`). `anim`'s walk cycle and idle age come from `Sim::body_pose`
   (a `lodestone_entity::pose::EntityPose`), ticked once per physics tick from
   the player's own post-physics position exactly the way `entities.rs`'s
   `Track::render_anim` drives one for a tracked network entity — but facing
   (`body_yaw_deg`/`head_pitch_deg`) is read straight off the *interpolated
   player state* instead of that pose's own smoothed rotation, so the local
   avatar's facing never lags the camera by a tick the way a *remote* player's
   body yaw legitimately does. Held items are exactly the "rides along for
   free" case this doc predicted: main hand (selected hotbar slot) and off
   hand (native inventory index `40`, per `lodestone_game::menu`'s own slot
   table) both resolve into `ThirdPersonBodyState::equipment`.

Two gaps carried forward exactly where the equivalent gap already existed
elsewhere, not guessed at: **head yaw never diverges from body yaw**
(vanilla's `LivingEntity.tickHeadTurn` is not modelled for the local player
anywhere in this engine, so `AnimInput::head_yaw_deg` is always `0`), and
**`slim` stays `false`** (no real skin-model bit exists yet — unchanged from
this doc's original "How to change it" note below, which is still exactly
right about the fix).

`update_target` and `set_audio_listener` deliberately keep reading `Sim::camera`
(the true, un-pulled-back eye) rather than `Sim::render_camera` — block
interaction and (for now) audio should not move just because the camera did.

### What used to block this, verbatim (for the historical record)

Per this repo's own dominant-defect-class rule (see `CLAUDE.md`, "nothing is
done until something on screen changes"), this section used to name the two
things that made this an island:

1. **A third-person camera mode.** `ThirdPersonBodySource`'s doc comment
   states the design choice plainly: *"There is no separate 'camera mode' enum
   here on purpose: `f` returning `None` **is** first person, and `Some` **is**
   third person"* — so whatever eventually adds a camera-mode toggle does not
   need to tell this module about it separately; it just needs to *exist* and
   call `set_third_person_body_source` when the mode flips.
2. **A collision-aware pullback.** Vanilla's third-person camera is not simply
   "the same eye position, pulled back": it raycasts from the head to avoid
   clipping through blocks behind the player. Nothing in this client computed
   that yet.

Both now exist (see above), and `Sim::third_person_body_state` is exercised by
`app.rs`'s `redraw()` every frame the camera mode is third person — not just
reachable in principle, but on the one path a shipped binary actually runs.

## How to change it, and the gotchas

- **Never point `ThirdPersonBodyState`'s pose at `first_person_arm_pose`, or
  vice versa.** See "The deliberate split" above — this is the one invariant
  most likely to get "simplified" away by someone who does not know why the
  split exists.
- **The camera mode is wired**: `app.rs`'s `redraw()` calls
  `render.set_third_person_body_source(move || body_state.clone())` fresh
  every frame, where `body_state = self.sim.third_person_body_state()`. If you
  need a *different* camera-mode source (a settings toggle instead of `F5`,
  say), change what flips `Sim::third_person` — nothing about the source call
  itself needs to change, per `ThirdPersonBodySource`'s own "the bool is the
  toggle" design.
- **The pullback lives in `camera_rig.rs`, not `gpu.rs`.** `Sim::render_camera`
  is the only caller of `camera_rig::third_person_camera`; if the desired
  distance or margin ever need to change, they are
  `camera_rig::THIRD_PERSON_DISTANCE`/`COLLISION_MARGIN`, not new constants in
  the shell.
- **Real skin data is still unmodelled.** `slim` has no per-player signal to
  read yet (the tab-list player-info packet would carry it, decoded in the
  network layer) — `Sim::third_person_body_state` always passes `false`
  (`player_wide`), reproducing the arm's existing default. `player_model_name`
  exists specifically so that the day skin data arrives, selecting the right
  rig is a one-line change at the call site, not new plumbing in
  `lodestone-render`.
- **`bobView`/`bobHurt`/`equipProgress` stay unmodelled**, consistent with the
  first-person arm's own documented gaps (`prepare_first_person_arm`'s doc
  comment). Adding them is a shell-side animation-input concern, not a change
  to this bridge.
- **Head yaw never diverges from body yaw.** `Sim::third_person_body_state`
  always passes `head_yaw_deg: 0.0` — vanilla's independent
  head-turn-then-body-catches-up (`LivingEntity.tickHeadTurn`) is not modelled
  for the local player anywhere in this engine. Adding it is a `sim.rs`-side
  animation-input concern (a second smoothed yaw alongside `Sim::body_pose`),
  not a change to this bridge.
- **Held items ride along for free.** Because the body is folded into the same
  `entities` slice `prepare_item_geometry` reads, the local player's own held
  item renders through `merge_held_items` exactly like a mob's does — no
  separate held-item path was added for the third-person case.
  `Sim::third_person_body_state` resolves `MainHand` (selected hotbar slot),
  `OffHand` (native inventory index `40`), and all four armour slots (native
  indices `39/38/37/36` for head/chest/legs/feet) into
  `ThirdPersonBodyState::equipment` — see
  [`armour-rendering.md`](./armour-rendering.md) for the slot table and why the
  native indices run backwards. This line previously said armour was
  deliberately not carried; that was true when written and stale by the time
  it was re-read for #armour-rendering's follow-up pass — the local player's
  own armour draws as of `22dc0ee`.

## Configuration

No feature flag or env var. In-game, `F5` toggles `Sim::third_person`, which is
the entire "camera mode" state — `ThirdPersonBodySource` unset (`gpu.rs`'s
constructed default) is no longer reachable in a real session once `app.rs`
installs the per-frame source at first draw, but remains the correct "feature
off" behaviour for any harness that never calls
`set_third_person_body_source` at all (every existing render test, and
`--headless`).

## Dependencies

- `lodestone-render::entity` — `player_model_name`, `ThirdPersonBodyState`'s
  target shape (`EntityDraw`), `canonical_model_name`'s corpus-name fallback,
  and the pre-existing `Skeleton::pose` humanoid branch this reuses.
- `lodestone-render::entity_anim` — `AnimInput`, the animation-drive shape
  shared with every other tracked entity.
- `lodestone-entity::pose::EntityPose`/`WalkAnimation` — drives
  `Sim::body_pose`'s walk cycle and idle age, the same machinery
  `entities.rs` uses for every tracked network entity.
- `crates/lodestone-shell/src/gpu.rs` — `RenderState::render_inner` (the splice
  point), `RenderState::prepare_entities`/`merge_held_items` (the shared path),
  and `RenderState::prepare_first_person_arm` (the path this mutually
  excludes). Read-only dependency for this pass — `gpu.rs` itself was held by
  concurrent work throughout and none of the wiring below needed to touch it.
- `crates/lodestone-shell/src/camera_rig.rs` — `third_person_camera` and
  `collision_pullback`, the render-camera half of the wiring.
- `crates/lodestone-shell/src/collision.rs` — `LiveCollision`/`WorldCollision`,
  read (not edited) for their real `CollisionView::collision_boxes`
  implementations, exactly as `Sim::update_target` already reads them.

## See also

- [`docs/entity-rendering.md`](./entity-rendering.md) — the general
  resolve/cull/pose/upload pipeline this bridges into.
