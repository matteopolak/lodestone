# View bobbing, the damage tilt, and view lag

Issue #58. The camera's response to walking (`bobView`), to being hit (`bobHurt`),
and the held item's smoothed lag behind the mouse (`xBob`/`yBob`). Walking used to
feel dead because the camera never moved with the stride.

**Status: `bobView` is implemented and reaches pixels. `bobHurt` is implemented and
deliberately not wired. `xBob`/`yBob` are not started.** The reason for each is
below — none of them is an oversight, and the blocker for two of the three is the
same one thing.

> **Issue #391, and the reason it is filed under the menu and not the camera.**
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

`bobHurt` is **not** the red screen overlay — that is #371, in
`entity_pipeline.rs`, blended at ~30% red per vanilla's `entity.fsh:57`. The two
fire together and are otherwise unrelated.

## How it works

Everything lives in [`crates/lodestone-shell/src/camera_rig.rs`](../crates/lodestone-shell/src/camera_rig.rs)
alongside `EyeHeightSmoother`, which has the same shape: per-tick state that
cannot be a pure function of the current `PlayerState`.

```
Sim::step, once per 20 Hz tick
  ViewBob::tick(moved_horizontal, speed_horizontal, on_ground, dead, swimming)
     -> walk_dist / walk_dist_o      the stride PHASE
     -> bob / bob_o                  the AMPLITUDE
  (ViewBob::hurt(yaw) on a damage report -> hurt_time / hurt_dir)

Sim::render_camera, once per frame
  Sim::bob_frame()  -> ViewBob::frame(interp_alpha) -> BobFrame
  bobbed_camera(camera, frame, damage_tilt_strength) -> Camera
     -> Camera::view_projection, which gpu.rs already reads
```

### The constants, and where each came from

All from `.cache/mc/26.2/client-src`, read rather than remembered:

| constant | source |
|---|---|
| `walkDist += length(dx, dz) * 0.6` | `LocalPlayer.move`, `LocalPlayer.java:989` |
| `bob += (min(0.1, speed) - bob) * 0.4`, target `0` unless `onGround && !dead && !swimming` | `AbstractClientPlayer.updateBob` + `ClientAvatarState.updateBob` |
| `translate(sin(bd·π)·bob·0.5, -abs(cos(bd·π)·bob), 0)` | `GameRenderer.bobView`, `GameRenderer.java:323-327` |
| `rotate Z by sin(bd·π)·bob·3.0` | `:328` |
| `rotate X by abs(cos(bd·π − 0.2)·bob)·5.0` | `:329` |
| `hurtDuration = 10`, `hurtTime = hurtDuration` | `LivingEntity.java:1873-1876`, `:2044-2049` |
| `sin(t⁴·π) · 14 · damageTiltStrength`, swung by `Ry(∓hurtDir)` | `GameRenderer.bobHurt`, `:297-317` |
| `xBob += (getXRot() − xBob) · 0.5` | `LocalPlayer.applyInput`, `LocalPlayer.java:694-697` |
| `rotate X/Y by (getViewXRot(t) − xBob) · 0.1` | `ItemInHandRenderer.java:354-357` |
| View Bobbing default **on** | `Options.java:600` |
| `damageTiltStrength` default `1.0` | `Options.java:876-883` |

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

## The one blocker, and what it costs

`Camera` has `position`, `yaw` and `pitch`, and `view_matrix` hardcodes `Vec3::Y`
as up — **three degrees of freedom where a bob matrix has four.** `bobbed_camera`
recovers position and forward mechanically from `B · V` (no sign is chosen by
hand, deliberately: `CLAUDE.md` records shipping an inside-out block because a
polarity was asserted rather than derived) and therefore **drops roll**.

| bob term | magnitude | carried? |
|---|---|---|
| `bobView` translate | ≤ 0.05 blocks | yes, exactly |
| `bobView` nod (`Axis.XP`) | ≤ 0.49° | yes, exactly |
| `bobView` roll (`Axis.ZP`) | ≤ 0.3° | **no** — measured at **2.52 px** worst case on a 1920×1080 frame |
| `bobHurt` tilt | ≤ 14° | only the component landing on the nod axis |

So the walk bob is worth landing without roll (0.3° is below noticing, and the
number is pinned by `the_dropped_roll_is_the_only_disagreement_and_it_is_small_for_the_walk_bob`)
and the damage tilt is not: a frontal hit is *pure* roll, so wiring `bobHurt`
through this fold would produce a visibly wrong tilt rather than a slightly
imprecise one. `Sim::render_camera` therefore passes `damage_tilt_strength = 0.0`.

## How to change it

### To land `bobHurt` (and the walk bob's last 0.3°)

`Camera` needs to carry roll. Either a `roll: f32` field, or a `Mat4` eye-space
hook consulted by `view_projection`. **Both are workspace-wide changes**: there are
48 full `Camera { .. }` struct literals across ~40 files, six of them in
`lodestone-shell/src/gpu.rs`'s test module and one in
`lodestone-render/src/entity.rs`. Nothing about the change is hard; it is purely
broad, which is why #58 stopped short of it rather than doing it under contention.

Once `Camera` can roll, `BobFrame::eye_transform` is already the whole answer — it
is a literal transcription of `bobHurt` *then* `bobView` in vanilla's own pose-stack
order, tested against `P · B · V`. Drive `ViewBob::hurt(yaw)` from the local
player's `EntityHurtAnimation` (the yaw is already decoded and on the event; see
`lodestone-model/src/event.rs`) and `HurtTime` (already ingested and ticking), and
pass the real `damage_tilt_strength` instead of `0.0`.

### To land `xBob`/`yBob`

This is **not a camera change** — it poses the *held item* and the third-person
body, in `lodestone-shell/src/gpu/first_person.rs` and
`lodestone-render/src/entity.rs`. Add the `x_bob`/`y_bob` pair (same
current/previous shape, `+= (target − current) · 0.5` per tick) to `ViewBob` and
prefix `Rx((viewXRot − xBob)·0.1°)`, `Ry((viewYRot − yBob)·0.1°)` onto the hand
pose. `entity.rs:1702-1703` already names the mechanism.

### Divergences deliberately not modelled

* **Roll**, as above — with the 2.52 px measurement.
* **The first-person arm does not bob.** Vanilla prefixes `bobHurt` and `bobView`
  onto `renderItemInHand`'s pose stack (`GameRenderer.java:344-346`); here the hand
  pass uses its own projection, so the arm is static. Convenient for the pixel
  gate (the arm cancels out of every diff) and wrong. Fix belongs with `xBob`/`yBob`.
* **Third person still bobs, and that is correct for 26.2.** #58's body says
  vanilla disables bobbing in third person. Re-read against
  `.cache/mc/26.2/client-src`: `renderLevel` (`:534-536`) has no camera-type check
  and `bobView` itself only tests `isPlayer`. Older versions did suppress it; 26.2
  does not.
* **`damageTiltStrength` has no UI.** Vanilla puts it on the Accessibility screen.
  Pointless to surface while the tilt itself is unwired.

## Configuration

`Options::view_bobbing` in
[`config.rs`](../crates/lodestone-shell/src/config.rs), persisted to
`options.json`, **default on** (vanilla's `options.viewBobbing`,
`Options.java:600`).

**Where the row lives moved with #55.** It is now on vanilla's own screen for it:
`Options...` → `Accessibility Settings...` → `View Bobbing`, paired with
`Notification Time` — **not** on Video, which is the intuitive and wrong answer.
Enter on that row toggles it, a click on it toggles it, and it is one of only
**two** live options in a tree of 135 (see
[`settings-screen.md`](./settings-screen.md)).

**A click used to toggle it from the *other* row, which is issue #391.** The
settings screen had no row cursor on purpose — each control owned a key
(`key_settings`: Up/Down the scale, Enter the toggle), so `MenuNav::hover` had no
`Screen::Settings` arm at all. `app.rs`'s click handler translated a click into
`hover(row)` + `MenuKey::Enter`, which is correct on the screens that *do* have a
cursor and, here, meant every click was "toggle View Bobbing" no matter which row
it landed on. The natural thing to click — GUI SCALE, row 0, the row `render.rs`
marked `selected` — silently turned the option off and wrote it to disk, and the
render chain underneath was working the whole time.

#55 removed the cause: the screen has a real cursor and every row resolves to its
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

Note vanilla gates only `bobView` on this flag — `bobHurt` is applied
unconditionally (`GameRenderer.java:534-536`). That split is reproduced, so
turning bobbing off will still leave the damage tilt once it is wired.

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
* **Nothing gated the *inputs* until #391, and that is the hole to keep shut.**
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

## Dependencies

* `lodestone_render::Camera` — the fold's target, and the source of the blocker.
* `lodestone_physics::PlayerState` — `on_ground`, `velocity`, `pose` feed the
  amplitude gate; the position delta feeds the phase.
* `lodestone_ecs::entity::HurtTime` and `ClientEvent::EntityHurtAnimation` — the
  hurt half's inputs, ingested and ticking, not yet read here.
* `glam` — `Mat4::from_rotation_{x,y,z}` are right-handed about `+axis`, matching
  JOML's `Axis.{X,Y,Z}P.rotationDegrees`. Verified by agreement with `P · B · V`
  rather than assumed.
