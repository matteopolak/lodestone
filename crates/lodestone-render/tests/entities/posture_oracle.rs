//! The wolf, fox and feline posture rigs, and the camel's dash head nod, against the
//! real client's pose setup.
//!
//! `tests/support/posture_jvm.txt` is written by `oracle-java/PostureOracle.java`
//! (`just oracle-posture`): it bakes each model from the client's own layer table,
//! fills the render state for a scenario, runs the real pose setup and prints every
//! part's local pose. The scenarios (their inputs) live in the dump too, so this test
//! reads both from one place and checks every part of every scenario.

use lodestone_render::entity::EntityModelSet;
use lodestone_render::entity_anim::{AnimInput, Skeleton};
use lodestone_render::entity_keyframe::Keyframes;
use lodestone_render::entity_posture::Posture;

const DUMP: &str = include_str!("../support/posture_jvm.txt");

/// One scenario: its model and the inputs the oracle filled.
struct Scenario {
    name: String,
    model: String,
    input: AnimInput,
}

/// The oracle's local pose for one part.
#[derive(Debug)]
struct Expected {
    part: String,
    values: [f32; 9],
    visible: bool,
}

fn parse() -> Vec<(Scenario, Vec<Expected>)> {
    let mut out: Vec<(Scenario, Vec<Expected>)> = Vec::new();
    for line in DUMP.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols[0] == "input" {
            let mut input = AnimInput::REST;
            let mut posture = Posture::NONE;
            for kv in &cols[3..] {
                let (key, value) = kv.split_once('=').expect("key=value");
                let v: f32 = value.parse().expect("number");
                match key {
                    "pitch" => input.head_pitch_deg = v,
                    "yaw" => input.head_yaw_deg = v,
                    "pos" => input.limb_swing = v,
                    "speed" => input.limb_swing_amount = v,
                    "age" => input.age_ticks = v,
                    "sitting" => posture.sitting = v != 0.0,
                    "crouching" => posture.crouching = v != 0.0,
                    "sprinting" => posture.sprinting = v != 0.0,
                    "sleeping" => posture.sleeping = v != 0.0,
                    "pouncing" => posture.pouncing = v != 0.0,
                    "crouch" => posture.crouch_amount = v,
                    "roll" => posture.head_roll = v,
                    "lie" => posture.lie_down = v,
                    "lie_tail" => posture.lie_down_tail = v,
                    "relax" => posture.relax = v,
                    // The camel's dash cooldown in ticks, as the shell's timers turn it
                    // into the head nod: 45 degrees at a full 55-tick cooldown.
                    "jump" => input.keyframes = Keyframes::NONE.with_pitch_bump(45.0 * v / 55.0),
                    other => panic!("unknown scenario key {other}"),
                }
            }
            input.posture = posture;
            out.push((Scenario { name: cols[1].to_owned(), model: cols[2].to_owned(), input }, Vec::new()));
        } else {
            let (scenario, parts) = out.last_mut().expect("a part line before any input line");
            assert_eq!(cols[0], scenario.name, "part line out of order");
            let mut values = [0.0; 9];
            for (slot, text) in values.iter_mut().zip(&cols[2..11]) {
                *slot = text.parse().expect("number");
            }
            parts.push(Expected { part: cols[1].to_owned(), values, visible: cols[11] == "1" });
        }
    }
    out
}

/// Every mismatch between the skeleton's pose for `input` and the oracle's parts.
fn mismatches(skeleton: &Skeleton, input: &AnimInput, expected: &[Expected]) -> Vec<String> {
    let poses = skeleton.local_poses(input);
    let hidden = skeleton.hidden_parts(input);
    let mut out = Vec::new();
    for want in expected {
        let Some(index) = poses.iter().position(|(name, _)| *name == want.part) else {
            out.push(format!("{}: missing from the baked model", want.part));
            continue;
        };
        let p = poses[index].1;
        let got = [p.x, p.y, p.z, p.x_rot, p.y_rot, p.z_rot, p.scale[0], p.scale[1], p.scale[2]];
        for (axis, (g, w)) in ["x", "y", "z", "xRot", "yRot", "zRot", "xs", "ys", "zs"]
            .iter()
            .zip(got.iter().zip(want.values))
        {
            if (g - w).abs() > 1.0e-4 {
                out.push(format!("{}.{axis}: got {g}, client {w}", want.part));
            }
        }
        if hidden.contains(&index) == want.visible {
            out.push(format!("{}: visible {} but client says {}", want.part, !hidden.contains(&index), want.visible));
        }
    }
    out
}

#[test]
fn every_posture_scenario_matches_the_client_pose_setup() {
    let set = EntityModelSet::load();
    let scenarios = parse();
    assert!(scenarios.len() >= 30, "the dump is suspiciously small: {}", scenarios.len());
    let mut failures = Vec::new();
    for (scenario, expected) in &scenarios {
        assert!(!expected.is_empty(), "{} has no parts", scenario.name);
        let skeleton = &set.get(&scenario.model).unwrap_or_else(|| panic!("no baked {}", scenario.model)).skeleton;
        assert!(
            skeleton.posture_rig().is_some() || skeleton.keyframe_rig().is_some(),
            "{} baked without its code-driven rig",
            scenario.model
        );
        for miss in mismatches(skeleton, &scenario.input, expected) {
            failures.push(format!("{}: {miss}", scenario.name));
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

/// Control: the checker sees a posture. Each resting scenario drawn standing (its
/// posture cleared) must disagree with the client, or the gate above proves nothing
/// about the postures themselves.
#[test]
fn the_posture_gate_rejects_a_standing_pose_for_a_resting_scenario() {
    let set = EntityModelSet::load();
    let mut checked = 0;
    for (scenario, expected) in parse() {
        let p = scenario.input.posture;
        let resting = p.sitting || p.sleeping || p.crouching || p.lie_down > 0.0 || p.relax > 0.0;
        if !resting {
            continue;
        }
        let skeleton = &set.get(&scenario.model).expect("baked").skeleton;
        let standing = AnimInput { posture: Posture::NONE, ..scenario.input };
        assert!(
            !mismatches(skeleton, &standing, &expected).is_empty(),
            "{}: a standing pose passes the gate",
            scenario.name
        );
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} resting scenarios were checked");
}

/// Control: the checker sees the camel's head nod. Each camel scenario with a dash
/// cooldown, drawn without it, must disagree with the client.
#[test]
fn the_camel_gate_rejects_a_head_without_its_dash_nod() {
    let set = EntityModelSet::load();
    let mut checked = 0;
    for (scenario, expected) in parse() {
        if scenario.input.keyframes.pitch_bump() <= 0.0 {
            continue;
        }
        let skeleton = &set.get(&scenario.model).expect("baked").skeleton;
        let still = AnimInput { keyframes: Keyframes::NONE, ..scenario.input };
        assert!(
            !mismatches(skeleton, &still, &expected).is_empty(),
            "{}: a camel head without its nod passes the gate",
            scenario.name
        );
        checked += 1;
    }
    assert!(checked >= 3, "only {checked} camel nod scenarios were checked");
}
