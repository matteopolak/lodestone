//! `LocalPlayer.updateAutoJump`'s facing-vs-moving gate, and the option that
//! turns the whole detector off — issue #201.
//!
//! # The gate
//!
//! `LocalPlayer.java:1022-1023` (26.2 client source, read directly):
//!
//! ```text
//! float facingVsMovingDotProduct2 = facingDir3.x * moveDir.x + facingDir3.z * moveDir.z;
//! if (!(facingVsMovingDotProduct2 < -0.15F)) { … }
//! ```
//!
//! So movement more than `acos(-0.15) ≈ 98.63°` off the facing direction is
//! refused, and **straight backwards never auto-jumps in vanilla either** — the
//! answer to "why doesn't ours auto-jump when I walk backwards".
//!
//! # Why "backwards does not jump" is not enough to prove the port
//!
//! A port that simply *lacks* the dot-product test would also fail to auto-jump
//! backwards, because in that case there is no obstacle in front of the probe at
//! all. It would, however, happily auto-jump on a hard strafe. So these tests
//! bracket the **constant**, not the sign:
//!
//! With yaw `0` (facing `+Z`), pitch `0`, and the input-vector fallback engaged
//! (a pinned player, `moveDistSq <= 0.001`), the rotation by yaw is the identity
//! and the dot product collapses to pure arithmetic on the normalised input:
//!
//! ```text
//! moveDir = normalize(strafe, forward) = normalize(1, -f)
//! dot     = facing · moveDir = moveDir.z = -f / sqrt(1 + f²)
//! ```
//!
//! which crosses `-0.15` at `f = sqrt(0.0225 / 0.9775) = 0.151717…`. The two
//! arms below sit either side of it:
//!
//! | `forward` | dot | vanilla | detector |
//! |---|---|---|---|
//! | `-0.14` | `-0.13865` | `!(dot < -0.15)` holds | must fire |
//! | `-0.16` | `-0.15798` | `dot < -0.15` | must refuse |
//!
//! A missing check fires on both. A threshold at `0.0` refuses both. A threshold
//! at `-0.5` (or vanilla's *other* nearby `0.15` constants — the climbable clamp
//! at `handle_on_climbable`, say) fires on both. Only `-0.15` splits this pair,
//! and the split is `0.01` wide in `f`.

use std::collections::HashMap;

use lodestone_physics::{
    Aabb, CollisionView, MovementInput, PhysicsProfile, PlayerState, Vec3d, tick_air,
};

/// A one-block-deep pit: solid floor, one-block-tall walls in all eight
/// neighbouring columns, the player's own cell free.
///
/// The pit is what engages vanilla's `moveDistSq <= 0.001` input-vector fallback
/// (`LocalPlayer.java:1008`): once the player is pressed into a corner, the
/// *actual* delta this tick is zero and the detector reconstructs the movement
/// direction from the input and the yaw. That is the branch whose dot product is
/// exact arithmetic rather than a function of collision residue, which is why the
/// bracket above can be this tight.
///
/// The wall top is `y = 2.0` and the feet start at `y = 1.0`, so `ydelta = 1.0`:
/// strictly above the `0.5` floor and at or below the `1.2` jump ceiling, i.e.
/// inside the band the detector is *for*.
struct Pit {
    cells: HashMap<(i32, i32, i32), Aabb>,
}

impl Pit {
    fn new() -> Self {
        let mut cells = HashMap::new();
        let full = Aabb::new(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        for x in -3..=3 {
            for z in -3..=3 {
                cells.insert((x, 0, z), full);
                if (x, z) != (0, 0) {
                    cells.insert((x, 1, z), full);
                }
            }
        }
        Self { cells }
    }
}

impl CollisionView for Pit {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if let Some(local) = self.cells.get(&(x, y, z)) {
            out.push(Aabb::new(
                local.min_x + f64::from(x),
                local.min_y + f64::from(y),
                local.min_z + f64::from(z),
                local.max_x + f64::from(x),
                local.max_y + f64::from(y),
                local.max_z + f64::from(z),
            ));
        }
    }
}

/// Runs the pit scenario and returns the highest feet `y` reached.
///
/// `1.0` (unchanged) means the detector never armed; anything above `1.9` means
/// it did and the player climbed out. Vanilla's jump reaches ~1.25 blocks, so
/// the two outcomes are separated by a wide margin rather than a tolerance.
fn peak_feet_y(input: MovementInput, auto_jump_enabled: bool) -> f64 {
    let world = Pit::new();
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0).with_auto_jump(auto_jump_enabled);
    state.pitch = 0.0;
    state.on_ground = true;
    let mut peak = state.position.y;
    for _ in 0..40 {
        tick_air(&mut state, input, &world, &profile);
        peak = peak.max(state.position.y);
    }
    peak
}

fn strafe_left_with_forward(forward: f32) -> MovementInput {
    MovementInput {
        strafe: 1.0,
        forward,
        ..MovementInput::NONE
    }
}

#[test]
fn a_strafe_98_degrees_off_facing_still_auto_jumps() {
    // dot = -0.14 / sqrt(1.0196) = -0.13865, and `!(dot < -0.15)` holds.
    let peak = peak_feet_y(strafe_left_with_forward(-0.14), true);
    assert!(
        peak > 1.9,
        "dot = -0.13865 is inside vanilla's -0.15 gate, so the detector must \
         arm and the player must clear the 1.0 step; peak feet y = {peak}"
    );
}

#[test]
fn a_strafe_99_degrees_off_facing_does_not_auto_jump() {
    // dot = -0.16 / sqrt(1.0256) = -0.15798 < -0.15: refused. The only thing
    // that differs from the test above is 0.02 of forward input, so nothing but
    // the threshold can explain a different outcome.
    let peak = peak_feet_y(strafe_left_with_forward(-0.16), true);
    assert!(
        peak < 1.05,
        "dot = -0.15798 is outside vanilla's -0.15 gate, so the detector must \
         refuse and the player must stay in the pit; peak feet y = {peak}"
    );
}

#[test]
fn walking_straight_backwards_never_auto_jumps() {
    // Matthew's question, and the vanilla answer: `moveDir` is exactly `-facing`,
    // so the dot product is `-1.0`. Ours refusing this is *correct*, not a bug.
    let peak = peak_feet_y(
        MovementInput {
            forward: -1.0,
            ..MovementInput::NONE
        },
        true,
    );
    assert!(
        peak < 1.05,
        "straight backwards is dot = -1.0, refused by vanilla too; peak feet y = {peak}"
    );
}

#[test]
fn walking_straight_forwards_auto_jumps() {
    // The positive control for the three negatives above: same pit, same
    // detector, dot = +1.0. Without this a threshold bug that refused
    // *everything* would read as three passes.
    let peak = peak_feet_y(
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
        true,
    );
    assert!(
        peak > 1.9,
        "dot = +1.0 must arm the detector; peak feet y = {peak}"
    );
}

#[test]
fn the_option_off_pins_the_player_in_the_pit() {
    // Issue #201's actual defect, at the physics layer: the same scenario that
    // clears the step with the option on must not clear it with the option off.
    // `lodestone_ecs::player::AutoJump` is what makes the shell's setting reach
    // this field; `lodestone-ecs`'s own tests gate that half.
    let on = peak_feet_y(
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
        true,
    );
    let off = peak_feet_y(
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
        false,
    );
    assert!(on > 1.9, "control: option on must clear the step, got {on}");
    assert!(
        off < 1.05,
        "option off must pin the player at the step; peak feet y = {off}"
    );
}
