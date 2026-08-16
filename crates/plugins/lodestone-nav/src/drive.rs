//! The **input script**, and the one place it lives.
//!
//! `docs/baritone-port.md` §4.4 is the design's central idea: every movement kind
//! is defined by a short program producing a [`MovementInput`] per tick, and the
//! movement's *cost* is obtained by running `lodestone_physics::tick` with that
//! program. The cost is therefore achievable by construction — it is the number the
//! executor produced under the same inputs against the same physics — and there is
//! no second place for the two to disagree.
//!
//! §4.8's split is what makes it a closed loop rather than a replay: **the script
//! owns which keys, the controller owns where you look.** Steering absorbs the
//! small errors that would otherwise compound into overshoot.
//!
//! This module is that shared definition. The cost model calls [`WalkDrive`]
//! against a synthetic stencil world; the executor calls the identical
//! [`WalkDrive`] against the live world. Neither has its own copy.

use lodestone_physics::{MovementInput, PlayerState, Vec3d};

/// Vanilla yaw for a horizontal direction: `0` faces `+Z` (south), increasing
/// clockwise seen from above.
///
/// `Entity` derives it as `-atan2(dx, dz)` in degrees. Deriving it here rather than
/// tabulating four constants is what keeps a diagonal (M2) from needing a fifth.
#[must_use]
pub fn yaw_towards(dx: f64, dz: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let yaw = (-dx.atan2(dz)).to_degrees() as f32;
    yaw
}

/// The `(forward, strafe)` axes that produce world-space horizontal direction
/// `(dx, dz)` at view angle `yaw`.
///
/// # Why the executor solves this rather than just holding `forward = 1.0`
///
/// `lodestone_physics`'s input assembly maps `(forward f, strafe s)` at yaw `θ` to
/// `(dx, dz) = (s·cos θ − f·sin θ, f·cos θ + s·sin θ)`. That matrix is its own
/// inverse (its determinant is `−1`), so the same expression recovers the axes from
/// a direction — and because the recovered pair is a unit vector, vanilla's
/// `movementInputToVelocity` normalisation leaves it alone and the resulting speed
/// is the full walk speed, not a fraction of it.
///
/// Holding `forward = 1.0` and *assuming* yaw is correct is the version that
/// drifts: any disagreement between the yaw the bot asked for and the yaw physics
/// actually reads becomes a heading error that grows with distance. Solving for the
/// axes makes a yaw error cost nothing, which is what lets the same executor work
/// whether or not it is permitted to steer the view at all.
#[must_use]
pub fn axes_for_world_dir(yaw_deg: f32, dx: f64, dz: f64) -> (f32, f32) {
    let len = (dx * dx + dz * dz).sqrt();
    if len < 1e-9 {
        return (0.0, 0.0);
    }
    let (ux, uz) = (dx / len, dz / len);
    let (sin, cos) = f64::from(yaw_deg).to_radians().sin_cos();
    #[allow(clippy::cast_possible_truncation)]
    let forward = (-sin * ux + cos * uz) as f32;
    #[allow(clippy::cast_possible_truncation)]
    let strafe = (cos * ux + sin * uz) as f32;
    (forward, strafe)
}

/// What one tick of a movement wants: the keys, and where to look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriveTick {
    /// The keys, as the physics engine and `SetPlayerInput` both consume them.
    pub input: MovementInput,
    /// The yaw to adopt, in degrees.
    pub yaw: f32,
}

/// The `Walk` script: aim at the destination, hold forward.
///
/// The braking phase matters and is not decoration. A walk edge that is the
/// **last** edge of a plan must stop *in* the destination cell rather than crossing
/// it, so once inside the destination the drive releases forward and lets friction
/// settle the body. A mid-plan edge does not brake: it completes the instant the
/// feet cross into the destination cell and the next edge takes over, which is what
/// produces continuous motion instead of a stutter per block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkDrive {
    /// Destination cell.
    pub cell: [i32; 3],
    /// World-space feet `y` at the destination, from the legality check.
    pub surface: f64,
    /// Whether this edge is the plan's last, and therefore brakes.
    pub brake: bool,
    /// Whether to hold sprint. M1 leaves this `false`: sprint reaches the server
    /// only as a `PlayerCommand` edge, and the sprint-overshoot rules are M2/M3.
    pub sprint: bool,
    /// Whether the caller will adopt [`DriveTick::yaw`] **before** the physics tick.
    ///
    /// This flag exists because getting it wrong is a silent 33% slowdown, which is
    /// how it was found. The axes are solved against a view angle
    /// ([`axes_for_world_dir`]); if the caller then *changes* the angle before
    /// physics reads it, the movement goes somewhere else for that tick. With one
    /// tick per block that is a third of the journey spent walking sideways, and it
    /// looks exactly like a plausible-but-slow walk rather than like a bug.
    ///
    /// * `true` — solve the axes against the yaw this tick is about to request, so
    ///   the pair is self-consistent. This is what the plugin does: it writes a
    ///   `LookIntent` that `TickSet::Intent` applies before `TickSet::Physics`.
    /// * `false` — solve against the body's current yaw and leave the view alone.
    ///   The bot then walks correctly while facing anywhere at all, which is what
    ///   makes view steering a nicety rather than a prerequisite.
    pub steer: bool,
    /// Whether this edge needs a jump to clear its destination — `true` only
    /// for [`lodestone_nav::MoveKind::StepUp`] (`Walk`/`Descend`/`Drop` all
    /// clear or fall without one). Held only while grounded and short of the
    /// destination cell (`docs/baritone-port.md` §2.3: "do not press jump if
    /// already airborne"); `lodestone_physics::tick`'s own jump-cooldown
    /// handling is what stops a held key from re-triggering mid-flight.
    pub jump: bool,
}

impl WalkDrive {
    /// The point the drive steers at: the destination cell's horizontal centre.
    #[must_use]
    pub fn target(&self) -> Vec3d {
        Vec3d::new(
            f64::from(self.cell[0]) + 0.5,
            self.surface,
            f64::from(self.cell[2]) + 0.5,
        )
    }

    /// Whether the feet are inside the destination cell horizontally.
    #[must_use]
    pub fn inside_cell(&self, position: Vec3d) -> bool {
        #[allow(clippy::cast_possible_truncation)]
        let (x, z) = (position.x.floor() as i32, position.z.floor() as i32);
        x == self.cell[0] && z == self.cell[2]
    }

    /// One tick of input from the player's **actual** state.
    ///
    /// Closed-loop: yaw and axes are both computed from where the body is now,
    /// never from a reference trajectory. That is what absorbs the errors §2.3 says
    /// compound into overshoot.
    #[must_use]
    pub fn tick(&self, state: &PlayerState) -> DriveTick {
        let target = self.target();
        let dx = target.x - state.position.x;
        let dz = target.z - state.position.z;
        let yaw = yaw_towards(dx, dz);

        // Braking: inside the destination and close enough to the centre that
        // friction will settle us there. Releasing forward rather than reversing,
        // because reversing overshoots the other way and oscillates.
        if self.brake && self.inside_cell(state.position) && (dx * dx + dz * dz) < BRAKE_RADIUS_SQR
        {
            return DriveTick {
                input: MovementInput::NONE,
                yaw,
            };
        }

        let frame = if self.steer { yaw } else { state.yaw };
        let (forward, strafe) = axes_for_world_dir(frame, dx, dz);
        // Only while grounded and still short of the destination: pressing jump
        // mid-air does nothing physically, and holding it past arrival is not
        // meaningful either since `done` will end the edge on the same tick.
        //
        // `!self.arrived(state)`, not `!self.inside_cell(...)` — see `arrived`'s
        // own doc comment for why `inside_cell` alone is unsafe here.
        let jump = self.jump && state.on_ground && !self.arrived(state);
        DriveTick {
            input: MovementInput {
                forward,
                strafe,
                jump,
                sneak: false,
                sprint: self.sprint,
                using_item: None,
            },
            yaw,
        }
    }

    /// Whether the body is both horizontally inside the destination cell *and*
    /// at its surface height.
    ///
    /// # Why `inside_cell` alone is unsafe for `StepUp`/`Descend`/`Drop`
    ///
    /// The player's AABB is 0.6 wide, so a body whose *centre* has just crossed
    /// the cell boundary still overlaps the **source** column for a few ticks —
    /// and if the source and destination surfaces are the same height (every
    /// `Walk`, by construction), that overlap is harmless: either surface
    /// answers the same question. It stops being harmless the moment the two
    /// heights differ. A body approaching a `StepUp` still straddles the low
    /// source floor when its centre first crosses into the taller
    /// destination's footprint — `on_ground` and `inside_cell` both read `true`
    /// while the body has not risen at all, and a `Descend`/`Drop` shows the
    /// mirror image, straddling the high source floor while `inside_cell` over
    /// the pit already reads `true`. `docs/baritone-port.md` §4.4's cost model
    /// found this directly: a synthetic `Drop` of 2 cells and one of 6 both
    /// "completed" in the same 5 ticks, at `y` never having left the *source*
    /// surface — the straddle window closed before the fall began, `done`
    /// fired on the horizontal coincidence alone, and the simulated cost was
    /// for a walk that never happened.
    fn arrived(&self, state: &PlayerState) -> bool {
        self.inside_cell(state.position) && (state.position.y - self.surface).abs() < SURFACE_ARRIVAL_EPS
    }

    /// Whether the movement is finished.
    ///
    /// A **volume** test, not a coordinate-equality test: the feet cell equals the
    /// destination and the body is grounded. §4.8 is explicit that a coordinate test
    /// against the cell *centre* is what produces the few-hundredths-short stall,
    /// and the point of measuring completion at the cell boundary is that crossing
    /// it is a discrete event the executor cannot miss by a rounding error.
    ///
    /// Also requires the surface height to match ([`Self::arrived`]) — necessary
    /// only since `StepUp`/`Descend`/`Drop` gave the source and destination
    /// different heights; every `Walk` already satisfies it trivially.
    ///
    /// A braking edge additionally waits for the body to be near enough to stopped,
    /// because "arrived at the goal" should look like arriving rather than like
    /// skidding through.
    #[must_use]
    pub fn done(&self, state: &PlayerState) -> bool {
        if !self.arrived(state) || !state.on_ground {
            return false;
        }
        if !self.brake {
            return true;
        }
        state.velocity.x.abs() < STOPPED_SPEED && state.velocity.z.abs() < STOPPED_SPEED
    }
}

/// The `Climb` script: hold a direction key against a climbable column, not
/// aim at a cell centre — the genuinely different script
/// `docs/autonomous-navigation.md`'s "`Climb`: stopped, and why" flagged as
/// the harder of the two things this kind needs.
///
/// Ascending holds jump every tick, never forward/strafe: `ctx.jumping` alone
/// fires `lodestone_physics::entity::travel_in_air`'s climb override, with no
/// wall to press into required — the one script that is universal across a
/// ladder (which has a wall) and a free-hanging vine strand (which may not).
/// Descending holds nothing at all: `handle_on_climbable`'s own velocity floor
/// (`-0.15`) already caps the fall, unassisted, and holding jump while
/// descending would only reverse it into an ascend.
///
/// No horizontal aiming, no `steer` flag, no `target()` — a climb has no
/// horizontal destination to aim at, which is the whole reason it could not
/// be expressed as a [`WalkDrive`] parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClimbDrive {
    /// The column this edge climbs within — `x` and `z` never change.
    pub column: [i32; 2],
    /// Destination feet cell `y`.
    pub target_y: i32,
    /// World-space feet `y` at the destination: the destination cell's own
    /// floor while continuing to climb, or the real (possibly partial-block)
    /// stand surface when dismounting.
    pub target_surface: f64,
    /// Ascending (hold jump) vs descending (hold nothing).
    pub ascending: bool,
    /// Whether the destination cell is itself climbable (continue) or solid
    /// ground (dismount). [`Self::done`] requires `on_ground` only for the
    /// latter — a body mid-column is never grounded, and requiring it there
    /// would make every non-terminal climb edge un-completable.
    pub continuing: bool,
}

impl ClimbDrive {
    /// One tick of input. No steering: yaw is left exactly where it was,
    /// since nothing here has a horizontal direction to face.
    #[must_use]
    pub fn tick(&self, state: &PlayerState) -> DriveTick {
        DriveTick {
            input: MovementInput {
                forward: 0.0,
                strafe: 0.0,
                jump: self.ascending,
                sneak: false,
                sprint: false,
                using_item: None,
            },
            yaw: state.yaw,
        }
    }

    /// Whether the movement is finished.
    ///
    /// **Explicitly vertical, not [`WalkDrive::done`]'s horizontal-cell-plus-
    /// on_ground test** — `docs/autonomous-navigation.md`'s brief for this
    /// kind is direct about why reusing that test would be wrong: "a climb is
    /// entirely vertical, so 'arrived' cannot mean an in-cell horizontal test
    /// at all." Arrival is a **vertical cell-boundary crossing**: the feet
    /// cell equals the target the instant `position.y` crosses into it,
    /// mirroring [`WalkDrive::done`]'s own "boundary, not centre" philosophy
    /// one axis over. `continuing` gates whether `on_ground` is also
    /// required, exactly as [`Self::continuing`] documents.
    #[must_use]
    pub fn done(&self, state: &PlayerState) -> bool {
        #[allow(clippy::cast_possible_truncation)]
        let cell_y = state.position.y.floor() as i32;
        if cell_y != self.target_y {
            return false;
        }
        self.continuing || state.on_ground
    }
}

/// Vertical tolerance for [`WalkDrive::arrived`], in blocks —
/// `docs/baritone-port.md` §4.8's own arrival-tolerance figure ("~0.1
/// vertical… because lily pads sit slightly above the block floor").
const SURFACE_ARRIVAL_EPS: f64 = 0.1;

/// Squared horizontal distance from the destination centre at which a braking edge
/// releases forward. `0.6²` — a little over the body's half-width, so the release
/// happens once the body is genuinely over the cell.
const BRAKE_RADIUS_SQR: f64 = 0.36;

/// Horizontal speed under which a braking edge calls itself stopped, in blocks per
/// tick. Walk speed is ~0.216 b/t, so this is ~5% of it.
const STOPPED_SPEED: f64 = 0.01;

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_physics::PhysicsProfile;

    /// The horizontal direction `(forward, strafe)` produces at `yaw`, from the
    /// physics crate's own `input_vector` — the engine's expression, not a
    /// re-derivation of it.
    fn input_direction(forward: f32, strafe: f32, yaw: f32) -> Vec3d {
        lodestone_physics::input_vector(strafe, forward, 1.0, yaw)
    }

    /// The axes really do reproduce the requested world direction through vanilla's
    /// own input assembly, so a change to the convention fails here rather than
    /// silently steering the bot sideways.
    ///
    /// # Why the tolerance is 3e-4 and not 1e-12
    ///
    /// The inverse cannot be exact, and the reason is worth writing down: this
    /// function uses `f64::sin_cos`, while `input_vector` uses **`Mth.sin`, vanilla's
    /// 65,536-entry lookup table** (`lodestone_physics::mth`), which the physics crate
    /// reproduces bit-for-bit because the server's own arithmetic depends on it. The
    /// LUT quantises the angle to `2π/65536 ≈ 9.6e-5` radians, so no closed-form
    /// inverse can round-trip tighter than about `1e-4` per component.
    ///
    /// That is *not* worth "fixing" by inverting the LUT. The residual is a heading
    /// error of ~1e-4 of a block per tick, and the executor is closed-loop — it
    /// re-solves the axes from the body's actual position every tick, so the error is
    /// corrected rather than integrated. The server's rubber-band bar is 0.25 blocks
    /// per packet (`docs/baritone-port.md` §3.2), three orders of magnitude away.
    #[test]
    fn axes_round_trip_through_the_engines_own_input_vector() {
        for yaw in [0.0_f32, 37.5, 90.0, -123.0, 180.0, 270.0] {
            for (dx, dz) in [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.7, -0.7), (-0.3, 0.9)] {
                let (forward, strafe) = axes_for_world_dir(yaw, dx, dz);
                let v = input_direction(forward, strafe, yaw);
                let len: f64 = (dx * dx + dz * dz).sqrt();
                assert!(
                    (v.x - dx / len).abs() < 3e-4 && (v.z - dz / len).abs() < 3e-4,
                    "yaw {yaw} dir ({dx},{dz}) -> ({forward},{strafe}) -> {v:?}"
                );
            }
        }
    }

    #[test]
    fn yaw_zero_faces_positive_z() {
        assert!(yaw_towards(0.0, 1.0).abs() < 1e-4);
        assert!((yaw_towards(1.0, 0.0) + 90.0).abs() < 1e-4);
        assert!((yaw_towards(-1.0, 0.0) - 90.0).abs() < 1e-4);
    }

    #[test]
    fn a_zero_length_direction_asks_for_no_input() {
        assert_eq!(axes_for_world_dir(0.0, 0.0, 0.0), (0.0, 0.0));
    }

    /// A mid-plan edge finishes at the cell boundary; a braking edge waits to stop.
    #[test]
    fn completion_is_the_cell_boundary_for_a_mid_plan_edge() {
        let drive = WalkDrive {
            cell: [1, 1, 0],
            surface: 1.0,
            brake: false,
            sprint: false,
            // `steer`: this test exercises `done()` only and never calls `tick`, so no yaw is
            // adopted.
            steer: false,
            jump: false,
        };
        let mut state = PlayerState::at(Vec3d::new(1.02, 1.0, 0.5), 0.0);
        state.velocity = Vec3d::new(0.2, 0.0, 0.0);
        assert!(!drive.done(&state), "not grounded yet");
        state.on_ground = true;
        assert!(drive.done(&state), "barely inside the cell is inside it");

        let braking = WalkDrive {
            brake: true,
            ..drive
        };
        assert!(!braking.done(&state), "still moving fast");
        state.velocity = Vec3d::ZERO;
        assert!(braking.done(&state));
    }

    /// `ClimbDrive::done` is explicitly vertical, not `WalkDrive::done` with a
    /// parameter — `docs/autonomous-navigation.md`'s own brief for this kind
    /// is direct that an in-cell horizontal test cannot mean "arrived" for a
    /// move with no horizontal destination at all. This checks the two
    /// branches directly, with no physics tick involved: a `continuing`
    /// climb is done the instant the feet cell crosses the target,
    /// regardless of `on_ground` (which a clinging body never reports); a
    /// dismount additionally requires it, or a body still airborne over the
    /// landing would be called arrived before it ever touches down.
    #[test]
    fn climb_drive_arrival_is_a_vertical_cell_crossing_gated_on_on_ground_only_when_dismounting() {
        let continuing = ClimbDrive {
            column: [0, 0],
            target_y: 2,
            target_surface: 2.0,
            ascending: true,
            continuing: true,
        };
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.5, 0.5), 0.0);
        assert!(!continuing.done(&state), "still in the source cell");
        state.position.y = 2.03;
        assert!(
            continuing.done(&state),
            "crossed into the target cell — a clinging body is never on_ground"
        );

        let dismount = ClimbDrive { continuing: false, ..continuing };
        assert!(
            !dismount.done(&state),
            "crossed the boundary but not yet settled onto real ground"
        );
        state.on_ground = true;
        assert!(dismount.done(&state), "crossed and settled — a real dismount");
    }

    /// The drive actually walks: given the engine and a floor, the body reaches the
    /// next cell. This is the closed loop in miniature, and it is the same call the
    /// cost model and the executor make.
    #[test]
    fn the_drive_moves_a_real_player_into_the_next_cell() {
        use crate::facts::{FactsTable, FixtureCensus};
        use crate::view::GridView;
        use std::sync::Arc;

        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, FixtureCensus::AIR, -64, 320, Some((-8, -8, 8, 8)));
        view.fill(-8, 0, -8, 8, 0, 8, FixtureCensus::STONE);
        let profile = PhysicsProfile::mc_1_21();

        let drive = WalkDrive {
            cell: [1, 1, 0],
            surface: 1.0,
            brake: false,
            sprint: false,
            // `steer`: the loop below does `state.yaw = step.yaw` before ticking.
            steer: true,
            jump: false,
        };
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        state.on_ground = true;
        let mut ticks = 0;
        while !drive.done(&state) && ticks < 60 {
            let step = drive.tick(&state);
            state.yaw = step.yaw;
            lodestone_physics::tick(&mut state, step.input, &view, &profile);
            ticks += 1;
        }
        assert!(drive.done(&state), "never arrived in {ticks} ticks");
        // **Half a block, not one.** The body starts at the source cell's centre and
        // `done` is a *boundary* test, so the distance covered here is 0.5 — the same
        // geometry `cost::EntryRel::Still` simulates, and it agrees at ~4 ticks. The
        // range used to be `6..=16`, which is what a whole block from rest costs; read
        // against half a block that range demanded ~0.08 blocks/tick *maximum*, i.e. it
        // could only have been satisfied by the drive walking too slowly.
        assert!(
            (3..=10).contains(&ticks),
            "{ticks} ticks to cross the half block from the cell centre to the boundary"
        );
        // The physical ceiling, which is the assertion that would actually catch a
        // too-fast drive: nothing on foot beats steady-state walk speed, and a body
        // starting from rest cannot even reach it.
        let travelled = state.position.x - 0.5;
        let rate = travelled / f64::from(ticks);
        assert!(
            rate < 0.216,
            "{rate:.3} blocks/tick average from rest exceeds the steady-state walk rate"
        );
    }

    /// The bot walks the right way even when the view is pointed somewhere else,
    /// which is what `axes_for_world_dir` buys and what makes a `LookIntent` a
    /// nicety rather than a prerequisite.
    #[test]
    fn the_drive_walks_the_right_way_with_the_view_held_wrong() {
        use crate::facts::{FactsTable, FixtureCensus};
        use crate::view::GridView;
        use std::sync::Arc;

        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, FixtureCensus::AIR, -64, 320, Some((-8, -8, 8, 8)));
        view.fill(-8, 0, -8, 8, 0, 8, FixtureCensus::STONE);
        let profile = PhysicsProfile::mc_1_21();

        let drive = WalkDrive {
            cell: [4, 1, 0],
            surface: 1.0,
            brake: true,
            sprint: false,
            // `steer`: the point of this test: the view is deliberately held at 137 deg and
            // `step.yaw` is never adopted.
            steer: false,
            jump: false,
        };
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 137.0);
        state.on_ground = true;
        for _ in 0..80 {
            // Deliberately do **not** adopt `step.yaw`: hold the view at 137°.
            let step = drive.tick(&state);
            lodestone_physics::tick(&mut state, step.input, &view, &profile);
            if drive.done(&state) {
                break;
            }
        }
        assert!(drive.done(&state), "ended at {:?}", state.position);
        assert!((state.yaw - 137.0).abs() < 1e-6, "the view never moved");
        assert!(state.position.z.abs() < 1.0, "drifted in z: {:?}", state.position);
    }
}
