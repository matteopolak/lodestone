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

### Player skins are translucent model data, not ordinary mob cutouts

26.2 constructs `PlayerModel` with `RenderTypes::entityTranslucent`; it does
not inherit the opaque/cutout render type that most living models use.
`EntityRenderer` therefore routes only the `player_wide` and `player_slim`
body batches through `EntityPipeline::player_skin_pipeline`. That pipeline
keeps the standard entity depth state (`LessEqual` in Lodestone's depth space,
with depth writes) but follows `ENTITY_TRANSLUCENT`'s blend state and `0.1`
alpha cutout. This preserves a skin's partially-alpha outer-layer texels
without changing armor's `ARMOR_CUTOUT_NO_CULL` contract.

The two small openings at the top of the vanilla diamond chestplate are not a
depth error: `textures/entity/equipment/humanoid/diamond.png` deliberately has
zero-alpha texels in its body-front UV rectangle (`20..28 × 20..32`). The skin
or jacket behind those holes is supposed to remain visible. Do not close them
with a mask or an armor offset; that would be a non-vanilla visual patch.

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

1. **The camera mode** is `Sim::camera_type: camera_rig::CameraType`
   (`crates/lodestone-shell/src/sim.rs`), advanced by `Sim::cycle_camera_type`
   and bound to `F5` in `app/lifecycle.rs`'s `KeyOutcome::TogglePerspective` arm
   — vanilla's own key.

   **This was a `bool` and this paragraph argued no richer enum was needed.**
   That argument was half right and shipped a missing feature: vanilla's
   `CameraType` has *three* states (`FIRST_PERSON`, `THIRD_PERSON_BACK`,
   `THIRD_PERSON_FRONT`), so a bool simply had no front view — the owner
   reported it as "the other third-person perspective is missing". The part that
   was right is the seam: `ThirdPersonBodySource`'s `None`/`Some` split is still
   the whole of what `gpu.rs` knows about camera mode, because it answers
   `CameraType::isFirstPerson()`, and that genuinely is two-valued.

   The distinction that matters when you touch a consumer:

   | ask | who asks it | why |
   |---|---|---|
   | `is_first_person()` | `Sim::render_camera`'s early return, `Sim::third_person_body_state`, and hence `RenderStats::third_person_body_drawn` → the first-person arm and the first-person screen-overlay group in `gpu/frame.rs`, and the spyglass FOV zoom | "is the camera in the player's head". Vanilla's own predicate at every one of these sites |
   | `is_mirrored()` | `camera_rig::third_person_camera` **only** | "is this the front view", exactly as in `Camera.alignWithEntity`'s detached branch |

   Asking "is the camera *behind* me" at an `is_first_person` site is how the
   first-person arm and the pumpkin/underwater overlays reappear in the front
   view. No `cargo check` sees that — the bool still compiles.

   The front view itself is not a second camera: `Camera.java:266-271` is
   `setRotation(this.yRot + 180.0F, -this.xRot)` followed by the *same*
   `move(-getMaxZoom(...), 0, 0)` pullback, so `third_person_camera` mirrors the
   two angles by field assignment and then reads `forward()` back off the
   mirrored camera. The angles are left **unwrapped**, as vanilla leaves them;
   an earlier draft wrapped into `-180..180` and got the wrap backwards, mapping
   a yaw of `0` to `0` — i.e. no mirror at all at the one heading a spot check
   is most likely standing on.
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
   `Track::render_anim` drives one for a tracked network entity. Facing now
   models `LivingEntity.tickHeadTurn` for real — see "Head yaw now diverges
   from body yaw" below. It used to read `body_yaw_deg` straight off the
   *interpolated player state* (the raw look yaw) with no lag and no clamp at
   all, so the local avatar's body always faced exactly wherever the camera
   did. Held items are exactly the "rides along for free" case this doc
   predicted: main hand (selected hotbar slot) and off hand (native inventory
   index `40`, per `lodestone_game::menu`'s own slot table) both resolve into
   `ThirdPersonBodyState::equipment`.

One gap carried forward exactly where the equivalent gap already existed
elsewhere, not guessed at: **`slim` stays `false`** (no real skin-model bit
exists yet — unchanged from this doc's original "How to change it" note
below, which is still exactly right about the fix).

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
- **The camera mode is wired**: `app/redraw.rs`'s `redraw()` calls
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
- **The sneak pose is wired, and it is the pose and not the shift key.**
  `AnimInput::crouching` drives `Skeleton::pose`'s humanoid crouch branch
  (`HumanoidModel.java:274-284`: `body.xRot = 0.5F`, arms `+= 0.4F` pitch and
  `+= 3.2` texels down, legs `z += 4.0`, head `y += 4.2`, body `y += 3.2` —
  assigning the body pitch, adding everything else). It sits after the attack
  swing and before the idle arm bob, which is vanilla's order and is observable
  in both directions: the swing twists `body.y_rot` while the crouch assigns
  `body.x_rot`, and the bob still rides on top of the lowered arms.

  Two producers feed it, and both had to be right about *which* flag:
  `Sim::third_person_body_state` reads `PlayerState::pose == Pose::Crouching`
  (the fit-gated pose `lodestone_physics::pose::update_player_pose` already
  writes every tick — the same thing that lowers the eye to `1.27`), and
  `entities.rs`'s `extract_entity_draws` reads the `Pose` ingest component, which
  ingest had been folding and *nothing* had been reading. Neither reads
  `EntityFlags & 0x02`: vanilla's `isCrouching()` is `hasPose(Pose.CROUCHING)`
  and the shift bit is `isShiftKeyDown()`/`isDiscrete()` — which is what the
  nametag see-through gate reads, correctly, since `shouldShowName` really does
  ask `isDiscrete()`. Two questions, two fields; shift-held in a one-block gap
  is `SWIMMING`, not `CROUCHING`.

  Still unmodelled, because `ArmPose` does not carry the poses that need it:
  `HumanoidModel.java:370`'s extra `-PI/12` on a crouching `TOOT_HORN`/`BRUSH`
  arm.
- **Head yaw now diverges from body yaw.** `Sim::step` (`sim/step.rs`) ports
  `LivingEntity.tickHeadTurn`: a body-yaw candidate
  (`body_yaw_target` — the current body yaw by default, the walking direction
  once the feet move enough, flipped 180° for a backwards-relative walk, or
  the raw look yaw outright mid-swing — `LivingEntity.tick`'s `yBodyRotT`)
  eased 30%/tick toward the body (`tick_head_turn`), then clamped so the head
  never sits more than `Sim::is_blocking`'s `15.0`/`50.0` degrees from it
  (`Player.getMaxHeadRotationRelativeToBody`). The eased, clamped value is
  what feeds `Sim::body_pose` (an `EntityPose`, which does not compute this
  itself — see its own module doc: it stores whatever `body_yaw` it is
  given), and `Sim::body_anim`/`Sim::third_person_body_state` read the
  pose's own `body_yaw`/`head_yaw` back out rather than the raw look yaw.
  Before this, both parameters were fed the same raw look yaw and
  `AnimInput::head_yaw_deg` was hardcoded `0.0` on top of that, so the body
  always equalled the head unconditionally. **Not modelled**: vanilla's
  `attackAnim`-driven snap reads the swing state one tick later than
  `Sim::step`'s own tick ordering allows without splitting `EntityPose::tick`
  in two — see `body_yaw_target`'s doc for the exact staleness, which is the
  same order of magnitude as `EntityPose::start_swing`'s own two-tick lag
  before `attack_anim` turns positive.
- **Remote players have the same gap `entities.rs` owns.** `entities.rs`'s
  `render_anim` feeds `EntityPose`-equivalent logic the network `Rotation`
  yaw directly as the body yaw every frame — there is no per-tick easing
  toward a movement-direction candidate for a tracked entity, only the
  head-to-body clamp (`clamp_head_to_body`, `75°`, mirroring
  `Mob.clampHeadRotationToBody`, not `Player`'s `50°`/`15°`). A correct fix
  is the same shape as this one: compute a `tick_head_turn`-eased body yaw
  per entity per tick from its own previous body yaw and the network yaw,
  before building the `RenderPose`. Out of scope here — filed as a follow-up.
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

No feature flag or env var. In-game, `F5` cycles `Sim::camera_type`, which is
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
