//! Keyframe rigs against the real baked corpus: every bone an animation names
//! exists, and each rig's state switches change the part matrices the way its
//! behaviour says.

use glam::Mat4;
use lodestone_render::entity_anim::{AnimInput, Skeleton};
use lodestone_render::entity::EntityModelSet;
use lodestone_render::entity_keyframe::{Flag, Keyframes, Slot, rig_spec};

const MODELS: [&str; 10] = [
    "rabbit", "rabbit_baby", "bat", "frog", "camel", "camel_baby", "armadillo", "armadillo_baby", "sniffer", "fox_baby",
];

fn skeleton<'a>(set: &'a EntityModelSet, name: &str) -> &'a Skeleton {
    &set.get(name).unwrap_or_else(|| panic!("no baked {name}")).skeleton
}

fn posed(skeleton: &Skeleton, keyframes: Keyframes) -> Vec<Mat4> {
    skeleton.pose(&AnimInput { keyframes, ..AnimInput::REST })
}

fn collapsed(m: &Mat4) -> bool {
    m.x_axis.length() < 1.0e-6 && m.y_axis.length() < 1.0e-6 && m.z_axis.length() < 1.0e-6
}

fn differs(a: &Mat4, b: &Mat4) -> bool {
    (a.to_cols_array().iter().zip(b.to_cols_array())).any(|(x, y)| (x - y).abs() > 1.0e-4)
}

/// Every bone any carried animation drives, and every part a rig hides, exists in the
/// baked model it is attached to.
#[test]
fn every_rig_bone_exists_in_its_model() {
    let set = EntityModelSet::load();
    for name in MODELS {
        let spec = rig_spec(name).unwrap_or_else(|| panic!("{name} has no rig"));
        let skel = skeleton(&set, name);
        assert!(skel.keyframe_rig().is_some(), "{name} baked without its rig");
        let anims = spec.walks.iter().map(|w| w.anim).chain(spec.states.iter().map(|(_, a)| *a));
        for bone in anims
            .flat_map(|a| a.def().channels.iter().map(|c| c.bone))
            .chain(spec.hides.iter().flat_map(|(_, names)| names.iter().copied()))
        {
            // `root` is the model's own root part, which the baked tree carries unnamed.
            let found = if bone == "root" { skel.index_of("").is_some() } else { skel.index_of(bone).is_some() };
            assert!(found, "{name}: bone {bone} is missing");
        }
    }
    // Control: a model with no rig has none, and a made-up bone is reported missing.
    assert!(skeleton(&set, "pig").keyframe_rig().is_none());
    assert!(skeleton(&set, "rabbit").index_of("no_such_bone").is_none());
}

/// A roosting bat hangs upside down: the body turns half a revolution about X.
#[test]
fn a_roosting_bat_turns_over() {
    let set = EntityModelSet::load();
    let bat = skeleton(&set, "bat");
    let body = bat.index_of("body").unwrap();
    let flying = posed(bat, Keyframes::NONE.with(Slot::Fly, 0));
    let resting = posed(bat, Keyframes::NONE.with(Slot::Rest, 0).flag(Flag::Resting, true));
    // Turning over negates the body's up axis relative to the flying pose.
    assert!(flying[body].y_axis.y > 0.0, "{:?}", flying[body].y_axis);
    assert!(resting[body].y_axis.y < -0.99, "{:?}", resting[body].y_axis);
}

/// The armadillo's shell state hides the body, hind legs and tail and shows the
/// ball; idle is the reverse. Only the part's own geometry is zeroed.
#[test]
fn a_curled_armadillo_swaps_its_body_for_the_ball() {
    let set = EntityModelSet::load();
    let skel = skeleton(&set, "armadillo");
    let [body, cube, head] = ["body", "cube", "head"].map(|n| skel.index_of(n).unwrap());
    let hiding = posed(&skel, Keyframes::NONE.flag(Flag::Hiding, true));
    assert!(collapsed(&hiding[body]) && !collapsed(&hiding[cube]));
    assert!(!collapsed(&hiding[head]), "a hidden part must not collapse its children");
    let idle = posed(&skel, Keyframes::NONE);
    assert!(!collapsed(&idle[body]) && collapsed(&idle[cube]));
}

/// The frog's croaking body shows only while the croak runs.
#[test]
fn a_frog_shows_its_throat_sac_only_while_croaking() {
    let set = EntityModelSet::load();
    let skel = skeleton(&set, "frog");
    let sac = skel.index_of("croaking_body").unwrap();
    assert!(collapsed(&posed(skel, Keyframes::NONE)[sac]));
    // The croak scales the sac up from nothing at 0.4 s, so it is still collapsed
    // early in the animation and open once it has grown.
    assert!(collapsed(&posed(skel, Keyframes::NONE.with(Slot::Croak, 100))[sac]));
    assert!(!collapsed(&posed(skel, Keyframes::NONE.with(Slot::Croak, 600))[sac]));
}

/// A hop moves the rabbit, and a rabbit that is not hopping is the same pose with or
/// without the walk input: the keyframe rig replaces the swing of a four-legged mob.
#[test]
fn a_rabbit_hop_moves_the_body_and_the_swing_does_nothing() {
    let set = EntityModelSet::load();
    let skel = skeleton(&set, "rabbit");
    let body = skel.index_of("body").unwrap();
    let rest = posed(skel, Keyframes::NONE);
    let hop = posed(skel, Keyframes::NONE.with(Slot::Hop, 250));
    assert!(differs(&rest[body], &hop[body]));
    let swung = skel.pose(&AnimInput { limb_swing: 3.0, limb_swing_amount: 1.0, ..AnimInput::REST });
    for (a, b) in rest.iter().zip(&swung) {
        assert!(!differs(a, b), "a rabbit has no limb swing");
    }
}

/// The idle head tilt takes the head from look tracking; without it the head follows
/// the look. A yaw only changes the head while the tilt is stopped.
#[test]
fn the_rabbit_head_follows_the_look_only_when_not_tilting() {
    let set = EntityModelSet::load();
    let skel = skeleton(&set, "rabbit");
    let head = skel.index_of("head").unwrap();
    let pose = |yaw: f32, keyframes| skel.pose(&AnimInput { head_yaw_deg: yaw, keyframes, ..AnimInput::REST });
    let tracking = Keyframes::NONE;
    assert!(differs(&pose(0.0, tracking)[head], &pose(40.0, tracking)[head]));
    let tilting = Keyframes::NONE.with(Slot::IdleHeadTilt, 400);
    assert!(!differs(&pose(0.0, tilting)[head], &pose(40.0, tilting)[head]));
}

/// A camel's neck is clamped: a look far past the limit gives the clamped pose.
#[test]
fn the_camel_head_turn_is_clamped() {
    let set = EntityModelSet::load();
    let skel = skeleton(&set, "camel");
    let head = skel.index_of("head").unwrap();
    let pose = |yaw: f32| skel.pose(&AnimInput { head_yaw_deg: yaw, ..AnimInput::REST })[head];
    assert!(differs(&pose(0.0), &pose(30.0)));
    assert!(!differs(&pose(30.0), &pose(90.0)), "past 30 degrees the head stops turning");
}

/// The baby fox walks by keyframes; the adult fox keeps its limb swing.
#[test]
fn a_baby_fox_walks_by_keyframes_and_the_adult_does_not() {
    let set = EntityModelSet::load();
    let walk = |name: &str| {
        let skel = skeleton(&set, name);
        let leg = skel.index_of("right_front_leg").unwrap();
        let rest = skel.pose(&AnimInput::REST)[leg];
        let moving = skel.pose(&AnimInput { limb_swing: 4.0, limb_swing_amount: 0.4, ..AnimInput::REST })[leg];
        differs(&rest, &moving)
    };
    assert!(walk("fox_baby"));
    assert!(skeleton(&set, "fox").keyframe_rig().is_none(), "control: the adult has no rig");
}
