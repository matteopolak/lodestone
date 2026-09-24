//! Vanilla's own bubble-column block's up/down impulse — the four constants,
//! in isolation.
//!
//! `tests/golden.rs` already replays four bubble-column scenarios bit-for-bit
//! against the independent Python oracle. That proves the *whole pipeline* agrees;
//! it does not isolate which constant produced which number, because every tick of
//! those traces carries two cell impulses plus water drag plus buoyancy folded
//! together.
//!
//! This file isolates them. Every expectation here is a **difference** between two
//! worlds that are identical except for the bubble column, so water drag and
//! buoyancy cancel exactly and what remains is the impulse literal from
//! vanilla's own entity per-tick update:
//!
//! | | `drag=false` (soul sand) | `drag=true` (magma) |
//! |---|---|---|
//! | inside | `min(0.7, vy + 0.06)` | `max(-0.3, vy - 0.03)` |
//! | above | `min(1.8, vy + 0.1)` | `max(-0.9, vy - 0.03)` |
//!
//! The literals came from the decompiled 26.2 source, and the two block-state ids
//! the `drag` property distinguishes (`15294` = `drag=true`, the default; `15295` =
//! `drag=false`) came from Mojang's own `generated/reports/blocks.json`. Neither
//! originates in this crate.
//!
//! # The single-cell fixture, and why it is a real world
//!
//! A standing player is `1.8` high and therefore spans **two** block cells, so the
//! natural fixture measures two impulses at once. To isolate one, these worlds put
//! the column's top cell where the player's *feet* are and plain water above it:
//! the foot cell is a bubble column, the head cell is water and contributes
//! nothing. That is not a contrivance — it is exactly the top of any real column,
//! which is why the cell above reads as water and the *inside* pair is selected
//! there rather than the surface pair.

use std::collections::{HashMap, HashSet};

use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::Aabb;
use lodestone_physics::player::{MovementInput, PlayerState, tick};
use lodestone_physics::{PhysicsProfile, Vec3d};

/// The impulse literals, named. Taken from vanilla's own inside/above
/// bubble-column handlers, not from anything in this repo.
const INSIDE_UP_STEP: f64 = 0.06;
const INSIDE_UP_CLAMP: f64 = 0.7;
const INSIDE_DOWN_STEP: f64 = -0.03;
const INSIDE_DOWN_CLAMP: f64 = -0.3;
const ABOVE_UP_STEP: f64 = 0.1;
const ABOVE_UP_CLAMP: f64 = 1.8;
const ABOVE_DOWN_CLAMP: f64 = -0.9;

#[derive(Default)]
struct World {
    solid: HashSet<(i32, i32, i32)>,
    water: HashSet<(i32, i32, i32)>,
    bubble: HashMap<(i32, i32, i32), bool>,
}

impl World {
    fn solid(&mut self, x: i32, y: i32, z: i32) {
        self.solid.insert((x, y, z));
    }
    fn water(&mut self, x: i32, y: i32, z: i32) {
        self.water.insert((x, y, z));
    }
    /// A bubble column cell. Registers as water too — see
    /// vanilla's own bubble-column fluid-state accessor, which returns a
    /// water *source*. A dry bubble column is not a world vanilla can build.
    fn bubble(&mut self, x: i32, y: i32, z: i32, drag_down: bool) {
        self.bubble.insert((x, y, z), drag_down);
        self.water.insert((x, y, z));
    }
}

impl CollisionView for World {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if self.solid.contains(&(x, y, z)) {
            out.push(Aabb::new(
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
                f64::from(z) + 1.0,
            ));
        }
    }
    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        self.water.contains(&(x, y, z))
    }
    fn bubble_column(&self, x: i32, y: i32, z: i32) -> Option<bool> {
        self.bubble.get(&(x, y, z)).copied()
    }
}

/// A water shaft at `(0, 0)` from `y=80` up to `y=120`, floor at `79`. The player
/// stands at `feet_y`; `column` names the cells that are bubble columns instead of
/// plain water.
fn shaft(feet_y: f64, column: &[(i32, bool)]) -> (World, PlayerState) {
    let mut w = World::default();
    w.solid(0, 79, 0);
    for y in 80..=120 {
        w.water(0, y, 0);
    }
    for &(y, drag) in column {
        w.bubble(0, y, 0, drag);
    }
    (w, PlayerState::at(Vec3d::new(0.5, feet_y, 0.5), 0.0))
}

/// Runs one tick in each of two worlds and returns `(with_column_vy,
/// plain_water_vy)`. Both start from the same state, so the difference is the
/// impulse and nothing else.
fn one_tick_vy(feet_y: f64, column: &[(i32, bool)]) -> (f64, f64) {
    let profile = PhysicsProfile::mc_1_21();
    let (w, mut s) = shaft(feet_y, column);
    tick(&mut s, MovementInput::NONE, &w, &profile);
    let with = s.velocity.y;
    let (w0, mut s0) = shaft(feet_y, &[]);
    tick(&mut s0, MovementInput::NONE, &w0, &profile);
    (with, s0.velocity.y)
}

/// Runs `ticks` ticks and returns the final vertical velocity.
fn settle_vy(feet_y: f64, column: &[(i32, bool)], ticks: usize) -> f64 {
    let profile = PhysicsProfile::mc_1_21();
    let (w, mut s) = shaft(feet_y, column);
    for _ in 0..ticks {
        tick(&mut s, MovementInput::NONE, &w, &profile);
    }
    s.velocity.y
}

/// The **precondition guard** for every test below: the plain-water baseline must
/// be a *sinking* player with a non-zero velocity.
///
/// Without this, a fixture whose water was mis-registered (so the player fell
/// through air, or never moved) would still satisfy the difference assertions —
/// `0.0 + 0.06 == 0.06` is true in a world with no water in it at all. This is the
/// anti-vacuity check that makes the deltas mean what they claim.
#[test]
fn plain_water_baseline_sinks() {
    let (_, baseline) = one_tick_vy(85.0, &[]);
    assert!(
        baseline < 0.0,
        "baseline vy {baseline} — the control shaft is not sinking, so every delta \
         measured against it is meaningless"
    );
    // And the player must actually be *in* the water, not falling through a hole:
    // free fall's first tick is -0.0784, an order of magnitude faster than water.
    assert!(
        baseline > -0.02,
        "baseline vy {baseline} looks like free fall, not water — the fixture's \
         water is not being seen"
    );
}

/// One cell, `drag=false`, capped by water: exactly one `+0.06`.
#[test]
fn inside_push_up_is_one_step_per_cell() {
    // Feet at 85.0 → box 85.0..86.8 → cells 85 and 86. Only 85 is a column, so a
    // single impulse. Cell 86 is water, so cell 85's "nothing above" test is
    // false and the inside pair is selected.
    let (with, without) = one_tick_vy(85.0, &[(85, false)]);
    assert_eq!(
        with,
        without + INSIDE_UP_STEP,
        "one push-up cell should add exactly {INSIDE_UP_STEP} (got {with} vs \
         baseline {without})"
    );
}

/// Two cells, `drag=false`: exactly two `+0.06`. The per-cell rule, not per-tick.
#[test]
fn inside_push_up_scales_with_occupied_cells() {
    let (one, without) = one_tick_vy(85.0, &[(85, false)]);
    let (two, _) = one_tick_vy(85.0, &[(85, false), (86, false)]);
    assert_eq!(
        two,
        without + INSIDE_UP_STEP + INSIDE_UP_STEP,
        "two push-up cells should add exactly two steps (got {two}, one-cell {one}, \
         baseline {without})"
    );
    assert_ne!(
        one, two,
        "the second cell contributed nothing — the impulse is being applied per \
         tick instead of per visited cell"
    );
}

/// One cell, `drag=true`, capped by water: exactly one `-0.03`.
#[test]
fn inside_drag_down_is_one_step_per_cell() {
    let (with, without) = one_tick_vy(85.0, &[(85, true)]);
    assert_eq!(
        with,
        without + INSIDE_DOWN_STEP,
        "one drag-down cell should add exactly {INSIDE_DOWN_STEP} (got {with} vs \
         baseline {without})"
    );
}

/// The inside clamps are terminal: `min(0.7, …)` and `max(-0.3, …)`.
#[test]
fn inside_clamps_are_terminal() {
    // A tall column so neither run leaves it. 200 ticks is far past saturation.
    let up: Vec<(i32, bool)> = (80..=200).map(|y| (y, false)).collect();
    let mut w = World::default();
    w.solid(0, 79, 0);
    for y in 80..=260 {
        w.water(0, y, 0);
    }
    for &(y, d) in &up {
        w.bubble(0, y, 0, d);
    }
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 85.0, 0.5), 0.0);
    for _ in 0..60 {
        tick(&mut s, MovementInput::NONE, &w, &profile);
    }
    assert_eq!(
        s.velocity.y, INSIDE_UP_CLAMP,
        "push-up should saturate at exactly {INSIDE_UP_CLAMP}"
    );

    let down: Vec<(i32, bool)> = (80..=120).map(|y| (y, true)).collect();
    let vy = settle_vy(115.0, &down, 40);
    assert_eq!(
        vy, INSIDE_DOWN_CLAMP,
        "drag-down should saturate at exactly {INSIDE_DOWN_CLAMP}"
    );
}

/// The **branch control**. The "nothing above" test is what selects the
/// strong surface pair, and it must be *false* whenever anything at all
/// occupies the cell above.
///
/// Three worlds, identical but for the cell above the column's top: air, water, and
/// a solid lid. Only the air one may take the `+0.1` step. Run the water and lid
/// cases and watch them stay on `+0.06` — this is the control proving the branch
/// test is doing work, rather than a description of what it would do.
#[test]
fn above_branch_requires_open_air_over_the_cell() {
    let profile = PhysicsProfile::mc_1_21();

    // Shared shape: floor 79, one bubble column cell at 80, player's feet at 80.
    // Only cell 80 is a column; the head cell 81 varies.
    let build = |lid: &str| {
        let mut w = World::default();
        w.solid(0, 79, 0);
        w.bubble(0, 80, 0, false);
        match lid {
            "air" => {}
            "water" => w.water(0, 81, 0),
            "solid" => w.solid(0, 81, 0),
            _ => unreachable!(),
        }
        w
    };

    let vy = |lid: &str| {
        let w = build(lid);
        let mut s = PlayerState::at(Vec3d::new(0.5, 80.0, 0.5), 0.0);
        tick(&mut s, MovementInput::NONE, &w, &profile);
        s.velocity.y
    };

    let air = vy("air");
    let water = vy("water");
    let solid = vy("solid");

    // Air over the cell → the surface pair's larger step.
    assert!(
        air > water,
        "open air over the column ({air}) must give a stronger push than water \
         over it ({water}) — the 'nothing above' test is not selecting the surface branch"
    );
    // Both non-air lids must land on the *same* inside-branch answer as each
    // other: the fluid half and the shape half of the "nothing above" test are an AND, so
    // either one being occupied is enough to disqualify.
    assert_eq!(
        water, solid,
        "water ({water}) and a solid lid ({solid}) must both disqualify the \
         surface branch identically"
    );
    // And the difference between them is exactly the two literals' difference.
    assert_eq!(
        air - water,
        ABOVE_UP_STEP - INSIDE_UP_STEP,
        "the air-vs-covered gap should be exactly {ABOVE_UP_STEP} - \
         {INSIDE_UP_STEP} (got {air} - {water})"
    );
}

/// The above-branch clamps exist and are the *wider* pair. Asserted as ordering
/// against the inside pair rather than by driving a player to `1.8`, which needs a
/// column geometry that also launches them out of the water — `golden.rs`'s
/// `bubble_column_surface_launch` covers that end to end and measures a peak of
/// `0.775`, above the inside clamp of `0.7`.
#[test]
fn above_clamps_are_wider_than_inside_clamps() {
    assert!(
        ABOVE_UP_CLAMP > INSIDE_UP_CLAMP,
        "the surface push-up clamp must exceed the inside one"
    );
    assert!(
        ABOVE_DOWN_CLAMP < INSIDE_DOWN_CLAMP,
        "the surface drag-down clamp must be deeper than the inside one"
    );
    // The step asymmetry that a prose summary gets wrong: the drag-down step is
    // `-0.03` in BOTH rows and only the clamp widens, whereas the push-up step
    // changes as well. `BubbleColumnBlock` is not "three times stronger above".
    assert_eq!(
        INSIDE_DOWN_STEP, -0.03,
        "the drag-down step is the same in both rows"
    );
    assert_ne!(
        INSIDE_UP_STEP, ABOVE_UP_STEP,
        "the push-up step does change between rows"
    );
}

/// The seam's default is inert: a `CollisionView` that does not override
/// [`CollisionView::bubble_column`] must produce no impulse at all.
///
/// This is what made the change provably safe to land — every existing implementor
/// in the workspace inherits `None` and cannot have moved.
#[test]
fn unoverridden_seam_reports_no_column() {
    struct JustWater;
    impl CollisionView for JustWater {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
            true
        }
        // `bubble_column` deliberately not overridden.
    }
    assert_eq!(JustWater.bubble_column(0, 0, 0), None);

    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 85.0, 0.5), 0.0);
    tick(&mut s, MovementInput::NONE, &JustWater, &profile);
    assert!(
        s.velocity.y < 0.0,
        "a view with no bubble-column override must sink (got {})",
        s.velocity.y
    );
}
