//! `coral_tree` and `coral_claw`: a frame of nested placements (one coral block per call) grown
//! upward and sideways from the origin. Each step runs the nested placed feature at the cursor
//! and stops growing the moment it declines; the nested feature's own filter decides what counts
//! as water.

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;

use super::tree::trunk::shuffle;
use crate::blocks::Dir;
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::registry::PlacedFeature;

fn horizontal_shuffled(rng: &mut Rng) -> Vec<Dir> {
    let mut dirs = Dir::HORIZONTAL.to_vec();
    shuffle(&mut dirs, rng);
    dirs
}

pub fn place_tree(feature: &Arc<PlacedFeature>, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let mut at = origin;
    let trunk_height = rng.next_int_bounded(3) + 1;
    for _ in 0..trunk_height {
        if !feature.place_nested(level, rng, at) {
            return true;
        }
        at = at.above();
    }
    let top = at;
    let branches = rng.next_int_bounded(3) + 2;
    let dirs = horizontal_shuffled(rng);
    for dir in &dirs[..branches as usize] {
        let mut at = top.relative(*dir);
        let branch_height = rng.next_int_bounded(5) + 2;
        let mut segment = 0;
        let mut j = 0;
        while j < branch_height && feature.place_nested(level, rng, at) {
            segment += 1;
            at = at.above();
            if j == 0 || (segment >= 2 && rng.next_float() < 0.25) {
                at = at.relative(*dir);
                segment = 0;
            }
            j += 1;
        }
    }
    true
}

pub fn place_claw(feature: &Arc<PlacedFeature>, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    if !feature.place_nested(level, rng, origin) {
        return false;
    }
    let claw = Dir::HORIZONTAL[rng.next_int_bounded(4) as usize];
    let branches = rng.next_int_bounded(2) + 2;
    let mut options = [claw, claw.clockwise(), claw.counter_clockwise()];
    shuffle(&mut options, rng);
    for branch in &options[..branches as usize] {
        let mut at = origin;
        let sideways = rng.next_int_bounded(2) + 1;
        at = at.relative(*branch);
        let (segment, inward) = if *branch == claw {
            (claw, rng.next_int_bounded(3) + 2)
        } else {
            at = at.above();
            let pair = [*branch, Dir::Up];
            (pair[rng.next_int_bounded(2) as usize], rng.next_int_bounded(3) + 3)
        };
        let mut i = 0;
        while i < sideways && feature.place_nested(level, rng, at) {
            at = at.relative(segment);
            i += 1;
        }
        at = at.relative(segment.opposite()).above();
        for _ in 0..inward {
            at = at.relative(claw);
            if !feature.place_nested(level, rng, at) {
                break;
            }
            if rng.next_float() < 0.25 {
                at = at.above();
            }
        }
    }
    true
}
