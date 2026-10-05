//! Pixel gates for keyframe animation through the real [`RenderState::render`] path:
//! a started animation changes the pixels of the mob it plays on, and the same input
//! drawn twice changes none.
//!
//! ```text
//! cargo test -p lodestone-shell --test entities keyframe_pixels -- --ignored --nocapture
//! ```

use super::gear_pixels::{Scene, bbox, changed, draw, silhouette};
use lodestone::entities::EntityDraw;
use lodestone_render::AnimInput;
use lodestone_render::entity_keyframe::{Flag, Keyframes, Slot};

fn with(model: &str, keyframes: Keyframes) -> EntityDraw {
    EntityDraw { anim: AnimInput { keyframes, ..AnimInput::REST }, ..draw(model, vec![]) }
}

/// Draw `model` under `a` and `b` (and `a` twice, as the control) and return the
/// silhouette size and the changed pixels between the two.
fn compare(model: &str, a: Keyframes, b: Keyframes) -> (usize, Vec<usize>) {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let first = scene.shoot(&[with(model, a)]);
    let again = scene.shoot(&[with(model, a)]);
    let second = scene.shoot(&[with(model, b)]);
    assert_eq!(first, again, "control: the same input twice must match exactly");
    let body = silhouette(&empty, &first).len();
    let diff = changed(&first, &second);
    eprintln!("{model}: {body} px, {} changed, bbox {:?}", diff.len(), bbox(&diff));
    (body, diff)
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_roosting_bat_hangs_differently_from_a_flying_one() {
    let (body, diff) = compare(
        "bat",
        Keyframes::NONE.with(Slot::Fly, 150),
        Keyframes::NONE.with(Slot::Rest, 0).flag(Flag::Resting, true),
    );
    assert!(body > 100, "the bat must draw: {body} px");
    assert!(diff.len() > body / 4, "the roost repaints a real share of the bat: {} of {body}", diff.len());
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_hopping_rabbit_moves_its_legs_and_body() {
    let (body, diff) = compare("rabbit", Keyframes::NONE, Keyframes::NONE.with(Slot::Hop, 250));
    assert!(body > 100, "the rabbit must draw: {body} px");
    assert!(diff.len() > body / 10, "the hop moves a real share of the rabbit: {} of {body}", diff.len());
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_curled_armadillo_draws_a_ball_where_the_body_was() {
    let (body, diff) = compare("armadillo", Keyframes::NONE, Keyframes::NONE.flag(Flag::Hiding, true));
    assert!(body > 100, "the armadillo must draw: {body} px");
    assert!(diff.len() > body / 5, "the shell swap repaints a real share: {} of {body}", diff.len());
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_croaking_frog_swells_its_throat() {
    let (body, diff) = compare("frog", Keyframes::NONE, Keyframes::NONE.with(Slot::Croak, 600));
    assert!(body > 50, "the frog must draw: {body} px");
    assert!(diff.len() > 20, "the sac must appear: {} px", diff.len());
}

/// A camel's full dash nod (45 degrees) tips its head forward and down so far that
/// its hump becomes its highest point. Worked from the authored cubes: the head
/// pivots at model `(0, 1, -10)` (ground at `y = 24`, sixteenths of a block); its
/// top face is `21` units above the pivot, so at rest the top is `(24 + 20) / 16 =
/// 2.75` blocks up; pitched 45 degrees, its highest corner (`y = -21`, `z = -8` from
/// the pivot) rises only `21 cos 45 - 8 sin 45 = 9.19` units, `2.01` blocks, under the
/// hump's `(24 + 13) / 16 = 2.3125`. Side on at depth 6 the camera (at `y = 0.6`, 60
/// degree vertical field of view, 240 rows) puts height `h` at depth `d` on row
/// `120 + 207.8 (0.6 - h) / d`; the nearest faces are `0.22` (head) and `0.28`
/// (hump) blocks closer.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_dashing_camel_nods_its_head_below_its_hump() {
    let at = |keyframes| EntityDraw {
        feet: glam::Vec3::new(0.0, 0.0, 6.0),
        ..with("camel", keyframes)
    };
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let still = scene.shoot(&[at(Keyframes::NONE)]);
    let again = scene.shoot(&[at(Keyframes::NONE)]);
    let nod = scene.shoot(&[at(Keyframes::NONE.with_pitch_bump(45.0))]);
    assert_eq!(still, again, "control: the same input twice must match exactly");
    let still_box = bbox(&silhouette(&empty, &still)).expect("the camel draws");
    let nod_box = bbox(&silhouette(&empty, &nod)).expect("the nodding camel draws");
    eprintln!("camel: still {still_box:?}, nodding {nod_box:?}");
    let row = |h: f32, d: f32| 120.0 + 207.8 * (0.6 - h) / d;
    let head_top = row(2.75, 6.0 - 0.22);
    let hump_top = row(2.3125, 6.0 - 0.28);
    assert!((still_box.1 as f32 - head_top).abs() <= 2.0, "still top {} vs head row {head_top}", still_box.1);
    assert!((nod_box.1 as f32 - hump_top).abs() <= 2.0, "nodding top {} vs hump row {hump_top}", nod_box.1);
    assert_eq!(still_box.3, nod_box.3, "the feet do not move");
}
