//! One tick of the vehicle **we** are riding — the client-authoritative half of
//! riding.
//!
//! # Why this exists at all
//!
//! `Entity.isClientAuthoritative()` delegates to the controlling passenger
//! (`Entity.isClientAuthoritative`) and `Player.isClientAuthoritative()` is
//! `true`, so a ridden vehicle's server-side `travelRidden`
//! (`LivingEntity.travelRidden`) takes the `setDeltaMovement(Vec3.ZERO)` branch
//! and the server does nothing but **accept** `ServerboundMoveVehiclePacket`.
//! Nothing moves a boat or a horse unless the client simulates it. That is why
//! the whole riding subsystem could be individually correct and still reach zero
//! pixels of motion.
//!
//! Two families, two entirely different rules:
//!
//! * a **boat** never runs `travel` at all. `AbstractBoat.tick` classifies its
//!   surroundings ([`boat_status`]), applies buoyancy and per-status drag
//!   ([`float_boat`]), turns and accelerates off the raw key bits
//!   ([`control_boat`]), and then calls `move(SELF, deltaMovement)` directly.
//!   Gravity is `0.04`, not the living `0.08`.
//! * a **land mount** is an ordinary `LivingEntity` whose travel input is
//!   rewritten by the rider ([`ridden_input`]) and whose speed is its own
//!   `MOVEMENT_SPEED`. It routes through [`crate::travel_in_air`] like any mob,
//!   so slabs, ice and step-up cannot diverge from the player's own integrator.
//!
//! Everything here is a pure function over [`EntityMotion`] plus a small state
//! struct: no ECS, no world, no version adapter, so each clause can be gated
//! against arithmetic derived from the decompile rather than from our own
//! encoder.

use crate::collision::{CollisionView, no_collision};
use crate::entity::{AirTravelContext, EntityDimensions, EntityMotion, MoveContext, move_entity};
use crate::fluid::FluidKind;
use crate::geometry::{Aabb, Vec3d};
use crate::mth;
use crate::profile::PhysicsProfile;

// ---------------------------------------------------------------------------
// Constants, each a literal read out of the 26.2 decompile
// ---------------------------------------------------------------------------

/// `AbstractBoat.getDefaultGravity()` — **`0.04`**, half the living `0.08` in
/// [`PhysicsProfile::gravity`].
///
/// Named rather than taken from the profile because it is not
/// version-parameterised tuning: it is this entity family's override, and
/// reading the profile here would silently give a boat a player's gravity.
pub const BOAT_GRAVITY: f64 = 0.04;

/// The divisor in `floatBoat`'s buoyancy term,
/// `buoyancy * (getDefaultGravity() / 0.65)`.
const BOAT_BUOYANCY_GRAVITY_DIVISOR: f64 = 0.65;

/// `floatBoat`'s post-buoyancy vertical scale.
const BOAT_BUOYANCY_DAMPING: f64 = 0.75;

/// `AbstractBoat.controlBoat`'s forward acceleration per tick while the forward
/// key is held.
pub const BOAT_FORWARD_ACCELERATION: f64 = 0.04;

/// `AbstractBoat.controlBoat`'s backward acceleration — note it is **not** the
/// negation of the forward one; reverse is eight times weaker.
pub const BOAT_BACKWARD_ACCELERATION: f64 = 0.005;

/// `AbstractBoat.controlBoat`'s bonus acceleration while turning on the spot.
pub const BOAT_TURN_ACCELERATION: f64 = 0.005;

/// `Entity.maxUpStep()`'s base — a boat cannot step up at all.
pub const BOAT_STEP_HEIGHT: f32 = 0.0;

/// `LivingEntity.maxUpStep()` for a mount with a `Player` controlling passenger:
/// `Math.max(stepHeightAttribute, 1.0F)`.
///
/// A horse's own `Attributes.STEP_HEIGHT` is `1.0` (`AbstractHorse`'s attribute
/// supplier), so the `max` is not what makes this `1.0` for a horse — but it
/// **is** what makes a pig or a strider step a whole block while ridden, and
/// taking the attribute alone would leave those two stuck on every slab.
pub const RIDDEN_MOUNT_STEP_HEIGHT: f32 = 1.0;

/// `AbstractBoat.clampRotation`'s half-window, in degrees: a rider's yaw is held
/// within ±105° of the boat's heading.
pub const BOAT_RIDER_YAW_CLAMP_DEGREES: f32 = 105.0;

/// `AbstractHorse.executeRidersJump`'s forward nudge coefficient.
const HORSE_JUMP_FORWARD_NUDGE: f32 = 0.4;

/// `AbstractHorse`'s `Attributes.JUMP_STRENGTH` base, used by
/// [`horse_jump_impulse`] when no server-reported value is available.
pub const HORSE_BASE_JUMP_STRENGTH: f64 = 0.7;

// ---------------------------------------------------------------------------
// Boats
// ---------------------------------------------------------------------------

/// `AbstractBoat.Status` — which of the five surroundings the boat is in this
/// tick, as classified by `AbstractBoat.getStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoatStatus {
    /// Floating: the boat's floor is below the local water surface.
    InWater,
    /// The boat's roof is under *flowing* water (a non-source fluid state).
    /// Checked **before** [`Self::UnderWater`] and returns immediately, so a
    /// single flowing cell wins over any number of source cells.
    UnderFlowingWater,
    /// The boat's roof is under still water.
    UnderWater,
    /// Not in water, and something with friction is under the hull.
    OnLand,
    /// Not in water and nothing under the hull.
    InAir,
}

/// The mutable per-boat state `AbstractBoat` keeps between ticks.
///
/// Split out from [`EntityMotion`] because none of it is shared with any other
/// entity: `waterLevel` and `landFriction` are written by the classification
/// pass and read by `floatBoat` a few lines later, `deltaRotation` is a boat-only
/// angular velocity, and `lastYd` exists only to widen `getWaterLevelAbove`'s
/// scan while falling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoatState {
    /// This tick's classification, `None` before the first tick.
    pub status: Option<BoatStatus>,
    /// The previous tick's classification. `floatBoat`'s entry-into-water branch
    /// is an *edge* on this, so it cannot be derived from `status` alone.
    pub old_status: Option<BoatStatus>,
    /// `AbstractBoat.waterLevel` — the highest water surface found under the
    /// hull, or the box's own roof while submerged.
    pub water_level: f64,
    /// `AbstractBoat.landFriction`, latched by the classification pass while on
    /// land and **halved every tick a player is aboard** so a beached boat
    /// gradually stops sliding.
    pub land_friction: f32,
    /// `AbstractBoat.deltaRotation` — degrees of yaw applied per tick, decayed by
    /// the same `invFriction` as the horizontal velocity.
    pub delta_rotation: f32,
    /// `AbstractBoat.lastYd` — the post-move vertical velocity of the previous
    /// tick, recorded by `checkFallDamage`.
    pub last_yd: f64,
    /// `AbstractBoat.outOfControlTicks`. Tracked for parity (the server ejects
    /// passengers at 60) but not acted on here: ejection is a server decision and
    /// arrives as `SET_PASSENGERS`.
    pub out_of_control_ticks: f32,
}

impl Default for BoatState {
    fn default() -> Self {
        Self {
            status: None,
            old_status: None,
            // `checkInWater` opens with `waterLevel = -Double.MAX_VALUE`, and any
            // read before a classification pass would be of that sentinel.
            water_level: -f64::MAX,
            land_friction: 0.0,
            delta_rotation: 0.0,
            last_yd: 0.0,
            out_of_control_ticks: 0.0,
        }
    }
}

/// The four raw key bits `LocalPlayer.rideTick` hands a boat through
/// `AbstractBoat.setInput`.
///
/// These are **key presses, not the scaled movement vector**: vanilla passes
/// `input.keyPresses.{left,right,forward,backward}()` straight through, so
/// `modifyInput`'s `0.98` diagonal/sneak scaling never reaches a boat. Modelling
/// them as booleans rather than as an axis pair is what keeps
/// `inputRight != inputLeft` — a clause `controlBoat` really does test — from
/// becoming a float comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BoatInput {
    /// Strafe-left key.
    pub left: bool,
    /// Strafe-right key.
    pub right: bool,
    /// Forward key.
    pub up: bool,
    /// Backward key.
    pub down: bool,
}

/// `AbstractBoat.setPaddleState`'s two arguments, exactly as `controlBoat`
/// computes them.
///
/// Note the asymmetry: the **left** paddle animates on the *right* key. That is
/// vanilla's own (a boat is rowed, so turning right pulls the left oar), and
/// swapping the two is invisible to any round trip through our own encoder —
/// which is why the pair is derived by one named function with its own gate
/// rather than inline at the call site.
#[must_use]
pub fn boat_paddle_state(input: BoatInput) -> (bool, bool) {
    (
        input.right && !input.left || input.up,
        input.left && !input.right || input.up,
    )
}

/// `FluidState.getHeight(level, pos)` for the cell at `(x, y, z)`, as a fraction
/// of a block, or `None` when the cell holds no water.
///
/// `getOwnHeight()` is `amount / 9.0F`, **except** that a cell whose neighbour
/// above holds the same fluid reports a full `1.0` — that is what makes a
/// submerged column read as solid water rather than as a stack of 8/9ths.
/// Dropping the exception makes a boat sink slowly through deep water, because
/// every `waterLevel` it computes is `1/9` of a block too low.
fn water_height(view: &dyn CollisionView, x: i32, y: i32, z: i32) -> Option<f32> {
    let cell = view.fluid_at(x, y, z)?;
    if cell.kind != FluidKind::Water {
        return None;
    }
    let above_is_water = view
        .fluid_at(x, y + 1, z)
        .is_some_and(|above| above.kind == FluidKind::Water);
    Some(if above_is_water { 1.0 } else { cell.own_height() })
}

/// Whether the water at `(x, y, z)` is a **source** block (`FluidState.isSource`,
/// `amount == 8` for water).
fn water_is_source(view: &dyn CollisionView, x: i32, y: i32, z: i32) -> bool {
    view.fluid_at(x, y, z)
        .is_some_and(|cell| cell.kind == FluidKind::Water && cell.amount >= 8)
}

/// `AbstractBoat.isUnderwater()` — is the boat's **roof** submerged, and by
/// flowing or still water.
fn boat_is_underwater(view: &dyn CollisionView, bb: Aabb) -> Option<BoatStatus> {
    let probe_max_y = bb.max_y + 0.001;
    let x0 = mth::floor(bb.min_x);
    let x1 = mth::ceil(bb.max_x);
    let y0 = mth::floor(bb.max_y);
    let y1 = mth::ceil(probe_max_y);
    let z0 = mth::floor(bb.min_z);
    let z1 = mth::ceil(bb.max_z);
    let mut under_water = false;
    for x in x0..x1 {
        for y in y0..y1 {
            for z in z0..z1 {
                let Some(height) = water_height(view, x, y, z) else {
                    continue;
                };
                if probe_max_y < f64::from(y) + f64::from(height) {
                    // Flowing water returns *immediately*, before any source cell
                    // can set `underWater`. The order is observable: a boat under
                    // one flowing cell and eight source cells is
                    // `UNDER_FLOWING_WATER`, whose `invFriction` is 0.9 and whose
                    // buoyancy is zero — a very different tick from `UNDER_WATER`.
                    if !water_is_source(view, x, y, z) {
                        return Some(BoatStatus::UnderFlowingWater);
                    }
                    under_water = true;
                }
            }
        }
    }
    under_water.then_some(BoatStatus::UnderWater)
}

/// `AbstractBoat.checkInWater()` — is the boat's **floor** below a water
/// surface, and where is the highest such surface.
///
/// Returns `(in_water, water_level)`. Vanilla writes `waterLevel` as a side
/// effect *even when it returns false*, and the sentinel it leaves behind
/// (`-Double.MAX_VALUE`) is then never read, because only the `IN_WATER` arm of
/// `floatBoat` consults it. Returned as a pair rather than written through a
/// `&mut` so that fact is visible at the call site.
fn boat_check_in_water(view: &dyn CollisionView, bb: Aabb) -> (bool, f64) {
    let x0 = mth::floor(bb.min_x);
    let x1 = mth::ceil(bb.max_x);
    let y0 = mth::floor(bb.min_y);
    let y1 = mth::ceil(bb.min_y + 0.001);
    let z0 = mth::floor(bb.min_z);
    let z1 = mth::ceil(bb.max_z);
    let mut in_water = false;
    let mut water_level = -f64::MAX;
    for x in x0..x1 {
        for y in y0..y1 {
            for z in z0..z1 {
                let Some(height) = water_height(view, x, y, z) else {
                    continue;
                };
                // Vanilla computes this as a `float` (`y + fluidState.getHeight`)
                // and only then widens it into the `double` comparison, so the
                // `f32` add is reproduced rather than improved on.
                let surface = f64::from(y as f32 + height);
                water_level = mth::java_max_f64(surface, water_level);
                in_water |= bb.min_y < surface;
            }
        }
    }
    (in_water, water_level)
}

/// `AbstractBoat.getGroundFriction()` — the mean `Block.getFriction()` of every
/// block whose collision shape touches a 1 mm slab under the hull, or `NaN` when
/// there are none.
///
/// **`NaN` is load-bearing and is not an error path.** Vanilla returns
/// `friction / count` with an `int` count that really can be zero, and the caller
/// tests `friction > 0.0F` — false for `NaN` — so an empty probe falls through to
/// `IN_AIR`. Returning an `Option` here would read better and would invite a
/// caller to write `unwrap_or(0.0)`, which is the same answer for the empty case
/// but a *different* answer if a future block ever reports negative friction.
///
/// # The clause not implemented
///
/// Vanilla additionally skips `LilyPadBlock`
/// (`!(blockState.getBlock() instanceof LilyPadBlock)`), so a boat does not read
/// a lily pad as ground. There is no lily-pad predicate on [`CollisionView`] and
/// the omission is near-unobservable: `getStatus` consults the water checks
/// *first*, and a lily pad only exists on water, so the boat is classified
/// `IN_WATER` before this function is reached. Recorded rather than silently
/// dropped.
fn boat_ground_friction(view: &dyn CollisionView, bb: Aabb) -> f32 {
    let probe = Aabb::new(bb.min_x, bb.min_y - 0.001, bb.min_z, bb.max_x, bb.min_y, bb.max_z);
    let x0 = mth::floor(probe.min_x) - 1;
    let x1 = mth::ceil(probe.max_x) + 1;
    let y0 = mth::floor(probe.min_y) - 1;
    let y1 = mth::ceil(probe.max_y) + 1;
    let z0 = mth::floor(probe.min_z) - 1;
    let z1 = mth::ceil(probe.max_z) + 1;
    let mut friction = 0.0f32;
    let mut count = 0i32;
    let mut shapes = Vec::new();
    for x in x0..x1 {
        for z in z0..z1 {
            // Vanilla's corner exclusion: a column on two edges at once (a corner
            // of the inflated region) is skipped entirely, and a column on one
            // edge skips the topmost and bottommost y. The friction of a block
            // diagonally past the hull's corner cannot matter, and the y trim
            // keeps the probe from reaching a block it does not touch.
            let x_edge = i32::from(x == x0 || x == x1 - 1);
            let z_edge = i32::from(z == z0 || z == z1 - 1);
            let edges = x_edge + z_edge;
            if edges == 2 {
                continue;
            }
            for y in y0..y1 {
                if edges > 0 && (y == y0 || y == y1 - 1) {
                    continue;
                }
                shapes.clear();
                view.collision_boxes(x, y, z, &mut shapes);
                // `CollisionView::collision_boxes` already returns **world-space**
                // boxes -- the block-local shape plus its own `(x, y, z)` offset --
                // so nothing may be added here. Offsetting a second time pushed
                // every candidate to roughly twice its height, where it can never
                // meet a 1 mm probe under the hull: the count stayed 0, the mean
                // came back `NaN`, `friction > 0.0` was false, and every beached
                // boat was classified `IN_AIR`, whose `invFriction` of 0.9 is the
                // same number water uses. It is only harmless at `y == 0`.
                let touches = shapes.iter().any(|shape| shape.intersects(&probe));
                if touches {
                    friction += view.friction(x, y, z);
                    count += 1;
                }
            }
        }
    }
    // `friction / count` with an `int` divisor: 0/0 is NaN in Java too.
    friction / count as f32
}

/// `AbstractBoat.getWaterLevelAbove()` — the height of the first non-full water
/// layer above the boat's roof.
///
/// Only `floatBoat`'s entry-into-water branch reads this, and only to snap the
/// hull to the surface. The `- lastYd` in the upper bound is what widens the scan
/// while falling fast, so a boat dropped from height still finds the surface it
/// passed through this tick rather than the one at its resting depth.
fn boat_water_level_above(view: &dyn CollisionView, bb: Aabb, last_yd: f64) -> f32 {
    let x0 = mth::floor(bb.min_x);
    let x1 = mth::ceil(bb.max_x);
    let y0 = mth::floor(bb.max_y);
    let y1 = mth::ceil(bb.max_y - last_yd);
    let z0 = mth::floor(bb.min_z);
    let z1 = mth::ceil(bb.max_z);
    let mut last_y = y0;
    for y in y0..y1 {
        last_y = y;
        let mut block_height = 0.0f32;
        let mut full = false;
        'columns: for x in x0..x1 {
            for z in z0..z1 {
                if let Some(height) = water_height(view, x, y, z) {
                    // `Math.max(float, float)`; no `mth` helper exists for the
                    // f32 case and `f32::max` differs from Java only on NaN,
                    // which `water_height` cannot return.
                    block_height = block_height.max(height);
                }
                if block_height >= 1.0 {
                    full = true;
                    break 'columns;
                }
            }
        }
        if full {
            continue;
        }
        // Vanilla returns `pos.getY() + blockHeight` off the *mutable* cursor,
        // which after the loop above is the last cell it `set`. That is this
        // layer's `y` for any non-degenerate x/z range.
        return last_y as f32 + block_height;
    }
    // `return maxY + 1` — the exclusive bound, not the last visited layer.
    (if y1 > y0 { y1 } else { last_y }) as f32 + 1.0
}

/// `AbstractBoat.getStatus()` — classify the surroundings and latch the two
/// fields the classification writes (`waterLevel`, `landFriction`).
///
/// Clause by clause, and the order is the whole content of the function:
/// 1. roof submerged → that status, and `waterLevel` becomes the box's **roof**
///    rather than any measured surface;
/// 2. else floor below a surface → `IN_WATER`, with `waterLevel` from the floor
///    scan;
/// 3. else ground friction `> 0` → `ON_LAND`, latching `landFriction`;
/// 4. else `IN_AIR`.
pub fn boat_status(state: &mut BoatState, view: &dyn CollisionView, bb: Aabb) -> BoatStatus {
    if let Some(submerged) = boat_is_underwater(view, bb) {
        state.water_level = bb.max_y;
        return submerged;
    }
    let (in_water, water_level) = boat_check_in_water(view, bb);
    state.water_level = water_level;
    if in_water {
        return BoatStatus::InWater;
    }
    let friction = boat_ground_friction(view, bb);
    if friction > 0.0 {
        state.land_friction = friction;
        return BoatStatus::OnLand;
    }
    BoatStatus::InAir
}

/// `AbstractBoat.floatBoat()` — buoyancy, per-status drag, and the surface snap
/// on first entering water.
///
/// `player_aboard` is `getControllingPassenger() instanceof Player`, which gates
/// exactly one thing: the per-tick halving of `landFriction` that lets a beached
/// boat be pushed off. Passing it as a parameter rather than assuming it is what
/// keeps this usable for a boat we are merely *watching*.
pub fn float_boat(
    motion: &mut EntityMotion,
    state: &mut BoatState,
    dims: EntityDimensions,
    view: &dyn CollisionView,
    player_aboard: bool,
) {
    let bb_height = f64::from(dims.height);
    let mut vspeed = -BOAT_GRAVITY;
    let mut buoyancy = 0.0f64;
    let mut inv_friction = 0.05f32;

    let entering_water = state.old_status == Some(BoatStatus::InAir)
        && state.status != Some(BoatStatus::InAir)
        && state.status != Some(BoatStatus::OnLand);
    if entering_water {
        // `waterLevel = this.getY(1.0)`, i.e. the roof — `Entity.getY(progress)`
        // is `position.y + bbHeight * progress`.
        state.water_level = motion.position.y + bb_height;
        let bb = dims.bounding_box(motion.position);
        let target_y =
            f64::from(boat_water_level_above(view, bb, state.last_yd)) - bb_height + 0.101;
        if no_collision(view, bb.moved(0.0, target_y - motion.position.y, 0.0)) {
            motion.position = Vec3d::new(motion.position.x, target_y, motion.position.z);
            motion.velocity = motion.velocity.multiply_each(1.0, 0.0, 1.0);
            state.last_yd = 0.0;
        }
        // The reclassification is unconditional: it happens whether or not the
        // snap above was possible. Skipping it leaves the boat one tick of `IN_AIR`
        // drag while it is already floating.
        state.status = Some(BoatStatus::InWater);
        return;
    }

    match state.status {
        Some(BoatStatus::InWater) => {
            buoyancy = (state.water_level - motion.position.y) / bb_height;
            inv_friction = 0.9;
        }
        Some(BoatStatus::UnderFlowingWater) => {
            vspeed = -7.0E-4;
            inv_friction = 0.9;
        }
        Some(BoatStatus::UnderWater) => {
            // `0.01F` in vanilla — a `float` literal assigned into a `double`.
            buoyancy = f64::from(0.01f32);
            inv_friction = 0.45;
        }
        Some(BoatStatus::InAir) => inv_friction = 0.9,
        Some(BoatStatus::OnLand) => {
            inv_friction = state.land_friction;
            if player_aboard {
                state.land_friction /= 2.0;
            }
        }
        None => {}
    }

    let drag = f64::from(inv_friction);
    motion.velocity = Vec3d::new(
        motion.velocity.x * drag,
        motion.velocity.y + vspeed,
        motion.velocity.z * drag,
    );
    // The angular velocity decays by the *same* factor, which is why a boat keeps
    // turning after the key is released on water (0.9) and stops almost at once on
    // land (halved land friction).
    state.delta_rotation *= inv_friction;
    if buoyancy > 0.0 {
        motion.velocity = Vec3d::new(
            motion.velocity.x,
            (motion.velocity.y + buoyancy * (BOAT_GRAVITY / BOAT_BUOYANCY_GRAVITY_DIVISOR))
                * BOAT_BUOYANCY_DAMPING,
            motion.velocity.z,
        );
    }
}

/// `AbstractBoat.controlBoat()` — turn and accelerate off the four key bits.
///
/// Returns the paddle pair (`setPaddleState`'s arguments) so the caller can put
/// it on the wire; the yaw is written through `yaw`.
///
/// # The order inside this function is observable
///
/// `setYRot(getYRot() + deltaRotation)` happens **between** the turning-bonus
/// acceleration and the forward/back acceleration, and the final impulse is
/// applied along the **new** yaw. So a boat that starts turning this tick already
/// accelerates in the direction it has turned to, not the one it came from.
///
/// # The conjunct that is easy to drop
///
/// The turning bonus is `inputRight != inputLeft && !inputUp && !inputDown` —
/// three clauses, not one. Turning *while* holding forward gives `0.04`, not
/// `0.045`: the bonus exists to let a stationary boat pivot, and implementing
/// only the first clause makes every forward turn 12.5% too fast.
pub fn control_boat(
    motion: &mut EntityMotion,
    state: &mut BoatState,
    yaw: &mut f32,
    input: BoatInput,
) -> (bool, bool) {
    let mut acceleration = 0.0f32;
    if input.left {
        state.delta_rotation -= 1.0;
    }
    if input.right {
        state.delta_rotation += 1.0;
    }
    if input.right != input.left && !input.up && !input.down {
        acceleration += BOAT_TURN_ACCELERATION as f32;
    }
    *yaw += state.delta_rotation;
    if input.up {
        acceleration += BOAT_FORWARD_ACCELERATION as f32;
    }
    if input.down {
        acceleration -= BOAT_BACKWARD_ACCELERATION as f32;
    }
    // `Mth.sin(-yRot * π/180)` and `Mth.cos(yRot * π/180)` — note the negation is
    // on the **sine's** argument only, and both are the `float` sin table.
    let radians = f64::from(*yaw) * std::f64::consts::PI / 180.0;
    let sin = f64::from(mth::sin(-radians));
    let cos = f64::from(mth::cos(radians));
    let accel = f64::from(acceleration);
    motion.velocity = motion
        .velocity
        .add(Vec3d::new(sin * accel, 0.0, cos * accel));
    boat_paddle_state(input)
}

/// One whole tick of a boat **we** control — `AbstractBoat.tick`'s
/// locally-authoritative branch.
///
/// Returns the paddle pair for `ClientAction::PaddleBoat`, which vanilla sends
/// from inside this same branch, every tick, unconditionally.
///
/// Order, and each step is a clause of the vanilla method:
/// 1. `oldStatus = status; status = getStatus()` — the edge `floatBoat` reads;
/// 2. `outOfControlTicks` accumulates while submerged and resets otherwise;
/// 3. `floatBoat()`;
/// 4. `controlBoat()`;
/// 5. `move(MoverType.SELF, getDeltaMovement())`;
/// 6. `checkFallDamage` latches `lastYd` from the post-move velocity.
///
/// # What is deliberately not here
///
/// `super.tick()`'s `baseTick` (fire, portals, freezing), the bubble-column
/// column, the paddle sound clock, the passenger-pushing sweep at the tail, and
/// the 60-tick `ejectPassengers` — all of which are either cosmetic or a server
/// decision that arrives back as a packet.
pub fn tick_boat(
    motion: &mut EntityMotion,
    state: &mut BoatState,
    yaw: &mut f32,
    input: BoatInput,
    dims: EntityDimensions,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) -> (bool, bool) {
    let bb = dims.bounding_box(motion.position);
    state.old_status = state.status;
    state.status = Some(boat_status(state, view, bb));
    if matches!(
        state.status,
        Some(BoatStatus::UnderWater | BoatStatus::UnderFlowingWater)
    ) {
        state.out_of_control_ticks += 1.0;
    } else {
        state.out_of_control_ticks = 0.0;
    }

    // `player_aboard` is `true` by construction: this function is only reached for
    // a boat *we* ride, so `getControllingPassenger() instanceof Player` holds and
    // the per-tick halving of `landFriction` applies. Passed explicitly rather than
    // folded into `float_boat` so that function stays usable for a boat we watch.
    float_boat(motion, state, dims, view, true);
    // `controlBoat`'s own `if (this.isVehicle())` guard is likewise satisfied by
    // construction — we are the passenger — so it has no arm here.
    let paddles = control_boat(motion, state, yaw, input);

    let boat_dims = EntityDimensions::new(dims.width, dims.height, BOAT_STEP_HEIGHT);
    move_entity(motion, boat_dims, view, profile, MoveContext::default());
    // `checkFallDamage` runs inside `Entity.move`, after restitution, and its
    // first line is `lastYd = getDeltaMovement().y`.
    state.last_yd = motion.velocity.y;
    paddles
}

/// `AbstractBoat.clampRotation(passenger)` — hold a rider's yaw within ±105° of
/// the boat's heading.
///
/// Returns the rider's new yaw. Vanilla applies the same delta to `yRotO` and
/// `yHeadRot`, which this client keeps in the camera's own interpolation rather
/// than as separate fields.
///
/// The clamp is on the **wrapped** difference, so it is a window around the
/// boat's current heading and not an absolute range: a boat turning under a still
/// rider therefore drags the rider's view with it once the window's edge is
/// reached, which is the behaviour a player notices.
#[must_use]
pub fn clamp_rider_yaw(rider_yaw: f32, boat_yaw: f32) -> f32 {
    let delta = mth::wrap_degrees_f32(rider_yaw - boat_yaw);
    let target = mth::clamp_f32(
        delta,
        -BOAT_RIDER_YAW_CLAMP_DEGREES,
        BOAT_RIDER_YAW_CLAMP_DEGREES,
    );
    rider_yaw + target - delta
}

// ---------------------------------------------------------------------------
// Land mounts
// ---------------------------------------------------------------------------

/// `Pig.getRiddenSpeed`'s scale on the mount's own `MOVEMENT_SPEED`.
pub const PIG_RIDDEN_SPEED_FACTOR: f64 = 0.225;

/// `Strider.getRiddenSpeed`'s scale while **not** suffocating. The suffocating
/// arm is `0.35F` and is not modelled — see [`MountRule`].
pub const STRIDER_RIDDEN_SPEED_FACTOR: f64 = 0.55;

/// `Camel.getRiddenSpeed`'s additive bonus while the rider is sprinting.
pub const CAMEL_SPRINT_SPEED_BONUS: f32 = 0.1;

/// Which family's overrides of `getRiddenInput` / `getRiddenSpeed` a mount uses.
///
/// **`AbstractHorse`'s rule is not universal, and assuming it is produces a
/// steerable pig.** `Pig` and `Strider` both override `getRiddenInput` to a
/// **constant** `new Vec3(0.0, 0.0, 1.0)` — you cannot steer them with the
/// movement keys at all, only with the mouse — and both scale their speed down
/// hard. A pig driven by the horse rule strafes and reverses at 4.4× vanilla's
/// forward speed.
///
/// # What each variant does not model
///
/// * **`Steered`** drops `ItemBasedSteering.boostFactor()`, which is `1.0F` unless
///   a carrot/fungus-on-a-stick boost is live. The boost's duration arrives as the
///   `DATA_BOOST_TIME` entity-data field, which this client does not decode, so
///   `1.0` is the *correct* value for every state it can observe rather than a
///   stand-in. It also drops `Strider.isSuffocating()`'s `0.35F` arm.
/// * **`Camel`** drops `refuseToMove()` (sitting or mid-pose-transition), which
///   would zero both the input and the rotation, and reads
///   `getJumpCooldown() == 0` as always true — both are pose/dash state with no
///   decoded field here. The visible consequence is a sitting camel that walks.
// No `Eq`: `Steered` carries an `f64` scale, and the same reason
// `MoveContext` gave up its `Eq` when it gained one applies here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MountRule {
    /// `AbstractHorse` — the base rule. Horses, donkeys, mules, skeleton and
    /// zombie horses, llamas.
    Horse,
    /// `Pig` and `Strider` — a constant forward input and a scaled speed.
    Steered {
        /// The `MOVEMENT_SPEED` scale: [`PIG_RIDDEN_SPEED_FACTOR`] or
        /// [`STRIDER_RIDDEN_SPEED_FACTOR`].
        speed_factor: f64,
    },
    /// `Camel` — the horse input rule plus a sprint speed bonus.
    Camel,
}

/// `getRiddenInput(controller, selfInput)` — how a mount rewrites its rider's
/// movement keys, per [`MountRule`].
///
/// `AbstractHorse`'s three clauses, all of them observable:
/// 1. a standing (reared) mount on the ground with no pending jump returns
///    `Vec3.ZERO` — it does not move at all;
/// 2. sideways is **halved** (`controller.xxa * 0.5F`);
/// 3. backward is **quartered** (`if (forward <= 0) forward *= 0.25F`), and the
///    test is `<= 0`, so a zero forward stays zero either way.
///
/// [`MountRule::Steered`] replaces all three with a constant `(0, 1)`.
///
/// `standing` folds vanilla's `isStanding() && !allowStandSliding` — both are
/// rear-up animation state with no wire field on this client, so it is passed in
/// as `false` by the live caller and exists here so the clause is present rather
/// than absent.
#[must_use]
pub fn ridden_input(
    rule: MountRule,
    strafe: f32,
    forward: f32,
    on_ground: bool,
    standing: bool,
) -> (f32, f32) {
    if let MountRule::Steered { .. } = rule {
        // `Pig.getRiddenInput` / `Strider.getRiddenInput`: a bare
        // `new Vec3(0.0, 0.0, 1.0)`, with no reference to the controller at all.
        // The rear-up clause below does not apply — neither class can rear.
        return (0.0, 1.0);
    }
    if on_ground && standing {
        return (0.0, 0.0);
    }
    let sideways = strafe * 0.5;
    let forward = if forward <= 0.0 { forward * 0.25 } else { forward };
    (sideways, forward)
}

/// `getRiddenSpeed(controller)` — the mount's travel speed, per [`MountRule`].
///
/// `attribute_speed` is the mount's **own** `minecraft:movement_speed`, never the
/// rider's. Vanilla computes the `Steered` arm entirely in `double` and narrows
/// once at the end (`(float)(getAttributeValue(...) * 0.225 * boostFactor())`),
/// which is reproduced here rather than multiplying in `f32`.
#[must_use]
pub fn ridden_speed(rule: MountRule, attribute_speed: f64, rider_sprinting: bool) -> f32 {
    match rule {
        MountRule::Horse => attribute_speed as f32,
        MountRule::Steered { speed_factor } => {
            // `* boostFactor()`, which is exactly 1.0 for every state this client
            // can observe — see `MountRule`.
            (attribute_speed * speed_factor) as f32
        }
        MountRule::Camel => {
            let bonus = if rider_sprinting {
                CAMEL_SPRINT_SPEED_BONUS
            } else {
                0.0
            };
            attribute_speed as f32 + bonus
        }
    }
}

/// `LocalPlayer.aiStep`'s horse-jump charge ramp, for a jump key that has been
/// held `ticks` ticks.
///
/// **Two arms with a discontinuity at 10**, which is the whole reason this is a
/// named function with its own gate:
///
/// | ticks | scale |
/// |---|---|
/// | 0 | 0.0 |
/// | 1..=9 | `ticks * 0.1` — 0.1 … 0.9 |
/// | 10 | `0.8 + 2.0/1 * 0.1` = **1.0** |
/// | 11 | `0.8 + 2.0/2 * 0.1` = 0.9 |
/// | 12 | `0.8 + 2.0/3 * 0.1` ≈ 0.8667 |
///
/// So the charge **peaks at exactly ten ticks and then decays back toward 0.8**.
/// A fixture at five ticks measures the first arm only and cannot tell the second
/// exists; a fixture that assumes "longer hold, stronger jump" is wrong past ten.
#[must_use]
pub fn jump_riding_scale(ticks: i32) -> f32 {
    if ticks < 10 {
        // Vanilla has no clamp at zero here: `jumpRidingScale` is *assigned* 0.0
        // on the press edge and this arm is only reached while the key is held, so
        // `ticks` is at least 1 in practice. Negative ticks are the cooldown
        // latch's `-10`, which never reaches this function.
        ticks as f32 * 0.1
    } else {
        0.8 + 2.0 / (ticks - 9) as f32 * 0.1
    }
}

/// `PlayerRideableJumping.getPlayerJumpPendingScale(jumpAmount)` — the server's
/// and the mount's reading of the `0..=100` boost byte the client sends.
///
/// `jumpAmount >= 90 ? 1.0F : 0.4F + 0.4F * jumpAmount / 90.0F`. Note the floor
/// is **0.4**, not 0: even a one-tick tap jumps meaningfully.
#[must_use]
pub fn player_jump_pending_scale(jump_amount: i32) -> f32 {
    if jump_amount >= 90 {
        1.0
    } else {
        0.4 + 0.4 * jump_amount as f32 / 90.0
    }
}

/// `AbstractHorse.executeRidersJump(amount, input)` — the vertical impulse and
/// the forward nudge.
///
/// `jump_strength` is the mount's `Attributes.JUMP_STRENGTH`
/// ([`HORSE_BASE_JUMP_STRENGTH`] when the server has reported none), and
/// `block_jump_factor` is `getBlockJumpFactor()` (honey blocks lower it). Vanilla
/// computes `getJumpPower(amount)` as
/// `JUMP_STRENGTH * amount * getBlockJumpFactor() + getJumpBoostPower()` — the
/// multiplier is on the **attribute**, not on the finished impulse, so a Jump
/// Boost effect is added afterwards at full strength and is not scaled by the
/// charge.
///
/// The forward nudge is applied only when `input.z > 0.0` — i.e. the rider is
/// pressing *forward*, after [`ridden_input`]'s quartering, so a backward-pressing
/// rider gets a purely vertical hop.
pub fn horse_jump_impulse(
    motion: &mut EntityMotion,
    amount: f32,
    forward_input: f32,
    yaw: f32,
    jump_strength: f64,
    block_jump_factor: f32,
    jump_boost_power: f32,
) {
    let impulse = f64::from(
        (jump_strength as f32) * amount * block_jump_factor + jump_boost_power,
    );
    motion.velocity = Vec3d::new(motion.velocity.x, impulse, motion.velocity.z);
    if forward_input > 0.0 {
        let radians = f64::from(yaw) * std::f64::consts::PI / 180.0;
        let sin = mth::sin(radians);
        let cos = mth::cos(radians);
        motion.velocity = motion.velocity.add(Vec3d::new(
            f64::from(-HORSE_JUMP_FORWARD_NUDGE * sin * amount),
            0.0,
            f64::from(HORSE_JUMP_FORWARD_NUDGE * cos * amount),
        ));
    }
}

/// `AbstractHorse.getRiddenRotation(controller)` — a mount copies its rider's yaw
/// outright and **halves** its pitch.
#[must_use]
pub fn ridden_rotation(rider_yaw: f32, rider_pitch: f32) -> (f32, f32) {
    (rider_yaw, rider_pitch * 0.5)
}

/// One whole tick of a land mount **we** control — `LivingEntity.travelRidden`'s
/// locally-authoritative branch, plus `AbstractHorse.tickRidden`.
///
/// Order:
/// 1. `getRiddenInput` ([`ridden_input`]);
/// 2. `tickRidden` — copy the rider's rotation ([`ridden_rotation`]) and, on the
///    ground, spend any pending jump ([`horse_jump_impulse`]);
/// 3. `setSpeed(getRiddenSpeed(controller))` — the mount's own `MOVEMENT_SPEED`,
///    never the rider's;
/// 4. `travel(riddenInput)` → [`crate::travel_in_air`] with the ridden step
///    height.
///
/// `pending_jump` is consumed: it is `Some(scale)` only on the tick the rider
/// released the jump key, and vanilla clears `playerJumpPendingScale` whenever
/// the mount is on the ground whether or not the jump fired.
#[allow(clippy::too_many_arguments)]
pub fn tick_ridden_mount(
    motion: &mut EntityMotion,
    yaw: &mut f32,
    pitch: &mut f32,
    rule: MountRule,
    rider_yaw: f32,
    rider_pitch: f32,
    strafe: f32,
    forward: f32,
    pending_jump: Option<f32>,
    speed: f32,
    jump_strength: f64,
    dims: EntityDimensions,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    let (sideways, forward) = ridden_input(rule, strafe, forward, motion.on_ground, false);
    let (new_yaw, new_pitch) = ridden_rotation(rider_yaw, rider_pitch);
    *yaw = new_yaw;
    *pitch = new_pitch;
    if motion.on_ground
        && let Some(amount) = pending_jump
        && amount > 0.0
    {
        // `getBlockJumpFactor` and `getJumpBoostPower` are 1.0 / 0.0 for every
        // surface and effect this client models for a mount; passing them
        // explicitly keeps the vanilla formula intact rather than folding them
        // away, so wiring either later is a call-site change and not a rewrite.
        horse_jump_impulse(motion, amount, forward, *yaw, jump_strength, 1.0, 0.0);
    }
    let mount_dims = EntityDimensions::new(dims.width, dims.height, RIDDEN_MOUNT_STEP_HEIGHT);
    let ctx = AirTravelContext {
        yaw: *yaw,
        ..AirTravelContext::default()
    };
    crate::entity::travel_in_air(motion, mount_dims, (sideways, forward), speed, ctx, view, profile);
}
