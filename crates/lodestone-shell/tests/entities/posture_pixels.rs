//! Pixel gates for wolf, fox and cat postures through the real
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
