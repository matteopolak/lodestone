//! The keyframe sampler against values that do not come from it: hand arithmetic on
//! a few keys, and a fixture an independent evaluator wrote from the reference
//! definitions (`scripts/keyframe-oracle.py`).

use lodestone_assets::keyframe::{Anim, Interp, KEYFRAME_COUNT, Target};

const FIXTURE: &str = include_str!("../fixtures/keyframe-samples.txt");

fn find(name: &str) -> Anim {
    *Anim::ALL
        .iter()
        .find(|a| format!("{a:?}") == name)
        .unwrap_or_else(|| panic!("fixture names an unknown animation {name}"))
}

fn target(name: &str) -> Target {
    match name {
        "POSITION" => Target::Position,
        "ROTATION" => Target::Rotation,
        "SCALE" => Target::Scale,
        other => panic!("unknown target {other}"),
    }
}

/// Every sampled channel of every driven animation agrees with the independent
/// evaluator, which works in double precision from the source text.
#[test]
fn sampling_matches_the_independent_evaluator() {
    let mut checked = 0;
    let mut worst = 0.0_f32;
    for line in FIXTURE.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let anim = find(f[0]);
        let def = anim.def();
        let channel = &def.channels[f[1].parse::<usize>().unwrap()];
        assert_eq!(channel.bone, f[2], "{line}");
        assert_eq!(channel.target, target(f[3]), "{line}");
        let millis: i64 = f[4].parse().unwrap();
        let weight: f32 = f[5].parse().unwrap();
        let want: Vec<f32> = f[6..9].iter().map(|v| v.parse().unwrap()).collect();
        let got = channel.sample(def.clock_seconds(millis), weight);
        for axis in 0..3 {
            let err = (got[axis] - want[axis]).abs();
            worst = worst.max(err);
            assert!(err < 2.0e-4, "{line}: axis {axis} sampled {} but the oracle says {}", got[axis], want[axis]);
        }
        checked += 1;
    }
    assert!(checked > 4000, "the fixture is suspiciously small: {checked}");
    eprintln!("{checked} samples, worst error {worst}");
}

/// Control: the comparison above is sensitive. Shifting the clock by a tenth of a
/// second moves enough samples outside the tolerance that a sampler reading the
/// wrong time would fail it.
#[test]
fn the_fixture_comparison_rejects_a_shifted_clock() {
    let mut off = 0;
    for line in FIXTURE.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let def = find(f[0]).def();
        let channel = &def.channels[f[1].parse::<usize>().unwrap()];
        let millis: i64 = f[4].parse::<i64>().unwrap() + 100;
        let want: Vec<f32> = f[6..9].iter().map(|v| v.parse().unwrap()).collect();
        let got = channel.sample(def.clock_seconds(millis), f[5].parse().unwrap());
        if (0..3).any(|a| (got[a] - want[a]).abs() > 2.0e-4) {
            off += 1;
        }
    }
    assert!(off > 500, "a clock off by 100 ms changed only {off} samples");
}

fn rabbit_body_rotation(seconds: f32) -> [f32; 3] {
    let def = Anim::RabbitHop.def();
    let channel = def.channels.iter().find(|c| c.bone == "body" && c.target == Target::Rotation).unwrap();
    channel.sample(seconds, 1.0)
}

/// Linear segment, by hand: between the keys at 0.2083 s (4 degrees) and 0.2917 s
/// (32.5 degrees), reached by a linear key, the midpoint of the time span is the
/// midpoint of the angles.
#[test]
fn a_linear_segment_blends_halfway_at_the_midpoint() {
    let mid = (0.2083_f32 + 0.2917) / 2.0;
    let got = rabbit_body_rotation(mid);
    let want = (4.0_f32 + 32.5) / 2.0 * std::f32::consts::PI / 180.0;
    assert!((got[0] - want).abs() < 1.0e-5, "{} vs {want}", got[0]);
    assert_eq!((got[1], got[2]), (0.0, 0.0));
}

/// Spline segment, by hand: the key at 0.4167 s (33 degrees) is reached with a spline
/// from 0.2917 s (32.5 degrees), with 4 degrees before and 18 degrees after. At a
/// quarter of the way the cubic is evaluated from those four angles.
#[test]
fn a_spline_segment_follows_the_four_surrounding_keys() {
    let (t0, t1) = (0.2917_f32, 0.4167_f32);
    let alpha = 0.25_f32;
    let seconds = t0 + (t1 - t0) * alpha;
    let (p0, p1, p2, p3) = (4.0_f64, 32.5_f64, 33.0_f64, 18.0_f64);
    let a = f64::from(alpha);
    let want = 0.5
        * (2.0 * p1 + (p2 - p0) * a + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * a * a + (3.0 * p1 - p0 - 3.0 * p2 + p3) * a * a * a);
    let got = rabbit_body_rotation(seconds)[0];
    assert!((f64::from(got) - want.to_radians()).abs() < 1.0e-4, "{got} vs {}", want.to_radians());
    // Control: the spline is not the straight line between the two middle keys.
    let straight = (p1 + (p2 - p1) * a).to_radians();
    assert!((straight - want.to_radians()).abs() > 1.0e-3, "the chosen point cannot tell a spline from a line");
}

/// A single-key channel is that key everywhere, which is how the bat's roost holds
/// its upside-down pose: the head turned half a revolution and nudged down half a texel.
#[test]
fn a_single_key_channel_holds_its_value() {
    let def = Anim::BatResting.def();
    let head = |target| def.channels.iter().find(|c| c.bone == "head" && c.target == target).unwrap();
    for millis in [0, 100, 499, 10_000] {
        let seconds = def.clock_seconds(millis);
        let rot = head(Target::Rotation).sample(seconds, 1.0);
        assert!((rot[0] - std::f32::consts::PI).abs() < 1.0e-6 && rot[1] == 0.0 && rot[2] == 0.0);
        assert_eq!(head(Target::Position).sample(seconds, 1.0), [0.0, -0.5, 0.0], "the position's y is negated");
    }
}

/// The clock wraps at the length for a looping animation and holds the last key for
/// a one-shot.
#[test]
fn looping_wraps_and_one_shots_hold() {
    let hop = Anim::RabbitHop.def();
    assert!(hop.looping);
    assert!((hop.clock_seconds(750 + 125) - 0.125).abs() < 1.0e-6);
    let croak = Anim::FrogCroak.def();
    assert!(!croak.looping);
    assert_eq!(croak.clock_seconds(9_000), 9.0);
    let late = croak.channels[0].sample(9.0, 1.0);
    let last = croak.channels[0].keys.last().unwrap().value;
    assert_eq!(late, last);
}

/// A weight scales every vector, which is how a walk cycle fades with its amplitude.
#[test]
fn a_weight_scales_the_sample() {
    let def = Anim::CamelWalk.def();
    let channel = def.channels.iter().find(|c| c.target == Target::Rotation).unwrap();
    let seconds = def.clock_seconds(400);
    let full = channel.sample(seconds, 1.0);
    let half = channel.sample(seconds, 0.5);
    assert!(full.iter().any(|v| v.abs() > 1.0e-3), "pick a channel that moves");
    for axis in 0..3 {
        assert!((half[axis] - full[axis] * 0.5).abs() < 1.0e-6);
    }
}

/// Structural facts of the generated data: keys ascend in time, every channel has
/// one, and both interpolations are present.
#[test]
fn the_generated_data_is_well_formed() {
    let (mut linear, mut spline, mut keys) = (0, 0, 0);
    for anim in Anim::ALL {
        for channel in anim.def().channels {
            assert!(!channel.keys.is_empty(), "{anim:?} {}", channel.bone);
            assert!(channel.keys.windows(2).all(|w| w[0].time <= w[1].time), "{anim:?} {} keys out of order", channel.bone);
            for key in channel.keys {
                keys += 1;
                match key.interp {
                    Interp::Linear => linear += 1,
                    Interp::CatmullRom => spline += 1,
                }
            }
        }
    }
    assert_eq!(keys, KEYFRAME_COUNT);
    assert!(linear > 0 && spline > 0);
}

/// The key count against the reference sources, counted by `grep`-style occurrence of
/// the keyframe constructor in the definition files of the animations we carry.
#[test]
#[ignore = "reads the decompiled client under .cache/mc"]
fn the_key_count_matches_the_reference_sources() {
    let dir = lodestone_mc_cache::version_root(&lodestone_mc_cache::current_version())
        .join("client-src/net/minecraft/client/animation/definitions");
    let files = [
        "RabbitAnimation", "BabyRabbitAnimation", "BatAnimation", "FrogAnimation", "CamelAnimation",
        "CamelBabyAnimation", "ArmadilloAnimation", "BabyArmadilloAnimation", "SnifferAnimation",
        "FoxBabyAnimation", "BabyAxolotlAnimation",
    ];
    let mut in_files = 0;
    for file in files {
        let text = std::fs::read_to_string(dir.join(format!("{file}.java"))).expect("definition source");
        in_files += text.matches("new Keyframe(").count();
    }
    // The sources also hold animations we do not drive (the sniffer's baby transform and
    // fall), so the carried total can only be at most the file total.
    assert!(KEYFRAME_COUNT <= in_files, "{KEYFRAME_COUNT} carried but only {in_files} in the sources");
    assert!(KEYFRAME_COUNT * 10 > in_files * 8, "{KEYFRAME_COUNT} of {in_files}: most of the sources should be carried");
}
