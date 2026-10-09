# Entity postures

## What it is

Code-driven poses for the wolf, fox and felines (cat, ocelot), adult and baby: walking, sitting, a cat lying down and relaxing, a fox sleeping, crouching and pouncing, the wolf's tail, and the adult axolotl's blend of swimming, hovering, crawling, lying still and playing dead. Wire flags pick the state, the shell ramps the amounts the client ramps, and a posture rig in the renderer turns both into part poses.

## How it works

1. **Wire.** The tameable flags byte (index 18: `0x04` tame, `0x01` sitting) decodes to `EntityMetadataUpdate::{tamed, sitting}` for wolf, cat and parrot. The fox flags (sitting `1`, crouching `4`, interested `8`, pouncing `16`, sleeping `32`, face-planted `64`) and the cat's lying and relaxed (index 22) flags are `MobAppearance::{fox_flags, cat_lying, cat_relaxed}` rows in `packets/metadata/appearance.rs`. A feline's crouch is the shared crouching pose and its run is the sprint bit.
2. **ECS.** `ingest` folds the sitting bit into `lodestone_ecs::entity::Sitting` beside `Tamed`; the rest ride `Appearance`, `Pose`, `EntityFlags` and `Health`.
3. **Ramps.** `lodestone::entities::PostureRamps` (a component on wolves, foxes, cats, ocelots) is stepped each tick by `tick_posture_ramps` (`TickSet::Animate`):
   - fox crouch depth `+0.2` a tick while crouching, capped at 5, else reset;
   - fox interested tilt closes 40% of the gap to 1 (or 0) per tick, drawn as `0.11 PI` head roll;
   - cat lie-down: body `+0.15`/`-0.22`, tail `+0.08`/`-0.13`, relax `+0.1`/`-0.13` per tick, clamped `0..=1`;
   - wolf tail: angry `1.5393804`, tame `(0.55 - 0.4 (40 - health) / 40) PI`, wild `PI / 5`.

   `PostureRamps::posture(partial_tick)` interpolates into `lodestone_render::entity_posture::Posture` for `AnimInput::posture`. A sleeping fox also binds `fox_sleep` or `fox_snow_sleep` (and `_baby` forms via `baby_sheet`).
4. **Rig.** `PostureRig::for_model` attaches to the `wolf`, `fox`, `cat`, `ocelot` corpus models and `_baby` rigs. Like a keyframe rig it replaces the family limb animation and assigns its own legs, tail and head. A keyframed walk (baby fox) runs first, posture edits land on top, and the head is assigned last. Fixed offsets are `Edit` tables per state and rig; `Shift` offsets scale with age (`0.5` on a baby, which makes the baby wolf's sit a lean), `ShiftFixed` ones do not.
5. **Whole-model turns.** A lying cat rolls a quarter turn about Z and moves `(0.4, 0.15, 0.1)` blocks times the lie amount; a pouncing or face-planted fox pitches by its look. The client applies these after body yaw and before the model's mirror and lift, so `PostureRig::root` conjugates them by that mirror and lift and `Skeleton::pose_swelling` composes them under the root.
6. **Hiding.** A sleeping fox's legs collapse via `Skeleton::hidden_parts`, the same path keyframe hides use.
7. **Adult axolotl.** `Posture::axolotl` carries four eased factors (playing dead, in water, on ground, moving) kept by `KeyframeTimers` ([keyframe-animation](keyframe-animation.md)). The rig yaws the body by the look, then adds five motions, each scaled by the minimum of its factors: swimming (moving, in water; body also pitches), hovering (still, water), crawling (moving, ground), lying still (still, ground) and playing dead. Sways use the client's table sine and cosine (`lodestone_physics::mth`). The right legs then add the left legs' mirrored rotation scaled by `1 - min(on ground, moving)`, so only a crawl moves sides independently. The only wire fact is metadata index 19 (`MobAppearance::axolotl_playing_dead`).

Producer side: the integrated server streams `TamableFlags { tame, sitting }` for tame wolves, cats, parrots and ocelots from the sit goal. It models no cat lying or relaxing and no fox flag, so those postures appear only on servers that send them.

## How to change it

- New state: add a `Posture` field, a ramp or flag in `PostureRamps::step`, and edits in `PostureRig::apply_*`; add a scenario to `oracle-java/PostureOracle.java` and run `just oracle-posture`.
- New species: a `Kind`, a `posture_kind` row, a `PostureSpecies` row, and any new bones in `BONES`.
- Gotchas: the rig assigns, so anything the family animation added (head tracking) must be done by the rig; order matters (the fox head is assigned after the sit pose zeroes it); a `Shift` on a baby rig is halved, so check each offset's scaling against the client; the client's rotation lerp wraps differences as if degrees, harmless at these angles.
- Not ported: the fox's face-planted leg twitch (the client advances it per rendered frame), the wolf's begging tilt and wet shake, and a cat on a sleeping player's bed.

## Configuration

None. `just oracle-posture` needs Apple `container` and `.cache/mc/<mc-version>/client.jar`.

## Dependencies

`lodestone-render` (`entity_posture`, `Skeleton`), `lodestone-ecs`, the v26-2 metadata decoder (shared by 26.3), the shell's `EntityInterpPlugin`. `crates/lodestone-render/tests/entities/posture_oracle.rs` checks every part of 54 scenarios (camel nod, baby and adult axolotl included) against `tests/support/posture_jvm.txt`, written by `PostureOracle.java` running the real client's pose setup, with controls that must fail (resting drawn standing, nods and axolotl states drawn without their state). Also `entity_posture` unit tests (whole-model turns against hand projections), `posture_wire.rs` (wire to draw) and the ignored `posture_pixels.rs` (silhouette edge).
