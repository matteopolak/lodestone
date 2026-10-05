# Entity postures

## What it is

Code-driven poses for the wolf, the fox and the felines (cat and ocelot), adult and baby rigs: the walk, sitting, a cat lying down and relaxing, a fox sleeping, crouching and pouncing, and the wolf's tail. Wire flags pick the state, the shell ramps the amounts the client ramps, and a posture rig in the renderer turns both into part poses.

## How it works

Chain, from the wire to pixels:

1. **Wire.** The tameable flags byte (index 18, `0x04` tame, `0x01` sitting) decodes to `EntityMetadataUpdate::{tamed, sitting}` for wolf, cat and parrot. The fox flag byte (sitting `1`, crouching `4`, interested `8`, pouncing `16`, sleeping `32`, face-planted `64`), the cat's lying flag and its relaxed flag (index 22) are `MobAppearance::{fox_flags, cat_lying, cat_relaxed}` rows in `packets/metadata/appearance.rs`, anchored to the jar's metadata dump. A feline's crouch is the shared crouching pose and its run is the shared flags byte's sprint bit.
2. **ECS.** `ingest` folds the sitting bit into `lodestone_ecs::entity::Sitting`, beside `Tamed`; the rest ride `Appearance`, `Pose`, `EntityFlags` and `Health`.
3. **Ramps.** `lodestone::entities::PostureRamps` is a track component on wolves, foxes, cats and ocelots, stepped once per tick by `tick_posture_ramps` (`TickSet::Animate`). It keeps the flags and the client-side ramps:
   - fox crouch depth: `+0.2` a tick while crouching, capped at `5`, reset to `0` otherwise;
   - fox interested tilt: closes 40% of the gap to `1` (or `0`) a tick, drawn as `0.11 PI` of head roll;
   - cat lie-down: body `+0.15`/`-0.22`, tail `+0.08`/`-0.13`, relax `+0.1`/`-0.13` a tick, clamped to `0..=1`;
   - wolf tail: angry `1.5393804`, tame `(0.55 - 0.4 (40 - health) / 40) PI`, wild `PI / 5`.

   `PostureRamps::posture(partial_tick)` interpolates them into `lodestone_render::entity_posture::Posture`, which `extract_entity_draws` puts on `AnimInput::posture`. A sleeping fox also binds `fox_sleep` (or `fox_snow_sleep`, and their `_baby` forms through `baby_sheet`).
4. **Rig.** `PostureRig::for_model` attaches to the `wolf`, `fox`, `cat`, `ocelot` corpus models and their `_baby` rigs. Like a keyframe rig it **replaces the family limb animation**: these models assign their own legs, tail and head (the feline's walk has no `1.4` amplitude and swings its tail). A keyframed walk (the baby fox's) runs first, the posture edits land on top, and the head is assigned last, so the baby fox's keyframe rig has no head rule of its own. Fixed offsets are `Edit` tables per state and rig; `Shift` offsets are multiplied by the age scale (`0.5` on a baby rig, which is what makes the baby wolf's sit a lean), `ShiftFixed` ones are not.
5. **Whole-model turns.** A lying cat rolls a quarter turn about the entity's Z and moves `(0.4, 0.15, 0.1)` blocks times the lie amount; a pouncing or face-planted fox pitches by its look. The client applies these in the entity frame, after the body yaw and before the model's mirror and lift, so `PostureRig::root` conjugates them by that mirror and lift and `Skeleton::pose_swelling` composes them under the root.
6. **Hiding.** A sleeping fox's legs collapse through `Skeleton::hidden_parts`, the same path keyframe hides use.

Producer side: the integrated server streams `TamableFlags { tame, sitting }` for every tame wolf, cat, parrot and ocelot, from the sit goal's pose (`an_ordered_sit_streams_the_sitting_bit_and_standing_clears_it`). It does not model a cat lying on a bed or relaxing, or any fox flag, so those postures appear only when joining a server that sends them.

## How to change it

- New posture state: add a field to `Posture`, a ramp or flag in `PostureRamps::step`, and edits in `PostureRig::apply_*`; add a scenario line to `oracle-java/PostureOracle.java`, run `just oracle-posture`, and the gate covers it.
- New species: a `Kind`, a row in `posture_kind`, a `PostureSpecies` row, its bones in `BONES` if new.
- Gotchas: the posture rig assigns, so anything the family animation used to add (head tracking) must be done by the rig; order matters (the fox's head is assigned after its sit pose zeroes it); a `Shift` on a baby rig is halved, so check each offset's scaling against the client; the client's rotation lerp wraps the difference as if it were degrees, harmless at these angles.
- Not ported: the fox's face-planted leg twitch (the client advances it per rendered frame, not per tick), the wolf's begging head tilt and wet shake, a cat lying on a sleeping player's bed (its extra shift needs the player's sleeping state).

## Configuration

None. `just oracle-posture` needs Apple `container` and reads `.cache/mc/<mc-version>/client.jar`.

## Dependencies

- `lodestone-render` (`entity_posture`, `Skeleton`), `lodestone-ecs` (`Sitting`, `Appearance`), the v26-2 metadata decoder (shared by 26.3), the shell's `EntityInterpPlugin`.
- Tests: `crates/lodestone-render/tests/entities/posture_oracle.rs` checks every part of 35 scenarios against `tests/support/posture_jvm.txt`, which `oracle-java/PostureOracle.java` writes by running the real client's pose setup (control: each resting scenario drawn standing must fail); `entity_posture` unit tests check the whole-model turns against hand projections; `crates/lodestone-shell/tests/entities/posture_wire.rs` covers wire to draw, and `posture_pixels.rs` (ignored, needs a GPU) the pixels, measured by silhouette edge.
