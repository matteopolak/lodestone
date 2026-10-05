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
