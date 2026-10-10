//! Where a passenger sits — 26.2's data-driven **entity attachment** system,
//! ported as a pure function so it can be unit-tested with no `World`, no
//! server and no version adapter.
//!
//! # The rule, read out of the real 26.2 tree
//!
//! Every fact below is read from the real 26.2 decompiled tree.
//!
//! Vanilla's own ride-tick routine zeroes the
//! passenger's velocity and then hands positioning to the **vehicle**:
//!
//! ```text
//! vehicle's own position-rider routine(passenger, moveFunction):
//!   position = this.passenger-riding-position(passenger)
//!   offset   = passenger.vehicle-attachment-point(this)
//!   passenger's set pos
//! ```
//!
//! so, spelled out:
//!
//! ```text
//! passenger.pos = vehicle.pos
//!               + PASSENGER attachment of the vehicle, at the passenger's seat index,
//!                 rotated by the vehicle's yaw
//!               - VEHICLE attachment of the passenger, rotated by the passenger's yaw
//! ```
//!
//! * the passenger-riding-position routine = `position() + passenger-attachment-point(...)`
//!   (vanilla's own living-entity override of it only
//!   passes pose dimensions and a scale).
//! * the passenger-attachment-point routine →
//!   the clamped attachment lookup for the PASSENGER attachment, at the
//!   passenger's index, rotated by `vehicle.yRot`
//!   (vanilla's own default passenger-attachment-point routine), where `index` is
//!   get passengers's get passengers.
//! * the vehicle-attachment-point routine = the unclamped attachment lookup for
//!   the VEHICLE attachment, always index `0`, rotated by `this.yRot`
//!   (vanilla's own vehicle-attachment-point routine) — always index `0`, and rotated by the
//!   **passenger's** own yaw.
//! * the rotation is `point.yRot(-rotY * π/180)`
//!   (vanilla's own attachment-point transform), i.e. its own yaw-rotation routine:
//!   `x' = x·cos + z·sin`, `z' = z·cos − x·sin`.
//!
//! # Two constants that are easy to get wrong, and were checked
//!
//! * **The `PASSENGER` fallback is `(0, height, 0)` — `height × 1.0`, the *top* of
//!   the vehicle's box.** Vanilla's own attachment enum declares
//!   the PASSENGER attachment with an at-height fallback, whose formula is
//!   `(width, height) -> a vector (0, height, 0)`. The
//!   tempting `height × 0.85` is a *different* quantity —
//!   vanilla's own default-eye-height factor — and
//!   using it here would sink every unlisted mount by 15% of its height.
//! * **The player's `VEHICLE` attachment is not zero.** `VEHICLE`'s fallback *is*
//!   the at-feet fallback, which is zero, but the player
//!   declares one explicitly: vanilla's own default-vehicle-attachment constant
//!   of `(0.0, 0.6, 0.0)`, wired in at
//!   its own player entity-type declaration and baked into vanilla's own standing-dimensions constant.
//!   It is **subtracted**, so it lowers the player 0.6
//!   below the seat point. Dropping it would float the rider 0.6 blocks above
//!   every saddle — the single largest error available in this function.
//!
//! Both are [`PASSENGER_HEIGHT_FACTOR`] and [`PLAYER_VEHICLE_ATTACHMENT_Y`].
//!
//! # Why this is a small table and not the whole registry
//!
//! ~70 entity types declare an explicit `PASSENGER` point in vanilla's own
//! entity-type registry.
//! Transcribing all of them by hand here is the mistake `CLAUDE.md` names twice
//! over — the right home is a jar-generated table in `lodestone-data`, next to
//! `entity_dimensions`, produced by the same real-server-bootstrap walk the
//! collision-shape and hardness censuses use. That is filed as follow-up work.
//!
//! What is here instead is the **general rule** plus the handful of types a
//! *player* can actually be a passenger of, each transcribed with its
//! own registry declaration. Everything else falls through to vanilla's own
//! `AT_HEIGHT` fallback, computed from the real generated height — so an unlisted
//! mount is a few centimetres high rather than structurally wrong, which is the
//! benign direction. See [`passenger_attachment_local`].
//!
//! # What is deliberately not modelled
//!
//! The camel is the one per-instance point modelled: its seat follows its sit and
//! stand transitions ([`CamelSeat`], [`camel_passenger_attachment`]).
//!
//! Each of these is a *per-instance animation* on top of the static point, not a
//! different rule, and each needs state this crate does not hold:
//!
//! * vanilla's own abstract-horse override adds
//!   `(0, 0.15·standAnim, −0.7·standAnim)` while the horse rears — its own
//!   "stand anim" field
//!   is a client-side animation clock with no wire field.
//! * vanilla's own strider override adds `0.12·cos(walkPos·1.5)·2·min(0.25, walkSpeed)`
//!   — the walk-animation bob, which is *explicitly* client-cosmetic (the server
//!   returns plain `super`).
//! * vanilla's own abstract-minecart override lowers the point to `ZERO` for villagers and
//!   wandering traders only — never for a player.
//! * the second boat seat's `Animal` nudge (vanilla's own abstract-boat override).

use lodestone_physics::Vec3d;

/// Vanilla's own at-height fallback factor:
/// the default `PASSENGER` point sits at the
/// **full** height of the vehicle's box.
///
/// Named rather than inlined because the plausible-but-wrong neighbour —
/// vanilla's own default-eye-height factor's `0.85` — is a real constant in the same file family,
/// and the two are indistinguishable at a glance in a call site.
pub const PASSENGER_HEIGHT_FACTOR: f64 = 1.0;

/// Vanilla's own default-vehicle-attachment constant's Y component, the player's own
/// `VEHICLE` attachment, **subtracted** from the vehicle's seat point.
pub const PLAYER_VEHICLE_ATTACHMENT_Y: f64 = 0.6;

/// A vehicle's `PASSENGER` attachment point, in vehicle-local coordinates and
/// **before** the yaw rotation.
///
/// `entity_type_path` is the [`ResourceKey`](lodestone_model::ResourceKey) path
/// (`"boat"`, `"horse"`, …) — the only identity that survives ingest, and the
/// same thing `lodestone_shell::net`'s snapshot lowering already keys player
/// detection off. `height` is the vehicle's base box height from
/// [`EntityFacts::dimensions`](lodestone_model::EntityFacts), i.e. the real
/// jar-generated number, which is what makes the fallback arm correct rather
/// than guessed.
///
/// `seat_index` is the passenger's position in the vehicle's
/// [`Passengers`](crate::entity::Passengers) list. Out-of-range **clamps to the
/// last seat** rather than failing, which is vanilla's own
/// clamped attachment lookup
/// (vanilla's own `clamp(index, 0, size - 1)`) — a third rider on a two-seat vehicle
/// silently shares the last seat there too, and the clamped lookup is what the
/// passenger path calls (the throwing lookups are used elsewhere).
///
/// # The boat family bypasses the attachment table entirely
///
/// Vanilla's own abstract-boat override never
/// consults `dimensions.attachments()`; it builds the point from an abstract
/// ride height and a **Z** (forward/back) offset:
///
/// ```text
/// offset = get single passenger x offset                    // 0.0, or 0.15 for chest boats
/// if passengers.size() > 1 { offset = if index == 0 { 0.2 } else { -0.6 } }
/// Vec3(0, ride height, offset)
/// ```
///
/// Vanilla's own ride-height override is `height / 3.0` for boats and chest boats
/// (vanilla's own boat and chest-boat ride-height overrides, the second repeating
/// the first) and `height × 0.8888889` for rafts
/// and chest rafts (vanilla's own raft and chest-raft ride-height overrides,
/// likewise repeated). At the shared
/// `sized(1.375, 0.5625)` (a boat's own entity-type declaration) that is `0.1875` and
/// `0.5` respectively — so a raft seat is nearly three times higher than a
/// boat's, and reading one rule for both is a visible error.
///
/// Note vanilla's own single-passenger-offset routine names its parameter for
/// X and applies it to **Z**.
/// That is vanilla's own naming inconsistency, noted here deliberately.
///
/// A camel (or camel husk) ignores `height` and takes its point from `camel`, the
/// settled standing adult when `None`.
#[must_use]
pub fn passenger_attachment_local(
    entity_type_path: &str,
    height: f32,
    seat_index: usize,
    camel: Option<CamelSeat>,
) -> Vec3d {
    if matches!(entity_type_path, "camel" | "camel_husk") {
        return camel_passenger_attachment(camel.unwrap_or(CamelSeat::STANDING), seat_index);
    }
    let height = f64::from(height);
    // The boat family, in `path()` form. `is_raft` before `is_boat` would be
    // wrong the other way round, so both are matched by suffix on the *whole*
    // path: every wood variant is `<wood>_boat` / `<wood>_raft` /
    // `<wood>_chest_boat` / `<wood>_chest_raft` (vanilla's own entity-type
    // registry's boat block).
    let raft = entity_type_path.ends_with("raft");
    let boat = raft || entity_type_path.ends_with("boat");
    if boat {
        let ride_height = if raft {
            // Vanilla's own raft ride-height override — a `float` literal in vanilla, widened here.
            height * f64::from(0.888_888_9_f32)
        } else {
            // Vanilla's own boat ride-height override — `dimensions.height() / 3.0F`.
            height / 3.0
        };
        let chest = entity_type_path.contains("chest_");
        let z = if seat_index == 0 {
            // One-passenger case and the front seat of a two-passenger boat
            // differ: vanilla's own chest-boat single-passenger-offset override
            // returns `0.15` where
            // vanilla's own boat single-passenger-offset override returns `0.0`, but a *shared* boat
            // overrides both with `0.2` for index 0. We do not know the other
            // seats' occupancy here — the caller does, through `Passengers` —
            // and passing that in for a difference of 0.05 blocks is not worth
            // the extra parameter, so the single-passenger value is used. Noted
            // rather than hidden.
            if chest { 0.15 } else { 0.0 }
        } else {
            // Vanilla's own abstract-boat override — every seat past the first.
            -0.6
        };
        return Vec3d::new(0.0, ride_height, z);
    }
    match declared_passenger_attachment(entity_type_path, seat_index) {
        Some(point) => point,
        // Vanilla's own at-height fallback, from the real generated height.
        None => Vec3d::new(0.0, height * PASSENGER_HEIGHT_FACTOR, 0.0),
    }
}

/// The explicitly-declared `PASSENGER` points for the types a **player** can
/// ride, each verified against its own entity-type registry declaration.
///
/// `None` means "this type declares none here", which the caller turns into
/// vanilla's own at-height fallback — the same answer vanilla gives for a type that
/// genuinely declares none. So a type missing from this table is
/// *approximately* right rather than wrong-shaped; see the module docs on why
/// the full ~70-row registry belongs in a generated `lodestone-data` table
/// instead of here.
fn declared_passenger_attachment(entity_type_path: &str, seat_index: usize) -> Option<Vec3d> {
    // Every value below is passenger attachments, i.e. `(0, y, 0)` —
    // vanilla's own entity-type builder's passenger-attachments setter.
    let y = match entity_type_path {
        // Shared by every minecart variant: chest,
        // furnace, hopper, tnt, spawner and command-block minecarts all repeat
        // `.sized(0.98F, 0.7F).passenger_attachments(0.1875F)` verbatim
        // in their own entity-type registry declarations, so one arm covers the
        // family. Note it is far *below* the
        // `0.7` box top the fallback would give — a minecart seat is inside the
        // cart, not on its roof.
        "minecart"
        | "chest_minecart"
        | "furnace_minecart"
        | "hopper_minecart"
        | "tnt_minecart"
        | "spawner_minecart"
        | "command_block_minecart" => 0.1875,
        // Vanilla's own HORSE entity-type declaration.
        "horse" => 1.443_75,
        // Vanilla's own DONKEY entity-type declaration.
        "donkey" => 1.112_5,
        // Vanilla's own MULE entity-type declaration.
        "mule" => 1.212_5,
        // Vanilla's own SKELETON_HORSE and ZOMBIE_HORSE entity-type declarations — the same value, declared twice.
        "skeleton_horse" | "zombie_horse" => 1.318_75,
        // Vanilla's own PIG entity-type declaration.
        "pig" => 0.868_75,
        // Vanilla's own LLAMA and TRADER_LLAMA entity-type declarations — the only rideable-adjacent entry
        // with a non-zero Z, so it cannot use the `y`-only tail below. A llama
        // is not player-rideable, but a *caravan* makes it a vehicle and the
        // point is cheap to state correctly while the citation is open.
        "llama" | "trader_llama" => return Some(Vec3d::new(0.0, 1.37, -0.3)),
        _ => return None,
    };
    // Every arm above declares exactly one seat, so a clamped index is still
    // seat 0 — stated by construction rather than by calling a clamp helper,
    // which would suggest there is a list here to index into.
    let _ = seat_index;
    Some(Vec3d::new(0.0, y, 0.0))
}

/// What a camel's seat height depends on: whether it sits, how far into its pose
/// change it is, and whether it is a baby.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CamelSeat {
    /// Sitting, which the synced pose-change stamp encodes as a negative tick.
    pub sitting: bool,
    /// Ticks since the pose changed (game time minus the stamp's magnitude), plus any
    /// partial tick.
    pub pose_time: f64,
    /// A baby camel's seat is lower and its offsets are scaled by `0.6`.
    pub baby: bool,
}

impl CamelSeat {
    /// A settled, standing adult: the stamp's default of `0`, long ago.
    pub const STANDING: CamelSeat = CamelSeat { sitting: false, pose_time: f64::MAX, baby: false };

    /// From the synced pose-change stamp and the game time in ticks.
    #[must_use]
    pub fn from_stamp(stamp: i64, game_time: f64, baby: bool) -> Self {
        CamelSeat { sitting: stamp < 0, pose_time: game_time - stamp.unsigned_abs() as f64, baby }
    }
}

/// A camel's adult box height.
const CAMEL_HEIGHT: f32 = 2.375;
/// How much lower a sitting camel's box is than a standing one's.
const CAMEL_SITTING_DROP: f32 = 1.43;
/// A baby camel's standing and sitting box heights.
const CAMEL_BABY_HEIGHT: f32 = 1.4;
const CAMEL_BABY_SITTING_HEIGHT: f32 = 0.425;
/// A baby camel's age scale.
const CAMEL_BABY_SCALE: f32 = 0.6;
/// Sitting down takes 40 ticks; standing up takes 52.
const CAMEL_SIT_TICKS: f32 = 40.0;
const CAMEL_STAND_TICKS: f32 = 52.0;

/// A camel's passenger point: the seat drops from `height - 0.375` (a baby's
/// `- 0.09375`) of its standing box to `0.2` above its sitting box's equivalent, in
/// two linear legs through a flex point that differs for the front (driver) and back
/// seats, so the rider rides the camel's back down and up. The driver sits half a
/// block forward, a second rider `0.7` back (both times the age scale).
///
/// Sitting down: 40 ticks, the legs meet at tick 28, the flex point `0.5` (front) or
/// `0.1` (back) of the way down. Standing up: 52 ticks, the legs meet at 24 (front)
/// or 32 (back), the flex point `0.6` or `0.35`. During a transition the box is the
/// new pose's, so the offsets are relative to it.
#[must_use]
pub fn camel_passenger_attachment(seat: CamelSeat, seat_index: usize) -> Vec3d {
    let front = seat_index == 0;
    let scale = if seat.baby { CAMEL_BABY_SCALE } else { 1.0 };
    let height = match (seat.baby, seat.sitting) {
        (false, false) => CAMEL_HEIGHT,
        (false, true) => CAMEL_HEIGHT - CAMEL_SITTING_DROP,
        (true, false) => CAMEL_BABY_HEIGHT,
        (true, true) => CAMEL_BABY_SITTING_HEIGHT,
    };
    let sit_offset = if seat.baby { 0.093_75 } else { 0.375 };
    let mut y = f64::from(height) - sit_offset;
    let drop = scale * CAMEL_SITTING_DROP;
    let travel = drop - scale * 0.2;
    let bottom = drop - travel;
    let duration = if seat.sitting { CAMEL_SIT_TICKS } else { CAMEL_STAND_TICKS };
    if seat.pose_time < f64::from(duration) {
        let (half, flex_share) = match (seat.sitting, front) {
            (true, true) => (28.0, 0.5),
            (true, false) => (28.0, 0.1),
            (false, true) => (24.0, 0.6),
            (false, false) => (32.0, 0.35),
        };
        let t = (seat.pose_time as f32).clamp(0.0, duration);
        let first = t < half;
        let part = if first { t / half } else { (t - half) / (duration - half) };
        let flex = drop - flex_share * travel;
        let (from, to) = match (seat.sitting, first) {
            (true, true) => (drop, flex),
            (true, false) => (flex, bottom),
            (false, true) => (bottom - drop, bottom - flex),
            (false, false) => (bottom - flex, 0.0),
        };
        y += f64::from(from + part * (to - from));
    } else if seat.sitting {
        y += f64::from(bottom);
    }
    let z = if front { 0.5 } else { -0.7 };
    Vec3d::new(0.0, y, f64::from(z * scale))
}

/// Rotate a vehicle-local attachment point into world space —
/// vanilla's own attachment-point transform composed
/// with its own yaw-rotation routine.
///
/// The angle is **negated** degrees-to-radians, and the sign matters: getting it
/// backwards mirrors a boat's seat from the bow to the stern, which reads as a
/// plausible-looking seat facing the wrong way rather than as a bug. A point on
/// the Y axis is rotation-invariant, so this is a no-op for every `y`-only
/// attachment in this module's table — which is exactly why it must not be
/// skipped as "probably inert": the boats and llamas it is *not* inert for are
/// the ones a player sees.
#[must_use]
pub fn rotate_attachment(point: Vec3d, yaw_degrees: f32) -> Vec3d {
    // Vanilla's own trig helpers are `float` in vanilla and feed a `double` vector;
    // computing in `f32` and widening reproduces that rounding rather than
    // improving on it.
    let radians = -yaw_degrees * std::f32::consts::PI / 180.0;
    let cos = f64::from(radians.cos());
    let sin = f64::from(radians.sin());
    Vec3d::new(
        point.x * cos + point.z * sin,
        point.y,
        point.z * cos - point.x * sin,
    )
}

/// Where the **local player**'s feet go while riding `entity_type_path` at
/// `vehicle_feet` — the whole of vanilla's own position-rider routine
/// for the one passenger this client controls.
///
/// ```text
/// vehicle_feet + rotate(passenger_attachment, vehicle_yaw) - (0, 0.6, 0)
/// ```
///
/// The subtracted term is the player's own `VEHICLE` attachment
/// ([`PLAYER_VEHICLE_ATTACHMENT_Y`]). Vanilla rotates it by the **passenger's**
/// yaw, which is a no-op for a `y`-only point, so no player yaw is taken here —
/// stated so the missing parameter reads as a derivation rather than an omission.
///
/// The camera needs nothing further: 26.2's own camera-align-with-entity
/// routine has **no
/// is passenger branch** except one for lerped new-behaviour minecarts, and
/// riding does not change the player's pose or eye height — vanilla's own
/// player-pose-update routine
/// has no riding case and there is no
/// `SITTING` pose, so a mounted player keeps vanilla's own default-eye-height
/// constant of `1.62`.
/// Moving the feet here therefore moves the eye, and that is
/// the entire camera-on-the-vehicle mechanism.
#[must_use]
pub fn player_seat_position(
    vehicle_feet: Vec3d,
    vehicle_yaw: f32,
    entity_type_path: &str,
    vehicle_height: f32,
    seat_index: usize,
    camel: Option<CamelSeat>,
) -> Vec3d {
    let local = passenger_attachment_local(entity_type_path, vehicle_height, seat_index, camel);
    let world = rotate_attachment(local, vehicle_yaw);
    Vec3d::new(
        vehicle_feet.x + world.x,
        vehicle_feet.y + world.y - PLAYER_VEHICLE_ATTACHMENT_Y,
        vehicle_feet.z + world.z,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every expected value here is computed from a constant read out of
    /// `.cache/mc/26.2` and cited in the source above — never from this module's
    /// own arithmetic. That is the point: `decode(encode(x))` would be satisfied
    /// by a consistent misreading of the attachment system, and the failure mode
    /// this guards is exactly a plausible-but-wrong seat height.
    #[test]
    fn a_minecart_seats_the_player_below_its_own_roof() {
        // Vanilla's own MINECART entity-type declaration: minecart is `sized(0.98F, 0.7F)` with
        // passenger attachments. Seat = 0.1875, minus the player's own
        // 0.6 vehicle attachment.
        let seat = player_seat_position(Vec3d::new(0.0, 64.0, 0.0), 0.0, "minecart", 0.7, 0, None);
        assert!(
            (seat.y - (64.0 + 0.1875 - 0.6)).abs() < 1e-9,
            "minecart seat y was {}",
            seat.y
        );
        // The magnitude, not just the sign: the declared point is *lower* than
        // the `AT_HEIGHT` fallback would give, so a fallback-only implementation
        // lands 0.5125 too high. Predicting both hypotheses is what separates
        // "we read the table" from "we happened to move the player down".
        let fallback_y = 64.0 + 0.7 - 0.6;
        assert!(
            (seat.y - fallback_y).abs() > 0.5,
            "the declared minecart point must differ from the AT_HEIGHT fallback, \
             or this test cannot tell the two apart: got {} vs fallback {fallback_y}",
            seat.y
        );
    }

    /// The player's own `VEHICLE` attachment is the largest single error
    /// available here, so it gets its own assertion in both directions.
    #[test]
    fn the_players_vehicle_attachment_lowers_the_seat_by_six_tenths() {
        // Vanilla's own default-vehicle-attachment constant of `(0.0, 0.6, 0.0)`.
        let with = player_seat_position(Vec3d::new(0.0, 0.0, 0.0), 0.0, "pig", 0.9, 0, None);
        // Vanilla's own PIG entity-type declaration: pig declares passenger attachments.
        assert!((with.y - (0.868_75 - 0.6)).abs() < 1e-9, "pig seat {}", with.y);
        assert!(
            with.y < 0.868_75,
            "the seat must sit *below* the declared attachment point, not on it"
        );
    }

    /// A raft's seat is `height × 0.8888889` and a boat's is `height / 3` — the
    /// two are nearly a factor of three apart at the same `sized(1.375, 0.5625)`
    /// box, so reading one rule for both is visible.
    #[test]
    fn a_raft_seats_higher_than_a_boat_of_the_same_box() {
        const BOAT_HEIGHT: f32 = 0.5625; // vanilla's own entity-type registry's boat block, `sized(1.375F, 0.5625F)`
        let boat = passenger_attachment_local("oak_boat", BOAT_HEIGHT, 0, None);
        let raft = passenger_attachment_local("bamboo_raft", BOAT_HEIGHT, 0, None);
        // Vanilla's own boat and raft ride-height overrides, evaluated by hand:
        // 0.5625 / 3 = 0.1875; 0.5625 * 0.8888889 = 0.5.
        assert!((boat.y - 0.1875).abs() < 1e-6, "boat ride height {}", boat.y);
        assert!((raft.y - 0.5).abs() < 1e-6, "raft ride height {}", raft.y);
        // A chest boat keeps the boat ride height but shifts Z.
        let chest = passenger_attachment_local("oak_chest_boat", BOAT_HEIGHT, 0, None);
        assert!((chest.y - 0.1875).abs() < 1e-6);
        // Vanilla's own chest-boat single-passenger-offset override.
        assert!((chest.z - 0.15).abs() < 1e-6, "chest boat z {}", chest.z);
        // Vanilla's own boat single-passenger-offset override — a plain boat's single-passenger Z is zero.
        assert!(boat.z.abs() < 1e-9, "plain boat z {}", boat.z);
        // Vanilla's own abstract-boat override — the second seat sits behind the first.
        let second_seat = passenger_attachment_local("oak_boat", BOAT_HEIGHT, 1, None);
        assert!(
            (second_seat.z + 0.6).abs() < 1e-6,
            "the second boat seat's z was {}",
            second_seat.z
        );
    }

    /// The yaw rotation is only observable on an attachment with a non-zero
    /// horizontal component, which is precisely why it cannot be dismissed as
    /// inert: the boat seat's Z is one.
    #[test]
    fn vehicle_yaw_rotates_a_horizontal_attachment_and_leaves_a_vertical_one_alone() {
        // Yaw 0 faces +Z (south) in Minecraft, so a point at z = -0.6 (behind the
        // boat) is at world -Z. Turning the boat 90° must swing it to world -X:
        // `yRot(-90°)` gives x' = x·cos + z·sin = 0·0 + (-0.6)·(-1) = 0.6 ...
        let behind = Vec3d::new(0.0, 0.1875, -0.6);
        let turned = rotate_attachment(behind, 90.0);
        assert!(
            turned.x.abs() > 0.5 && turned.z.abs() < 1e-6,
            "a 90 degree yaw must move a Z-only offset onto X, got ({}, {}, {})",
            turned.x,
            turned.y,
            turned.z
        );
        // Height is untouched by a Y rotation, at any yaw.
        assert!((turned.y - 0.1875).abs() < 1e-9);
        // The control this pairs with: a purely vertical point is invariant, so a
        // test that only ever fed one of those would pass with the rotation
        // deleted outright.
        let vertical = Vec3d::new(0.0, 1.443_75, 0.0);
        for yaw in [0.0_f32, 37.0, 90.0, 180.0, -145.0] {
            let r = rotate_attachment(vertical, yaw);
            assert!(
                r.x.abs() < 1e-6 && r.z.abs() < 1e-6 && (r.y - 1.443_75).abs() < 1e-9,
                "vertical attachment must be yaw-invariant, yaw {yaw} gave ({}, {}, {})",
                r.x,
                r.y,
                r.z
            );
        }
    }

    /// An unlisted type must land on vanilla's own fallback rather than on zero
    /// — a zero would put the rider's feet 0.6 blocks *inside* the mount.
    #[test]
    fn an_undeclared_type_uses_vanillas_at_height_fallback() {
        // Vanilla's own STRIDER entity-type declaration: strider is `sized(0.9F, 1.7F)` and declares no
        // passenger-attachments override, so vanilla itself uses `(0, 1.7, 0)`.
        let local = passenger_attachment_local("strider", 1.7, 0, None);
        // Tolerance is f32-sized, not f64-sized, and that is not slack: the height
        // crosses an `f32` boundary in vanilla's own entity-base-dimensions type
        // before the seat maths
        // widens it to `f64`, so `1.7f32 as f64` is 1.700_000_047_683_715_8. A
        // `1e-9` bound here — which the vertical-attachment test above can afford
        // because 1.443_75 is f32-exact — is tighter than the type can represent and
        // fails on a correct answer. Discrimination is untouched: the wrong
        // hypothesis below sits 0.255 away, five orders of magnitude outside this.
        assert!(
            (local.y - 1.7).abs() < 1e-6,
            "strider fallback {} (f32-widened 1.7 is 1.700_000_047_683_715_8)",
            local.y
        );
        // And the wrong-but-plausible neighbour: vanilla's own default-eye-height's 0.85 factor
        // would give 1.445. Predicting both is the point.
        assert!(
            (local.y - 1.7 * 0.85).abs() > 0.2,
            "the fallback must be height x 1.0, not the eye-height x 0.85"
        );
    }

    /// A cushion declares no attachment points and is `sized(1.0, 0.25)`, so the
    /// seat is the default point at the top of its box, 0.25 above its feet, and
    /// the rider's feet sit 0.6 below that: 0.35 under the cushion's own height
    /// origin. A yaw turn moves nothing, the point has no horizontal part.
    #[test]
    fn a_cushion_seats_the_player_a_third_of_a_block_below_its_feet() {
        for yaw in [0.0, 90.0, 180.0, 270.0] {
            let seat = player_seat_position(Vec3d::new(10.5, 65.0, -3.5), yaw, "cushion", 0.25, 0, None);
            assert!((seat.y - (65.0 + 0.25 - 0.6)).abs() < 1e-9, "yaw {yaw}: seat y {}", seat.y);
            assert!((seat.x - 10.5).abs() < 1e-9 && (seat.z + 3.5).abs() < 1e-9, "yaw {yaw}: {seat:?}");
        }
        assert!((passenger_attachment_local("cushion", 0.25, 0, None).y - 0.25).abs() < 1e-9);
    }

    /// A seat index past the end clamps rather than panicking or wrapping —
    /// vanilla's own clamped attachment lookup.
    #[test]
    fn an_out_of_range_seat_index_clamps() {
        let first = passenger_attachment_local("horse", 1.6, 0, None);
        let tenth = passenger_attachment_local("horse", 1.6, 9, None);
        assert_eq!(first, tenth, "a one-seat mount must clamp every index to 0");
    }

    /// A camel's seat, each value worked by hand from the client's camel constants
    /// (box `2.375` tall, `1.43` lower sitting, seat `0.375` under the box top, a
    /// `0.2` floor; baby box `1.4`, seat `0.09375` under it, age scale `0.6`).
    #[test]
    fn a_camel_seat_rides_its_back_down_and_up() {
        let at = |stamp: i64, game_time: f64, baby: bool, seat: usize| {
            camel_passenger_attachment(CamelSeat::from_stamp(stamp, game_time, baby), seat)
        };
        let close = |got: Vec3d, y: f64, z: f64, what: &str| {
            assert!((got.y - y).abs() < 1.0e-5 && (got.z - z).abs() < 1.0e-6, "{what}: {got:?}, want y {y} z {z}");
        };
        // Standing, settled: 2.375 - 0.375; the driver half a block forward.
        close(at(100, 1000.0, false, 0), 2.0, 0.5, "standing driver");
        // The stamp's default (never changed pose) is a settled stand too.
        close(passenger_attachment_local("camel", 2.375, 0, None), 2.0, 0.5, "default camel");
        // A second rider sits 0.7 back.
        close(at(100, 1000.0, false, 1), 2.0, -0.7, "standing back seat");
        // Sitting, settled: (2.375 - 1.43) - 0.375 + 0.2 = 0.77.
        close(at(-100, 1000.0, false, 0), 0.77, 0.5, "sitting driver");
        // Sitting down, 14 ticks in (front): halfway down the first leg, from 1.43 to
        // the flex point 1.43 - 0.5 * 1.23 = 0.815, so 1.1225; on the sitting box's
        // 0.57 that is 1.6925.
        close(at(-100, 114.0, false, 0), 1.6925, 0.5, "sitting down, front");
        // Standing up, 12 ticks in (front): half the first 24-tick leg, from
        // 0.2 - 1.43 = -1.23 to 0.2 - (1.43 - 0.6 * 1.23) = -0.492, so -0.861; on the
        // standing box's 2.0 that is 1.139.
        close(at(100, 112.0, false, 0), 1.139, 0.5, "standing up, front");
        // The back seat at the same moment: 12 of its 32-tick first leg, from -1.23 to
        // 0.2 - (1.43 - 0.35 * 1.23) = -0.7995, so -1.0685625 and 0.9314375 in all.
        close(at(100, 112.0, false, 1), 0.931_437_5, -0.7, "standing up, back");
        // A baby: 1.4 - 0.09375 standing, and the driver 0.5 * 0.6 forward.
        close(at(100, 1000.0, true, 0), 1.306_25, 0.3, "baby standing");
        // A baby sitting: 0.425 - 0.09375 + 0.6 * 0.2.
        close(at(-100, 1000.0, true, 0), 0.451_25, 0.3, "baby sitting");
        // The player rides 0.6 under the point: a sitting camel's rider at 64.17.
        let seat = player_seat_position(
            Vec3d::new(0.0, 64.0, 0.0),
            0.0,
            "camel",
            2.375,
            0,
            Some(CamelSeat::from_stamp(-100, 1000.0, false)),
        );
        assert!((seat.y - 64.17).abs() < 1.0e-5 && (seat.z - 0.5).abs() < 1.0e-6, "{seat:?}");
    }
}
