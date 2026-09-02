//! The **transmitted `on_ground` contract**.
//!
//! `PlayerState::on_ground` is the flag the client reports to the server every
//! movement packet (the client's own move-player packet's own ground flag), and
//! it is a *distinct decision* from "did our position end up resting on a
//! block": the server runs its **own** collision from our reported position
//! and, if it ever believes we are unsupported and not descending in open
//! air, increments its own above-ground-tick counter and disconnects us with
//! the flying-kick message at its own maximum-flying-ticks value —
//! `ceil(80 * max(0.08/gravity, 1))` = **80 ticks** at default gravity. That
//! failure is silent and physical: our motion stays perfect locally while the
//! server's belief about us drifts, then kicks ~80 ticks later with nothing
//! in between to point at.
//!
//! Vanilla's rule (its own move step → its own on-ground-with-movement
//! setter) is that `onGround = verticalCollisionBelow` — i.e. "we collided
//! *downward* this move" — in **every** movement mode. Swimming, climbing a
//! ladder/vine, and free-fall do **not** get a bespoke "supported" notion;
//! they are simply its own vertical-collision-below flag, which is `false`
//! unless the downward sweep hit something. The sole override is vanilla's
//! own per-tick player update: a **spectator or passenger**
//! (riding a boat/minecart/horse) forces `onGround = false` regardless of
//! collision — see [`spectator_or_passenger_note`].
//!
//! These tests pin that `on_ground` is computed as that exact value on the way
//! *out* of a tick, in each mode, so the flag we transmit can never silently
//! diverge from the simulation the server re-runs.
//!
//! One vanilla subtlety they also pin: because a tick runs `move()` *before*
//! applying gravity, a player starting from rest reports airborne for exactly
//! **one settling tick** before the flag flips grounded. This matches the
//! server's own first-tick computation, is a single tick (~80x under the kick
//! threshold), and is guarded so a naive "report grounded immediately" change
//! that would *desync* from the server is caught.

use std::collections::HashSet;

use lodestone_physics::{
    Aabb, CollisionView, MovementInput, PhysicsProfile, PlayerState, Vec3d, tick,
};

/// The server's flying-kick threshold at default gravity
/// (vanilla's own maximum-flying-ticks value = `ceil(80 * max(0.08/gravity, 1))`). A sustained
/// on-ground property must hold for at least this many ticks to be meaningful.
const MAX_FLYING_TICKS: usize = 80;

#[derive(Default)]
struct World {
    /// Full unit-cube solid cells.
    solid: HashSet<(i32, i32, i32)>,
    /// Non-cube collision boxes in *world* coordinates, keyed by cell.
    boxes: Vec<Aabb>,
    climbable: HashSet<(i32, i32, i32)>,
    water: HashSet<(i32, i32, i32)>,
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
    fn add_box(&mut self, b: Aabb) {
        self.boxes.push(b);
    }
    fn add_climbable_column(&mut self, x: i32, z: i32, ys: std::ops::RangeInclusive<i32>) {
        for y in ys {
            self.climbable.insert((x, y, z));
        }
    }
    fn fill_water(&mut self, r: i32, ys: std::ops::RangeInclusive<i32>) {
        for y in ys {
            for x in -r..=r {
                for z in -r..=r {
                    self.water.insert((x, y, z));
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
        let cell = Aabb::new(
            f64::from(x),
            f64::from(y),
            f64::from(z),
            f64::from(x) + 1.0,
            f64::from(y) + 1.0,
            f64::from(z) + 1.0,
        );
        for b in &self.boxes {
            // Emit any explicit box that overlaps this cell (the engine queries
            // by cell; a box registered once must surface for the cells it spans).
            if b.min_x < cell.max_x
                && b.max_x > cell.min_x
                && b.min_y < cell.max_y
                && b.max_y > cell.min_y
                && b.min_z < cell.max_z
                && b.max_z > cell.min_z
            {
                out.push(*b);
            }
        }
    }
    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        self.water.contains(&(x, y, z))
    }
    fn is_climbable(&self, x: i32, y: i32, z: i32) -> bool {
        self.climbable.contains(&(x, y, z))
    }
}

fn forward() -> MovementInput {
    MovementInput {
        forward: 1.0,
        ..MovementInput::NONE
    }
}

#[test]
fn resting_on_floor_settles_to_grounded_after_one_tick() {
    // Feet resting exactly on the top face of a floor at y=0 (top = y=1.0),
    // starting from rest with velocity 0.
    //
    // Vanilla's tick order is `move()` *then* apply gravity: on the very first
    // tick the downward sweep runs with velocity.y == 0, so nothing moves,
    // nothing collides, and its own vertical-collision-below flag is false —
    // the flag reports **airborne for exactly one settling tick**. Gravity is
    // applied *after*, so the next tick's sweep moves down 0.0784, hits the
    // floor, and the flag flips to grounded and stays there. This one-tick
    // settle is vanilla-exact and harmless: it is a single tick, ~80x under
    // the flying-kick threshold, and the block directly under us keeps its
    // own "no blocks around" check false so no counting
    // even begins. We pin it so a "fix" that eagerly reports grounded on tick 0
    // (which would *not* match the server's own first-tick computation) is caught.
    let world = World::flat_floor(4, 0);
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
    s.on_ground = false;

    tick(&mut s, MovementInput::NONE, &world, &profile);
    assert!(
        !s.on_ground,
        "first tick from rest is a settle tick: move() runs before gravity so \
         nothing collides downward yet (vanilla-exact)"
    );

    tick(&mut s, MovementInput::NONE, &world, &profile);
    assert!(
        s.on_ground,
        "second tick onwards: gravity pulls into the floor, the downward sweep \
         collides, so the flag reports grounded"
    );
    // ...and it stays grounded while at rest.
    for _ in 0..8 {
        tick(&mut s, MovementInput::NONE, &world, &profile);
        assert!(s.on_ground, "must remain grounded while resting");
    }
    assert!(
        (s.position.y - 1.0).abs() < 1e-9,
        "must not sink into the floor, got y={}",
        s.position.y
    );
}

#[test]
fn free_fall_reports_airborne() {
    // High in open air with no floor anywhere: the downward sweep never
    // collides, so on_ground stays false the whole way down.
    let world = World::default();
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
    s.on_ground = true; // even if we start "grounded", the fall must clear it
    for t in 0..40 {
        tick(&mut s, MovementInput::NONE, &world, &profile);
        assert!(!s.on_ground, "free fall must report airborne (tick {t})");
    }
}

#[test]
fn landing_flips_airborne_to_grounded_exactly_once() {
    // Drop from a small height onto a floor; on_ground is false while falling
    // and becomes true on the landing tick — the transition the server uses to
    // stop counting flying ticks.
    let world = World::flat_floor(4, 0);
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 3.0, 0.5), 0.0);
    let mut saw_airborne = false;
    let mut landed = false;
    for _ in 0..120 {
        tick(&mut s, MovementInput::NONE, &world, &profile);
        if !s.on_ground {
            saw_airborne = true;
        } else {
            landed = true;
            break;
        }
    }
    assert!(saw_airborne, "must have been airborne before landing");
    assert!(landed, "must eventually land and report grounded");
    assert!(
        (s.position.y - 1.0).abs() < 1e-6,
        "should rest on the floor top (y=1.0), got {}",
        s.position.y
    );
}

#[test]
fn walking_on_floor_reports_grounded_every_tick() {
    // The sustained property that actually keeps the server from ever counting a
    // flying tick: a player walking across flat ground must report grounded on
    // *every* tick after the initial one-tick settle, for longer than the
    // 80-tick flying threshold. A single spurious airborne tick would not kick,
    // but a persistent one would — this guards the flag from ever drifting
    // airborne while genuinely supported.
    let world = World::flat_floor(64, 0);
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
    s.on_ground = true;

    // Tick 0 is the settle tick (move() before gravity); airborne is expected.
    tick(&mut s, forward(), &world, &profile);
    assert!(!s.on_ground, "tick 0 from rest is the settle tick");

    // Every subsequent tick, across the whole flying window, must stay grounded.
    for t in 1..(MAX_FLYING_TICKS + 40) {
        tick(&mut s, forward(), &world, &profile);
        assert!(
            s.on_ground,
            "a walker on flat ground must stay grounded after settling \
             (tick {t}, y={})",
            s.position.y
        );
    }
    assert!(s.position.z > 1.0, "the walker should have moved forward");
}

#[test]
fn climbing_open_ladder_reports_airborne() {
    // A ladder column in open space with no floor below. Vanilla does not invent
    // a "supported" state for climbing — on_ground stays verticalCollisionBelow,
    // which is false because nothing is hit downward. (The server does not kick:
    // the ladder block is within its own "no blocks around" check's inflated
    // box. But the flag we send is still airborne, and that is what this
    // pins.)
    let mut world = World::default();
    world.add_climbable_column(0, 0, -8..=8);
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 4.0, 0.5), 0.0);
    s.on_ground = true;
    for t in 0..60 {
        // Push into the ladder so the climb logic engages, as a real climber would.
        tick(&mut s, forward(), &world, &profile);
        assert!(
            !s.on_ground,
            "climbing an open ladder must report airborne (tick {t})"
        );
    }
}

#[test]
fn submerged_swimmer_without_floor_reports_airborne() {
    // Fully submerged with no floor: the swimmer sinks under fluid gravity and
    // never collides downward, so on_ground stays false — matching vanilla's
    // verticalCollisionBelow in the water travel branch.
    let mut world = World::default();
    world.fill_water(4, 90..=110);
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 105.0, 0.5), 0.0);
    s.on_ground = true;
    for t in 0..60 {
        tick(&mut s, MovementInput::NONE, &world, &profile);
        assert!(
            !s.on_ground,
            "a swimmer with no floor must report airborne (tick {t})"
        );
    }
}

#[test]
fn stepping_up_a_slab_preserves_grounded() {
    // Auto-stepping a ≤0.6 rise (a 0.5-high slab) must keep the player grounded
    // across the step: the step-up mechanic reuses the grounded state, so the
    // transmitted flag never flickers airborne mid-stride.
    let mut world = World::flat_floor(64, 0);
    // A slab wall at z>=3: full-width boxes rising to y=1.5 (0.5 above the floor).
    for z in 3..=64 {
        for x in -2..=2 {
            world.add_box(Aabb::new(
                f64::from(x),
                1.0,
                f64::from(z),
                f64::from(x) + 1.0,
                1.5,
                f64::from(z) + 1.0,
            ));
        }
    }
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
    s.on_ground = true;
    let start_y = s.position.y;

    // Tick 0 is the settle tick; airborne is expected there.
    tick(&mut s, forward(), &world, &profile);
    assert!(!s.on_ground, "tick 0 from rest is the settle tick");

    let mut stepped = false;
    for t in 1..80 {
        tick(&mut s, forward(), &world, &profile);
        assert!(
            s.on_ground,
            "stepping up a slab must stay grounded after settling (tick {t}, y={})",
            s.position.y
        );
        if s.position.y > start_y + 0.4 {
            stepped = true;
        }
    }
    assert!(stepped, "the player should have stepped up onto the slab");
}

/// Documents (as an executable note) the one vanilla override this engine does
/// not model **and never will**: vanilla's own per-tick player update forces
/// `onGround = false` for a **spectator or passenger** — verified in the 26.2
/// decompile: if spectating or a passenger, the on-ground flag is forced
/// `false`.
///
/// # It has a driver now, and it is not here
///
/// The note used to say "if riding state is ever added to `PlayerState`, replace
/// this with a real assertion". Riding state was added — and *not* to
/// `PlayerState`, deliberately. Whether we are a passenger is a **session** fact
/// folded from vanilla's own set-passengers packet
/// (`lodestone_ecs::session::Riding`), and the override is applied by
/// `lodestone_ecs::player::pin_passenger_to_vehicle`, which is also what snaps
/// the player onto the seat. Putting a `passenger: bool` on `PlayerState` would
/// have given the pure engine a field it can neither set nor act on beyond
/// forcing one flag, and two writers of `on_ground` in the same tick.
///
/// So this file's subject genuinely has nothing to assert, and the real
/// assertions live where the state does:
/// `a_passenger_transmits_on_ground_false_while_sitting_just_above_a_block` in
/// `lodestone-ecs/src/player.rs`, with its dismounted control.
///
/// # And the reason usually given for the override is wrong
///
/// This module's header frames `on_ground` as a wire contract policed by the
/// server's above-ground-tick counter / flying-kick message. That
/// is right for a walking player and **not** why the passenger override matters:
/// the server's float check is explicitly "and not a passenger" (vanilla's own
/// server-side packet listener) and its move handler discards a
/// passenger's reported position outright, keeping only the rotation
/// (that same listener). A mounted client cannot be
/// kicked for this flag. The override exists for the *local* readers — pose, view
/// bob, jump, flight cancel — which would otherwise treat a seated player as
/// standing on something.
#[test]
fn spectator_or_passenger_note() {
    // No behaviour to assert in the pure engine, by design — see the doc above
    // for where the assertion actually lives now.
}
