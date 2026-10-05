//! Pixel gates for wolf, fox, cat and axolotl postures through the real
//! [`RenderState::render`] path, measured by location: where the silhouette's edge
//! moves to, against a projection worked out by hand from the client's offsets.
//!
//! ```text
//! cargo test -p lodestone-shell --test entities posture_pixels -- --ignored --nocapture
//! ```
//!
//! The scene (`gear_pixels::Scene`) looks along +Z from `(0, 0.6, 0)` with a 60
//! degree vertical field of view on a 320x240 target, so a point at height `y` and
//! depth `d` lands on row `120 + 207.8 (0.6 - y) / d`. The mob stands at `z = 3`,
//! side on. Model units are sixteenths of a block, Y down, with model `y = 24` on
//! the ground.

use super::gear_pixels::{Scene, bbox, changed, draw, silhouette};
use lodestone::entities::EntityDraw;
use lodestone_render::AnimInput;
use lodestone_render::entity_posture::Posture;

fn with(model: &str, posture: Posture) -> EntityDraw {
    EntityDraw { anim: AnimInput { posture, ..AnimInput::REST }, ..draw(model, vec![]) }
}

/// Silhouette boxes `(x0, y0, x1, y1)` standing and in `posture`, plus the changed
/// pixel count; the standing frame is drawn twice as the control.
fn compare(model: &str, posture: Posture) -> ((u32, u32, u32, u32), (u32, u32, u32, u32), usize) {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let standing = scene.shoot(&[with(model, Posture::NONE)]);
    let again = scene.shoot(&[with(model, Posture::NONE)]);
    let posed = scene.shoot(&[with(model, posture)]);
    assert_eq!(standing, again, "control: the same input twice must match exactly");
    let stand_box = bbox(&silhouette(&empty, &standing)).expect("the standing mob draws");
    let posed_box = bbox(&silhouette(&empty, &posed)).expect("the posed mob draws");
    let diff = changed(&standing, &posed).len();
    eprintln!("{model}: standing {stand_box:?}, posed {posed_box:?}, {diff} px changed");
    (stand_box, posed_box, diff)
}

/// Row of a point at world height `y` and depth `d` (see the module doc).
fn row(y: f32, d: f32) -> f32 {
    120.0 + 207.8 * (0.6 - y) / d
}

/// A sitting wolf drops its haunches below its standing feet: the tail, hanging at
/// its wild `PI / 5` from a pivot moved to model `y = 21`, reaches
/// `21 + 8 cos 36° = 27.47`, which is 3.47 units (0.217 blocks) under the ground.
/// Standing, the lowest point is the feet on the ground. Its ears (model `y = 8.5`)
/// do not move, so the top row stays put.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_sitting_wolf_sinks_its_haunches_and_keeps_its_head() {
    let (stand, sit, diff) = compare("wolf", Posture { sitting: true, ..Posture::NONE });
    assert!(diff > 200, "sitting repaints a real share of the wolf: {diff}");
    let feet = row(0.0, 3.0);
    let tail = row(0.001 - 3.47 / 16.0, 3.0);
    assert!((stand.3 as f32 - feet).abs() <= 3.0, "standing bottom {} vs feet row {feet}", stand.3);
    assert!((sit.3 as f32 - tail).abs() <= 3.0, "sitting bottom {} vs tail row {tail}", sit.3);
    assert!(stand.1.abs_diff(sit.1) <= 1, "the head does not move: {} vs {}", stand.1, sit.1);
}

/// A fully lain-down cat is rolled onto its side about the body axis (a quarter turn
/// about the entity's Z, then moved by `(0.4, 0.15, 0.1)` blocks), so it ends up much
/// lower and no longer than it stood: its top drops by at least a third of its
/// standing height.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_lying_cat_rolls_onto_its_side() {
    let (stand, lie, diff) =
        compare("cat", Posture { lie_down: 1.0, lie_down_tail: 1.0, ..Posture::NONE });
    assert!(diff > 200, "lying repaints a real share of the cat: {diff}");
    let stand_height = stand.3 - stand.1;
    let lie_height = lie.3 - lie.1;
    eprintln!("cat height standing {stand_height}, lying {lie_height}");
    assert!(lie.1 > stand.1 + stand_height / 3, "the top drops: {} -> {}", stand.1, lie.1);
}

/// A sleeping fox hides its legs and lies on its side, five units lower: the legs
/// (model `y` 18 to 24) are gone, so nothing is left at the feet row, and the body,
/// rolled about its own axis, now spans the bottom.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_sleeping_fox_tucks_away_its_legs() {
    let (stand, sleep, diff) = compare("fox", Posture { sleeping: true, ..Posture::NONE });
    assert!(diff > 200, "sleeping repaints a real share of the fox: {diff}");
    let feet = row(0.0, 3.0);
    assert!((stand.3 as f32 - feet).abs() <= 3.0, "standing bottom {} vs feet row {feet}", stand.3);
    assert!(sleep.1 > stand.1, "the sleeping fox is lower: top {} -> {}", stand.1, sleep.1);
}

/// A swimming adult axolotl pitches its whole body with its look, so looking down
/// 40 degrees dips its nose below the ground it stood on. At age 0 the swim sway is
/// zero and the body rises its full `0.45` units; the head's lower front edge (body
/// frame `y = 2`, `z = -14`) turns to `2 cos 40° + 14 sin 40° = 10.53` below the body
/// pivot at `19.5 - 0.45`, model `y = 29.58`: 5.58 units (0.349 blocks) underground,
/// at depth `3 - 4/16` on its near side. Every leg folds back and up while swimming,
/// so the nose is the lowest point. The control is the same input with no state
/// factor: nothing pitches the body, so the drawn bottom stays at the ground row
/// (the legs, flat plates seen almost edge on from the side, add at most a sliver),
/// far above the dipped nose.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_swimming_axolotl_dips_its_nose_with_its_look() {
    use lodestone_render::entity_posture::AxolotlFactors;
    let swim = Posture {
        axolotl: AxolotlFactors { in_water: 1.0, moving: 1.0, ..AxolotlFactors::NONE },
        ..Posture::NONE
    };
    let shot = |posture| EntityDraw {
        anim: AnimInput { posture, head_pitch_deg: 40.0, age_ticks: 0.0, ..AnimInput::REST },
        ..draw("axolotl", vec![])
    };
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let still = scene.shoot(&[shot(Posture::NONE)]);
    let swimming = scene.shoot(&[shot(swim)]);
    let still_box = bbox(&silhouette(&empty, &still)).expect("the axolotl draws");
    let swim_box = bbox(&silhouette(&empty, &swimming)).expect("the swimming axolotl draws");
    let diff = changed(&still, &swimming).len();
    eprintln!("axolotl: still {still_box:?}, swimming {swim_box:?}, {diff} px changed");
    assert!(diff > 200, "swimming repaints a real share of the axolotl: {diff}");
    let nose = row(-5.58 / 16.0, 3.0 - 4.0 / 16.0);
    // The ground row at the near side of the body, and the leg plates' tips 1.5 units
    // under it: the still bottom lies between them.
    let ground = row(0.0, 3.0 - 5.5 / 16.0);
    let foot = row(-1.5 / 16.0, 3.0 - 5.5 / 16.0);
    assert!((swim_box.3 as f32 - nose).abs() <= 3.0, "swimming bottom {} vs nose row {nose:.1}", swim_box.3);
    assert!(
        still_box.3 as f32 >= ground - 3.0 && still_box.3 as f32 <= foot + 1.0,
        "still bottom {} outside the ground row {ground:.1} to the leg tips {foot:.1}",
        still_box.3
    );
    assert!(swim_box.3 > still_box.3 + 10, "control: without the swim the nose does not dip");
}
