//! The **stuck-in-block** movement mechanism (`Entity.makeStuckInBlock` /
//! `Entity.stuckSpeedMultiplier`).
//!
//! A handful of blocks grab an entity that stands inside them and scale its
//! movement by a per-axis vector: cobweb `(0.25, 0.05, 0.25)`, powder snow
//! `(0.9, 1.5, 0.9)`, sweet berry bush `(0.8, 0.75, 0.8)`. Vanilla implements
//! this as a **per-tick vector**, not a drag term: at the top of `Entity.move`,
//! if a multiplier is pending it multiplies the tick's movement component-wise,
//! then zeroes both the multiplier and the velocity. The multiplier is *set* one
//! tick earlier by `checkInsideBlocks` from the block the box is inside, so there
//! is an observable one-tick lag between entering the block and being grabbed.
//!
//! These tests validate the mechanism two ways (the discipline this project
//! runs on): the first two pin the *arithmetic* directly — a preset multiplier
//! scales displacement by exactly that vector and zeroes horizontal velocity —
//! derived from first principles, not transcribed from source; the rest drive
//! the whole `tick` end-to-end through a synthetic world that reports cobweb /
//! powder-snow cells via the `CollisionView::stuck_multiplier` seam, checking the
//! emergent behaviour a player would feel (a web crawl, an arrested fall, speed
//! restored on exit).

use std::collections::HashMap;

use lodestone_physics::{
    Aabb, CollisionView, MovementInput, PhysicsProfile, PlayerState, Vec3d, tick,
};

/// Cobweb's stuck-speed multiplier (`WebBlock.entityInside`).
const COBWEB: Vec3d = Vec3d {
    x: 0.25,
    y: 0.05,
    z: 0.25,
};
/// Powder snow's stuck-speed multiplier (`PowderSnowBlock.entityInside`).
const POWDER_SNOW: Vec3d = Vec3d {
    x: 0.9,
    y: 1.5,
    z: 0.9,
};

#[derive(Default)]
struct World {
    solid: std::collections::HashSet<(i32, i32, i32)>,
    /// Cells that grab the entity, mapped to their multiplier.
    stuck: HashMap<(i32, i32, i32), Vec3d>,
}

impl World {
    fn flat_floor(r: i32, y: i32) -> Self {
        let mut w = World::default();
        for x in -r..=r {
            for z in -r..=r {
                w.solid.insert((x, y, z));
            }
        }
        w
    }
    /// Fills a rectangular column of `stuck` cells over the whole `-r..=r` x/z
    /// plane and the given y range.
    fn fill_stuck(&mut self, r: i32, ys: std::ops::RangeInclusive<i32>, m: Vec3d) {
        for y in ys {
            for x in -r..=r {
                for z in -r..=r {
                    self.stuck.insert((x, y, z), m);
                }
            }
        }
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
    fn stuck_multiplier(&self, x: i32, y: i32, z: i32) -> Option<Vec3d> {
        self.stuck.get(&(x, y, z)).copied()
    }
}

fn forward() -> MovementInput {
    MovementInput {
        forward: 1.0,
        ..MovementInput::NONE
    }
}

// --- Axis 1: the arithmetic, from first principles -------------------------

#[test]
fn a_pending_multiplier_scales_this_tick_displacement_component_wise() {
    // With a multiplier already pending and no input, a single tick must move the
    // player by exactly `velocity * multiplier` on each axis — the position is
    // committed from the scaled movement, and the post-move gravity only changes
    // velocity for the *next* tick, never this tick's displacement. Open world
    // (no collision) so the resolved movement equals the scaled delta exactly.
    let world = World::default();
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
    s.velocity = Vec3d::new(0.5, -0.5, 0.3);
    s.stuck_speed_multiplier = COBWEB;
    let before = s.position;

    tick(&mut s, MovementInput::NONE, &world, &profile);

    let dx = s.position.x - before.x;
    let dy = s.position.y - before.y;
    let dz = s.position.z - before.z;
    assert!(
        (dx - 0.5 * 0.25).abs() < 1e-12,
        "x scaled by 0.25, got {dx}"
    );
    assert!(
        (dy - (-0.5 * 0.05)).abs() < 1e-12,
        "y scaled by 0.05, got {dy}"
    );
    assert!(
        (dz - 0.3 * 0.25).abs() < 1e-12,
        "z scaled by 0.25, got {dz}"
    );
}

#[test]
fn consuming_a_multiplier_zeroes_horizontal_velocity() {
    // Vanilla's `setDeltaMovement(Vec3.ZERO)` after scaling: the horizontal
    // velocity is wiped, so momentum cannot carry through a web — each tick must
    // rebuild speed from input against a zeroed base. (Vertical velocity is then
    // reseeded by the post-move gravity, so we only pin the horizontal wipe.)
    let world = World::default();
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
    s.velocity = Vec3d::new(0.6, -0.2, -0.4);
    s.stuck_speed_multiplier = COBWEB;

    tick(&mut s, MovementInput::NONE, &world, &profile);

    assert_eq!(s.velocity.x, 0.0, "horizontal x velocity must be zeroed");
    assert_eq!(s.velocity.z, 0.0, "horizontal z velocity must be zeroed");
    // And the multiplier is one-shot: consumed, not left pending.
    assert!(
        s.stuck_speed_multiplier.length_sqr() <= 1.0e-7,
        "multiplier must be consumed (open world re-sets it to zero)"
    );
}

// --- Axis 2: emergent behaviour through the full seam ----------------------

/// Total forward (z) distance a walker covers in `ticks` ticks on flat ground,
/// optionally inside a stuck volume. Returns per-tick z advances so callers can
/// inspect the one-tick lag.
fn walk_z_advances(world: &World, ticks: usize) -> Vec<f64> {
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
    s.on_ground = true;
    let mut prev = s.position.z;
    let mut out = Vec::with_capacity(ticks);
    for _ in 0..ticks {
        tick(&mut s, forward(), world, &profile);
        out.push(s.position.z - prev);
        prev = s.position.z;
    }
    out
}

#[test]
fn cobweb_grabs_a_walker_after_a_one_tick_lag() {
    // A walker starting inside a cobweb volume advances *identically* to a free
    // walker on the first tick (the multiplier is not set until checkInsideBlocks
    // runs at the end of that tick), then is crawled to a fraction of free speed
    // from the second tick on.
    let free = World::flat_floor(64, 0);
    let mut web = World::flat_floor(64, 0);
    web.fill_stuck(64, 1..=2, COBWEB); // covers the standing player's body

    let free_adv = walk_z_advances(&free, 20);
    let web_adv = walk_z_advances(&web, 20);

    assert!(
        (free_adv[0] - web_adv[0]).abs() < 1e-12,
        "tick 0 must be unaffected (one-tick lag): free={}, web={}",
        free_adv[0],
        web_adv[0]
    );
    // From tick 1 on the web crawler is dramatically slower.
    let free_total: f64 = free_adv.iter().skip(1).sum();
    let web_total: f64 = web_adv.iter().skip(1).sum();
    assert!(
        web_total < free_total * 0.35,
        "cobweb must crawl the walker (web={web_total:.4} vs free={free_total:.4})"
    );
    assert!(web_total > 0.0, "the walker should still creep forward");
}

#[test]
fn cobweb_arrests_a_fall() {
    // A player falling through a tall cobweb column descends far slower than a
    // free-faller: each grabbed tick scales the fall by 0.05 and zeroes velocity,
    // so speed can never accumulate toward terminal velocity.
    let profile = PhysicsProfile::mc_1_21();
    let mut web = World::default();
    web.fill_stuck(4, 0..=200, COBWEB);
    let empty = World::default();

    let mut webbed = PlayerState::at(Vec3d::new(0.5, 150.0, 0.5), 0.0);
    let mut faller = PlayerState::at(Vec3d::new(0.5, 150.0, 0.5), 0.0);
    for _ in 0..40 {
        tick(&mut webbed, MovementInput::NONE, &web, &profile);
        tick(&mut faller, MovementInput::NONE, &empty, &profile);
    }
    let webbed_drop = 150.0 - webbed.position.y;
    let free_drop = 150.0 - faller.position.y;
    assert!(
        webbed_drop < free_drop * 0.2,
        "cobweb must arrest the fall (webbed dropped {webbed_drop:.3}, free {free_drop:.3})"
    );
    // Sustained: per-tick descent stays tiny rather than ramping up.
    let mut s = PlayerState::at(Vec3d::new(0.5, 150.0, 0.5), 0.0);
    let mut prev = s.position.y;
    let mut max_step = 0.0_f64;
    for _ in 0..40 {
        tick(&mut s, MovementInput::NONE, &web, &profile);
        max_step = max_step.max(prev - s.position.y);
        prev = s.position.y;
    }
    assert!(
        max_step < 0.02,
        "each web tick's descent must stay small, got max {max_step:.4}"
    );
}

#[test]
fn leaving_a_web_restores_full_speed() {
    // A short cobweb patch near spawn (z in -2..=4) on otherwise clear ground:
    // once the box walks clear of the patch the multiplier is consumed and never
    // re-set, so full walking speed returns.
    let mut world = World::flat_floor(256, 0);
    for z in -2..=4 {
        for x in -4..=4 {
            world.stuck.insert((x, 1, z), COBWEB);
            world.stuck.insert((x, 2, z), COBWEB);
        }
    }
    let free = World::flat_floor(256, 0);

    // Long enough to crawl out of the ~6-block patch (~0.025/tick) and reach a
    // cruising speed on the clear ground beyond.
    let adv = walk_z_advances(&world, 400);
    let free_adv = walk_z_advances(&free, 400);
    let late = adv[adv.len() - 1];
    let free_late = free_adv[free_adv.len() - 1];
    assert!(
        (late - free_late).abs() < 1e-6,
        "speed must recover after leaving the web: late={late:.5}, free={free_late:.5}"
    );
}

#[test]
fn powder_snow_scales_by_its_own_vector_not_cobwebs() {
    // The multiplier is chosen by the block, so powder snow (0.9, 1.5, 0.9) barely
    // slows a walker where cobweb (0.25) crawls it — proving the vector flows from
    // the seam rather than being hard-coded in the engine.
    let mut powder = World::flat_floor(64, 0);
    powder.fill_stuck(64, 1..=2, POWDER_SNOW);
    let mut web = World::flat_floor(64, 0);
    web.fill_stuck(64, 1..=2, COBWEB);
    let free = World::flat_floor(64, 0);

    let powder_total: f64 = walk_z_advances(&powder, 30).iter().skip(1).sum();
    let web_total: f64 = walk_z_advances(&web, 30).iter().skip(1).sum();
    let free_total: f64 = walk_z_advances(&free, 30).iter().skip(1).sum();

    assert!(
        powder_total > web_total * 2.0,
        "powder snow (0.9) must be far faster than cobweb (0.25): powder={powder_total:.3}, web={web_total:.3}"
    );
    assert!(
        powder_total < free_total,
        "powder snow still slows the walker below free speed"
    );
}
