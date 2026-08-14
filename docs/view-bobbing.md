# View bobbing, the damage tilt, and view lag

Issue #58. The camera's response to walking (`bobView`), to being hit (`bobHurt`),
and the held item's smoothed lag behind the mouse (`xBob`/`yBob`). Walking used to
feel dead because the camera never moved with the stride.

**Status: `bobView` and `bobHurt` both reach pixels, for both the world camera and
the first-person hand, including the death roll and the wire-fed hurt direction.
`xBob`/`yBob` are not started.**

The damage tilt was the long-standing hold, and what it was held on is worth
keeping straight because the record here was wrong about it twice. It was **not**
an unverified formula (the easing, the direction sandwich and the accessibility
option were all ported and unit-tested years of sessions before they drew a pixel)
and it was **not** a missing packet decode (`ClientboundHurtAnimationPacket`'s yaw
has been on `ClientEvent::EntityHurtAnimation` all along). It was a **seam**:
`bobbed_camera` folds the bob into a `Camera`, `Camera` is `position`/`yaw`/`pitch`,
and `bobHurt` is almost entirely a *roll* — so there was nowhere for a 14-degree
tilt to go, and `render_camera` passed a hard `0.0` rather than smear it into pitch.

The fix is not the workspace-wide `Camera` change this document used to prescribe.
Vanilla does not put the bob in its camera either: `renderLevel` does
`projectionMatrix.mul(bobStack.last().pose())`, an **eye-space** post-multiply.
`Camera::view_projection_eye_space` is that seam, `RenderState::set_eye_bob_transform`
installs the matrix once per frame, and `RenderState::world_view_projection` is what
every world-space uniform reads. Zero `Camera` literals changed.

> **The hand has its own copy of this transform, independent of the world's.**
> Vanilla applies `bobHurt`/`bobView` to `GameRenderer.renderItemInHand` a **second time**,
> separate from the copy folded into the world's
> projection (`GameRenderer.renderLevel`) — not something the arm inherits from the bobbed
> world camera. `gpu/first_person.rs`'s `HandBobSource`/`hand_view_proj` are
> that second application; see [How it works](#how-it-works) below. This used
> to be listed under "Divergences deliberately not modelled" as "the
> first-person arm does not bob... fix belongs with `xBob`/`yBob`" — that was
> **wrong**, kept here as a record rather than silently deleted: the arm's bob
> and the view-lag feature are unrelated mechanisms, and the arm did not need
> `xBob`/`yBob` at all, only its own wiring.

> **The settings-menu click bug, and the reason it is filed under the menu and not the camera.**
> "View bobbing does nothing in game" was reported from play with the whole
> render chain verified hop by hop. It was none of the three camera candidates.
> The reporter's `options.json` said `"view_bobbing": false`, written six minutes
> before the report — because `app.rs` translated **every** menu click into
> `hover(row)` + `MenuKey::Enter`, and on the settings screen `Enter` means
> `toggle_view_bobbing()` unconditionally. Clicking the **GUI SCALE** row turned
> View Bobbing off and persisted it. See [Configuration](#configuration).
>
> The bob itself was measured correct on the real tick path at the same time, and
> those numbers are now the gate — see
> [Gotchas when testing this](#gotchas-when-testing-this).

## What it is

Three separate mechanisms that a screenshot makes look like one:

| | what it does | driven by | state |
|---|---|---|---|
| `bobView` | the walking bob: a sway, a dip and a nod, once per footfall | `walkDist` / `bob` | `ViewBob` |
| `bobHurt` | the damage tilt: a **roll** toward the direction the hit came from | `hurtTime` / `hurtDir` | `ViewBob` |
| `xBob`/`yBob` | view lag: the held item and third-person body pose against a *smoothed* head rotation | `getXRot()` / `getYRot()` | none yet |

`bobHurt` is **not** the red screen overlay — that is a separate mechanism, in
`entity_pipeline.rs`, blended at ~30% red per vanilla's `entity.fsh`'s `overlayColor` mix. The two
fire together and are otherwise unrelated.

## How it works

The tick-advanced state lives in
[`crates/lodestone-shell/src/camera_rig.rs`](../crates/lodestone-shell/src/camera_rig.rs)
alongside `EyeHeightSmoother`, which has the same shape: per-tick state that
cannot be a pure function of the current `PlayerState`. From one `BobFrame`
read per frame, **two independent consumers** apply the identical transform —
mirroring vanilla's own two call sites for `bobHurt`/`bobView`
(`GameRenderer.renderLevel` for the world, `GameRenderer.renderItemInHand` for the hand):

```
Sim::step, once per 20 Hz tick
  ViewBob::tick(moved_horizontal, speed_horizontal, on_ground, dead, swimming)
     -> walk_dist / walk_dist_o      the stride PHASE
     -> bob / bob_o                  the AMPLITUDE
  (ViewBob::hurt(yaw) on a damage report -> hurt_time / hurt_dir)

Sim::render_camera, once per frame
  Sim::bob_frame()  -> ViewBob::frame(interp_alpha) -> BobFrame
   |
   +-> bobbed_camera(camera, frame, damage_tilt_strength) -> Camera   [the world]
   |      -> Camera::view_projection, which gpu.rs already reads
   |
   +-> RenderState::set_hand_bob_source(|| frame)                    [the hand]
          -> HandBobSource::value() -> hand_view_proj(aspect, frame, tilt)
          -> gpu/first_person.rs's write_hand_camera, both hand-pass uniforms
```

The hand's half needs **no fold at all** — `hand_view_proj` multiplies
`BobFrame::eye_transform`'s raw matrix straight into `hand_projection`, because
the hand pass carries no view matrix in the first place (`hand_projection`'s
own doc: the view rotation already cancels). That is *why* the hand can afford
`bobHurt`'s roll term where the world cannot — see
[The one blocker](#the-one-blocker-and-what-it-costs), which is a **world-only**
limitation now, not a shared one.

### The constants, and where each came from

All from `.cache/mc/26.2/client-src`, read rather than remembered:

| constant | source |
|---|---|
| `walkDist += length(dx, dz) * 0.6` | `LocalPlayer.move` |
| `bob += (min(0.1, speed) - bob) * 0.4`, target `0` unless `onGround && !dead && !swimming` | `AbstractClientPlayer.updateBob` + `ClientAvatarState.updateBob` |
| `translate(sin(bd·π)·bob·0.5, -abs(cos(bd·π)·bob), 0)` | `GameRenderer.bobView` |
| `rotate Z by sin(bd·π)·bob·3.0` | `GameRenderer.bobView` |
| `rotate X by abs(cos(bd·π − 0.2)·bob)·5.0` | `GameRenderer.bobView` |
| `hurtDuration = 10`, `hurtTime = hurtDuration` | `LivingEntity.animateHurt`, `LivingEntity.handleDamageEvent` |
| `sin(t⁴·π) · 14 · damageTiltStrength`, swung by `Ry(∓hurtDir)` | `GameRenderer.bobHurt` |
| `xBob += (getXRot() − xBob) · 0.5` | `LocalPlayer.applyInput` |
| `rotate X/Y by (getViewXRot(t) − xBob) · 0.1` | `ItemInHandRenderer.submitHandsWithItems` |
| View Bobbing default **on** | `Options.bobView` field |
| `damageTiltStrength` default `1.0` | `Options.damageTiltStrength` field |

### Four details that are easy to get subtly wrong

* **`bd` is not a lerp.** `getBackwardsInterpolatedWalkDistance` is
  `-(walkDist + (walkDist − walkDistO) · partialTicks)` — it *extrapolates* forward
  and negates. `getInterpolatedWalkDistance` is the lerp and nothing in `bobView`
  uses it. Reading the lerp is a half-tick phase error that looks fine.
* **The `− 0.2` in the nod is radians, inside the cosine.** `cos(bd·π − 0.2)`, not
  `cos((bd − 0.2)·π)`. The wrong reading is a 36° phase error that still looks like
  a nod. `the_nods_phase_offset_is_zero_point_two_radians_not_zero_point_two_pi`
  pins it against the hand-evaluated value.
* **The dip is rectified, the sway is not.** `-abs(cos(...))` means the eye drops
  once per footfall. Dropping the `abs` halves the apparent cadence and is
  invisible in a still frame.
* **The hurt tilt's quartic matters.** `sin(t⁴·π)` stays near zero then spikes, so
  the tilt is a jolt. `sin(t·π)` would be a smooth half-second lean and would read
  as nausea.

### The bob goes on `render_camera`, never `camera`

`Sim::camera` is also the block-targeting ray origin *and* the audio listener.
Vanilla bobs neither: `GameRenderer.renderLevel` post-multiplies the bob onto the
**projection** matrix (`:539`), so `Camera`'s own position and rotation — what
`getPickRay` and the listener read — never see it. Bobbing `Sim::camera` would be
a gameplay bug, not a visual one.

Because the bob lands between the projection and the view, it acts on **eye-space**
coordinates, and our eye space is identical to vanilla's (`+X` right, `+Y` up,
forward `-Z`: `Camera.FORWARDS` is `(0,0,-1)` and `glam::camera::rh::view::look_to_mat4`
gives the same basis). So every constant transcribes with **no sign flip**. The
`[0,1]`-versus-reversed-Z depth difference `CLAUDE.md` warns about lives entirely
inside the projection matrix, which sits to the *left* of the bob in `P · B · V`.

## The `Camera` fold, what it drops, and the seam that made that stop mattering

`Camera` has `position`, `yaw` and `pitch` — **two angles where a bob matrix has
three.** `bobbed_camera` recovers position and forward mechanically from `B · V`
(no sign is chosen by hand, deliberately: `CLAUDE.md` records shipping an
inside-out block because a polarity was asserted rather than derived) and
therefore **drops roll**.

*(This paragraph used to say `view_matrix` hardcodes `Vec3::Y` as up and called the
mismatch "three degrees of freedom where a bob matrix has four". Both were wrong:
the basis has derived `up` since `d17c731c`, and the counts are two-against-three.
The conclusion — that the fold cannot carry roll — is unchanged, which is exactly
why the wrong reasoning survived so long.)*

| bob term | magnitude | carried? |
|---|---|---|
| `bobView` translate | ≤ 0.05 blocks | yes, exactly |
| `bobView` nod (`Axis.XP`) | ≤ 0.49° | yes, exactly |
| `bobView` roll (`Axis.ZP`) | ≤ 0.3° | **no** — measured at **2.52 px** worst case on a 1920×1080 frame |
| `bobHurt` tilt | ≤ 14° | only the component landing on the nod axis |

So the walk bob is worth landing without roll (0.3° is below noticing, and the
number is pinned by `the_dropped_roll_is_the_only_disagreement_and_it_is_small_for_the_walk_bob`)
and the damage tilt is not: a frontal hit is *pure* roll, so pushing `bobHurt`
through this fold would produce a visibly wrong tilt rather than a slightly
imprecise one. `Sim::render_camera` therefore **still** passes
`damage_tilt_strength = 0.0` — and that is now a **routing** fact, not a hold.

The hurt half takes the other route instead:
`Sim::damage_tilt_eye_transform` → `RenderState::set_eye_bob_transform` →
`Camera::view_projection_eye_space`, i.e. `P · bobHurt · V`. Both halves are live;
what this fold still drops is `bobView`'s own `0.3°` roll and nothing else.

**Every world-space uniform must read `RenderState::world_view_projection`.** The
entity pass, the block-entity pass and the world glint each write their own group 0,
and a pass that skipped the tilt would slide against the terrain around it while the
camera leaned — a far more visible defect than no tilt at all. That is why it is a
method rather than four call sites composing the same product. `render_inner` is the
one site that composes further, adding the nausea/portal spin **to the right of**
the bob, matching `renderLevel`'s own order.

**This table does not apply to the hand.** `gpu/first_person.rs`'s
`hand_view_proj` multiplies `BobFrame::eye_transform`'s matrix straight into
`hand_projection`, with no `Camera` decomposition step at all, so it carries
**every** term exactly — roll included. The hand is therefore driven by the real
`Options::damage_tilt_strength`, pushed down through
`RenderState::set_damage_tilt_strength`, and it applies `bobHurt` a *second* time
independently of the world's copy exactly as vanilla's `renderItemInHand` does.

## How to change it

### The damage tilt — landed, and the two prescriptions this section used to carry

Both were wrong, and both are kept as corrections rather than deleted.

**"`Camera` needs to carry roll, and that is a workspace-wide change."** The count
was understated (117 exhaustive `Camera { .. }` literals across 54 files, not 48
across ~40) and the premise was unnecessary. Vanilla does not carry the bob in its
camera; it post-multiplies the projection in eye space. So does this now, through
`Camera::view_projection_eye_space` — **zero literals changed**. When a prescription
is "purely broad, nothing about it is hard", that is worth reading as a hint that a
narrower seam exists.

**"The hand is blocked on `Sim::bob_frame`'s all-or-nothing gating."** True when
written and already fixed by the time it was read: `bob_frame` had stopped returning
`BobFrame::default()` whole-cloth and now zeroes only `walk_phase`/`bob`, passing the
hurt half through — which is vanilla's split. The constant it named,
`HAND_HURT_TILT_STRENGTH`, is gone; `NO_DAMAGE_TILT` survives in its place as the
gates' zero anchor and carries the record.

What the wiring is, end to end:

| hop | where |
|---|---|
| wire yaw → shell stream | `net.rs`'s `forward` → `NetUpdate::HurtAnimation` |
| filtered to the local player | `net_apply`'s arm, against `server_entity_id()` |
| countdown + direction | `Sim::on_local_player_hurt` → `ViewBob::hurt` |
| death counter | `ViewBob::tick`'s `dead` flag |
| interpolated frame | `Sim::bob_frame` |
| the matrix | `BobFrame::hurt_transform` |
| installed per frame | `RenderState::set_eye_bob_transform` |
| into every world uniform | `RenderState::world_view_projection` |
| the hand's second copy | `RenderState::set_damage_tilt_strength` → `hand_view_proj` |

The **ingest** path is a different consumer of the same event and neither subsumes
the other: `lodestone_ecs::ingest` folds `EntityHurtAnimation` into a per-entity
`HurtTime` for the red overlay, and it **discards the yaw** — which is the entire
direction half of the tilt. Per-entity state goes to `ingest`; this is a
local-player scalar on the shell's own stream, like `EffectApplied`.

**Outstanding, and it is not a code gap:** `lodestone_model::event::route` still
lists `EntityHurtAnimation` as `INGEST` only. `forward`'s consistency `debug_assert`
is one-directional (`must_forward ⇒ has an arm`), so the wiring works and nothing
fails — but the routing table is now understating the event's consumers, and that
table is the thing a reader consults to answer "does anything consume event X". It
needs `Route { ingest: true, shell: true, ..NOWHERE }`.

### To land `xBob`/`yBob`

This is **not a camera change, and not the same fix as the arm's bob above** —
they were conflated in this tracker's history for a while (its own body used
to say "the arm does not bob... fix belongs with `xBob`/`yBob`"; it does not).
`xBob`/`yBob` poses the held item and third-person body against a *smoothed
follow* of the head rotation — a lag, not a bob — in
`lodestone-shell/src/gpu/first_person.rs` and `lodestone-render/src/entity.rs`.
Add the `x_bob`/`y_bob` pair (same current/previous shape,
`+= (target − current) · 0.5` per tick) to `ViewBob` and prefix
`Rx((viewXRot − xBob)·0.1°)`, `Ry((viewYRot − yBob)·0.1°)` onto the hand pose.
`first_person_arm_chain`'s own doc comment (`crates/lodestone-render/src/entity.rs`) already names the mechanism.

### Divergences deliberately not modelled

* **`bobView`'s roll, for the world camera only** — with the 2.52 px measurement.
  The hand carries it exactly (see above), and `bobHurt`'s roll now reaches the
  world through the eye-space seam rather than through this fold.
* **The nearest-*remote*-player case is not modelled** for the hurt direction: the
  yaw comes off the wire, so this one is exact — the gap is the enchanting table's,
  not the camera's.
* **Third person still bobs, and that is correct for 26.2.** This tracker's body
  originally said vanilla disables bobbing in third person. Re-read against
  `.cache/mc/26.2/client-src`: `GameRenderer.renderLevel` has no camera-type check
  and `bobView` itself only tests `isPlayer`. Older versions did suppress it; 26.2
  does not.
* **`damageTiltStrength` has a config key and a settings-screen row, but the row
  does not yet write the key.** `Options::damage_tilt_strength` is real, persisted,
  clamped to `0.0..=1.0` and pushed down every frame; the Accessibility slider still
  renders from `UNIT_DOUBLE_DEFAULTS`' frozen `1.0` like its neighbours. So the
  option is settable by editing `options.json` and not yet by dragging the slider.

## Configuration

`Options::view_bobbing` in
[`config.rs`](../crates/lodestone-shell/src/config.rs), persisted to
`options.json`, **default on** (vanilla's `options.viewBobbing`,
`Options.bobView` field).

**Where the row lives moved with the settings-tree rework.** It is now on vanilla's own screen for it:
`Options...` → `Accessibility Settings...` → `View Bobbing`, paired with
`Notification Time` — **not** on Video, which is the intuitive and wrong answer.
Enter on that row toggles it, a click on it toggles it, and it is one of only
**two** live options in a tree of 135 (see
[`settings-screen.md`](./settings-screen.md)).

**A click used to toggle it from the *other* row, which is the settings-menu click bug above.** The
settings screen had no row cursor on purpose — each control owned a key
(`key_settings`: Up/Down the scale, Enter the toggle), so `MenuNav::hover` had no
`Screen::Settings` arm at all. `app.rs`'s click handler translated a click into
`hover(row)` + `MenuKey::Enter`, which is correct on the screens that *do* have a
cursor and, here, meant every click was "toggle View Bobbing" no matter which row
it landed on. The natural thing to click — GUI SCALE, row 0, the row `render.rs`
marked `selected` — silently turned the option off and wrote it to disk, and the
render chain underneath was working the whole time.

The settings-tree rework removed the cause: the screen has a real cursor and every row resolves to its
own control, so there is no shared per-screen meaning of `Enter` left to
mis-apply. The gates were re-pointed rather than deleted —
`nav::clicking_a_settings_row_acts_on_that_row_and_no_other` is still the negative
assertion with an active row as its control, and
`options::the_settings_rows_are_in_the_order_click_assumes` sweeps every page at
every scroll position, because a row index is an index into a vector built in a
different file and reordering it would silently rebind the mouse again.

**A persisted `false` cannot be told from a deliberate choice**, so nothing
auto-heals it: anyone who hit this has to turn the option back on once (or delete
the key from `options.json`).

The default direction is the opposite of the deleted `unlock_framerate` debug knob
and the asymmetry is deliberate: a malformed value must read as **on**, because
degrading a shipped option to off is a silent feature loss. Only written when
turned off, so an untouched install has no key.

**Vanilla gates only `bobView` on this flag — `bobHurt` is applied
unconditionally, and that split *is* reproduced now.** This paragraph has been
wrong in both directions and both readings are kept, because the second is the more
instructive: it first claimed the split was reproduced when it was not, was corrected
to say it was not, and then **stayed** at "not" after the fix landed. `Sim::bob_frame`
zeroes only `walk_phase`/`bob` and passes `hurt`/`hurt_dir_degrees`/`death_time`
through untouched; `local_player_hurt_reaches_the_bob_frame_and_survives_view_bobbing_off`
asserts exactly that. A player who turns View Bobbing off still gets the damage tilt,
which is what vanilla does.

The damage tilt has its own separate switch — `Options::damage_tilt_strength`, the
accessibility option — and `0.0` there really does disable it
(`a_zero_damage_tilt_strength_disables_the_tilt_but_not_the_death_roll`). It does
**not** disable the death roll, which vanilla applies unscaled.

## Gotchas when testing this

* **A bob fixture that never accumulates `walkDist` measures nothing.** The
  offline world is real generated terrain, the player spawns on a slope, and
  walking north walls them out after ~0.2 blocks — a gate run against that reads
  `walk_phase: -0.0, bob: 0.0` and asserts nothing. `sim::tests::walking_accumulates_a_real_bob_that_only_the_render_camera_sees`
  flattens a corridor with `set_block_world` first, and asserts the player actually
  travelled before looking at the bob.
* **"The frame changed" is not enough.** It passes on the wrong amplitude, the
  wrong phase and the wrong axis. `tests/view_bob_pixels.rs` predicts the pixel
  displacement from vanilla's constants and asserts direction *and* magnitude on
  both axes.
* **Nothing gated the *inputs* until the settings-menu click bug was found, and that is the hole to keep shut.**
  Every gate above supplies its own `BobFrame`, so all of them prove the
  arithmetic and none of them can see `Sim` feeding it an unrealistic
  `moved`/`speed` — `CLAUDE.md`'s *world* species, invisible in the test source.
  `sim::tests::the_walk_bob_reaches_the_projection_at_vanillas_own_magnitude_and_axis`
  closes it: it drives a real walking `Sim` and pins the inputs first
  (0.2159 blocks/tick, vanilla's own walk speed, measured from the position and
  not read back out of the bob), then the amplitude (`0.1000`, saturating
  `min(0.1, speed)`), then the phase advance (exactly `0.6 x moved`).

  Two probes on `cam.forward()`, sampled at 60 fps, then separate the axes —
  measured against the value predicted from `bobView`'s constants:

  | probe | measured | predicted | what it isolates |
  |---|---|---|---|
  | infinity, dy | `-6.711` px | `-6.730` | the **nod** alone: translation cannot move a point at infinity, so a nod-free bob reads exactly `0.0` here |
  | infinity, dx | `±0.01` px | `0` | that the bob does not **yaw** — the sway is a translation, the roll is dropped |
  | 3 blocks, dx | `-12.847 .. +12.853` px | `±12.853` | the **sway**, and that it is a full sine (both ways) not a rectified one |
  | 3 blocks, dy | `+19.158` px | `+18.977` | the **dip**, net of the nod opposing it |

  The far probe's direction comes from `cam.forward()` and not from `-Z` on
  purpose: the offline spawn pitch is **10°**, and a probe placed down `-Z` sits
  that far above the view centre, where a pitch change of `t` moves it by
  `sec²(α)/tan(fov/2)`. That probe read `6.93` px against the on-axis `6.73` — a
  3% error pointing the wrong way, i.e. looking like a bob that is slightly too
  strong. Derive the geometry from the camera; do not restate the constant.
* **Four controls, each failing a different subset.** Run them; do not describe
  them. A single control here is not enough — `CLAUDE.md`'s "check you neutered
  *enough*" applies directly, because the bob has four terms and three of them
  survive any one neuter:

  | control | what fails |
  |---|---|
  | `render_camera` takes `BobFrame::default()` | both `Sim` gates; every box collapses to `0.000` |
  | `view_nod_degrees` returns `0.0` | the infinity probe only; sway still `±12.85`, dip still `25.69` |
  | the sway term of `view_translation` zeroed | the near-`dx` assertion only; nod still `-6.71`, dip still `19.16` |
  | the dip term of `view_translation` zeroed | the near-`dy` assertion only; nod and sway both intact |
* **A bounding-box centre is not a projected centroid.** Under a camera pitch
  change the near and far faces of a 3-D box move differently, so the silhouette's
  extremes do not shift like its centre. Measured: the chest's bbox centre moves
  **8.50 px** where the centroid moves **6.53**. That 2 px bias is close enough to
  the 8.31 px a *nod-free* bob would give that the pixel gate cannot separate them
  — so the discriminating assertion lives in
  `camera_rig::tests::the_nod_reaches_the_projection_and_is_worth_one_point_eight_pixels`,
  on the projected point, pinned to 0.01 px.
* **The island control fires exactly one test.** Replacing `Sim::bob_frame()` with
  `BobFrame::default()` in `render_camera` leaves all 25 `camera_rig` tests green
  and the GPU pixel gate green (it calls `bobbed_camera` directly). Only the `Sim`
  gate above catches it. If you delete one test in this feature, do not delete that
  one.
* **`view_bobbing` being toggleable is exactly why the hand needed two gates, not
  one.** `tests/view_bob_pixels.rs`'s `the_arm_does_not_move_when_no_hand_bob_source_is_installed`
  asserts the unset/off case is **bit-identical** (`assert_eq!(moved, 0)`, not a
  tolerance), and `the_arm_moves_when_a_hand_bob_source_is_installed` asserts the
  on case moves a substantial, direction-checked number of pixels. Either alone
  would leave the other state unverified — an arm that always bobs regardless of
  the option would pass a "does it bob" test and fail no gate that only checks
  the on case.
* **A GPU pixel gate cannot re-derive the hand's exact magnitude from a real
  mesh's silhouette** — the same bounding-box-vs-centroid gap the chest's own
  `+8.50` vs `+6.53` records, and the hand sits close enough to the eye that the
  effect is proportionally larger. The *exact* prediction is pinned without a GPU
  in `gpu::first_person::tests::the_dip_moves_the_test_point_by_the_hand_derived_pixel_offset`
  (`+27.10 px` for a synthetic point at a plausible arm depth, against two
  rejected hypotheses at `+28.56`/`+30.03`) and its sway sibling
  (`-14.28 px`/`-0.30 px`); the pixel gate's job is only to prove the transform is
  *called* for a really-rendered arm, not to re-pin its magnitude.

## Dependencies

* `lodestone_render::Camera` — the fold's target, and the source of the blocker.
* `lodestone_physics::PlayerState` — `on_ground`, `velocity`, `pose` feed the
  amplitude gate; the position delta feeds the phase.
* `lodestone_ecs::entity::HurtTime` and `ClientEvent::EntityHurtAnimation` — the
  hurt half's inputs, ingested and ticking, not yet read here.
* `glam` — `Mat4::from_rotation_{x,y,z}` are right-handed about `+axis`, matching
  JOML's `Axis.{X,Y,Z}P.rotationDegrees`. Verified by agreement with `P · B · V`
  rather than assumed.
