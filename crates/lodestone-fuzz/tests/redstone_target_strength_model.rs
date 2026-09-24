//! Bounded model coverage for target-block analog strength.
//!
//! The production helper receives a hit face and three fractional coordinates
//! and converts the two in-face offsets into a redstone level. Existing unit
//! examples pin the centre, edge and one quarter-offset cases; this lane adds
//! a fixed-seed generated campaign whose expected level is computed with integer
//! arithmetic, independently of the floating-point implementation.

use std::panic::{AssertUnwindSafe, catch_unwind};

use lodestone_server::{HitAxis, redstone_strength};
use proptest::prelude::*;

const CASES: u32 = 256;
const GRID: u16 = 997;

fn expected_strength(axis: HitAxis, x: u16, y: u16, z: u16) -> u8 {
    let distance =
        |coordinate: u16| u32::from((i32::from(coordinate) * 2 - i32::from(GRID)).unsigned_abs());
    let off_x = distance(x);
    let off_y = distance(y);
    let off_z = distance(z);
    let max_distance = match axis {
        HitAxis::X => off_y.max(off_z),
        HitAxis::Y => off_x.max(off_z),
        HitAxis::Z => off_x.max(off_y),
    };
    let remaining = u32::from(GRID).saturating_sub(max_distance);
    let rounded_up = (15 * remaining).div_ceil(u32::from(GRID));
    rounded_up.clamp(1, 15) as u8
}

fn axis_strategy() -> impl Strategy<Value = HitAxis> {
    prop_oneof![Just(HitAxis::X), Just(HitAxis::Y), Just(HitAxis::Z)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn generated_hit_coordinates_match_integer_model(
        axis in axis_strategy(),
        x in 0..=GRID,
        y in 0..=GRID,
        z in 0..=GRID,
    ) {
        let actual = redstone_strength(
            axis,
            f64::from(x) / f64::from(GRID),
            f64::from(y) / f64::from(GRID),
            f64::from(z) / f64::from(GRID),
        );
        prop_assert_eq!(actual, expected_strength(axis, x, y, z));
    }
}

#[test]
fn wrong_face_axis_is_rejected_by_the_detector() {
    // The Y face reads X/Z. A deliberately wrong model that reads Y/Z reads
    // the centre here while the independent model sees the off-centre X hit.
    let x = 897;
    let y = 498;
    let z = 498;
    let actual = redstone_strength(
        HitAxis::Y,
        f64::from(x) / f64::from(GRID),
        f64::from(y) / f64::from(GRID),
        f64::from(z) / f64::from(GRID),
    );
    let expected = expected_strength(HitAxis::Y, x, y, z);
    let wrong_axis = expected_strength(HitAxis::X, x, y, z);
    assert_eq!(actual, expected);
    assert_ne!(actual, wrong_axis);

    let mismatch = catch_unwind(AssertUnwindSafe(|| assert_eq!(actual, wrong_axis)));
    assert!(
        mismatch.is_err(),
        "the detector control must fail when the face axis is intentionally swapped"
    );
}
