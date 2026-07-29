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
out before this change existed. `first_person_arm_pose`'s own doc comment
(unmodified by this change, because it was already correct) says why:

> `AvatarRenderer.renderHand` calls `arm.resetPose()` and then forces
> `zRot = ±0.1F`, so the arm is drawn from its **authored rest pose** with one
> rotation replaced — never from the third-person `setupAnim` result. That is
> why this is a separate function from `EntityInstance::part_transforms` and
> must stay one: the deferred third-person player body needs the animated
> chain, and sharing a code path would silently give one of the two the
> other's pose.

Concretely: the arm's pose is *authored rest pose, one rotation swapped in* —
it never bends with a walk cycle or looks with the head. The body's pose is
the *animated* `Skeleton::pose` result every mob uses — it must swing its arms
when walking and turn its head to look around. Point one function at the
other's job and either the first-person arm starts animating (wrong — vanilla
never does this) or the third-person body freezes into the arm's static rest
pose (wrong — it would not visibly walk).

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

## Zero pixels — read this before assuming the feature is live

**This is pose-path plumbing with no consumer.** `grep -rn
set_third_person_body_source crates` finds only its own definition in
`gpu.rs` — nothing calls it. Per this repo's own dominant-defect-class rule
(see `CLAUDE.md`, "nothing is done until something on screen changes"), that
makes this an island until two things it deliberately does not build also
exist:

1. **A third-person camera mode.** `ThirdPersonBodySource`'s doc comment
   states the design choice plainly: *"There is no separate 'camera mode' enum
   here on purpose: `f` returning `None` **is** first person, and `Some` **is**
   third person"* — so whatever eventually adds a camera-mode toggle does not
   need to tell this module about it separately; it just needs to *exist* and
   call `set_third_person_body_source` when the mode flips.
2. **A collision-aware pullback.** Vanilla's third-person camera is not simply
   "the same eye position, pulled back": it raycasts from the head to avoid
   clipping through blocks behind the player. Nothing in this client computes
   that yet.

Until both land, `body_state` is always `None`, `entities_with_body` is never
built, and the code added in `81f4cc4` runs zero times in a shipped binary.
Naming that here rather than letting a green test suite read as "done" is the
whole point of this doc — see `CLAUDE.md`'s §"the two rules that matter most".

## How to change it, and the gotchas

- **Never point `ThirdPersonBodyState`'s pose at `first_person_arm_pose`, or
  vice versa.** See "The deliberate split" above — this is the one invariant
  most likely to get "simplified" away by someone who does not know why the
  split exists.
- **Wiring a camera mode**: call `render_state.set_third_person_body_source`
  with a closure that returns `None` in first person and `Some(state)` in
  third person, where `state` is built from the local player's own tick state
  (feet, body yaw, an `AnimInput` built the way `entities.rs` builds one for a
  network entity). That single call is the entire remaining integration; no
  change to `gpu.rs`'s render loop is needed.
- **Real skin data is still unmodelled.** `slim` has no per-player signal to
  read yet (the tab-list player-info packet would carry it, decoded in the
  network layer) — every caller has to pick a value today, and `false`
  (`player_wide`) reproduces the arm's existing default. `player_model_name`
  exists specifically so that the day skin data arrives, selecting the right
  rig is a one-line change at the call site, not new plumbing in
  `lodestone-render`.
- **`bobView`/`bobHurt`/`equipProgress` stay unmodelled**, consistent with the
  first-person arm's own documented gaps (`prepare_first_person_arm`'s doc
  comment). Adding them is a shell-side animation-input concern, not a change
  to this bridge.
- **Held items ride along for free.** Because the body is folded into the same
  `entities` slice `prepare_item_geometry` reads, the local player's own held
  item renders through `merge_held_items` exactly like a mob's does — no
  separate held-item path was or should be added for the third-person case.

## Configuration

None. No feature flag or env var; `ThirdPersonBodySource` unset is the entire
"feature off" state, by construction (see "Zero pixels" above).

## Dependencies

- `lodestone-render::entity` — `player_model_name`, `ThirdPersonBodyState`'s
  target shape (`EntityDraw`), `canonical_model_name`'s corpus-name fallback,
  and the pre-existing `Skeleton::pose` humanoid branch this reuses.
- `lodestone-render::entity_anim` — `AnimInput`, the animation-drive shape
  shared with every other tracked entity.
- `crates/lodestone-shell/src/gpu.rs` — `RenderState::render_inner` (the splice
  point), `RenderState::prepare_entities`/`merge_held_items` (the shared path),
  and `RenderState::prepare_first_person_arm` (the path this mutually
  excludes).

## See also

- [`docs/entity-rendering.md`](./entity-rendering.md) — the general
  resolve/cull/pose/upload pipeline this bridges into.
