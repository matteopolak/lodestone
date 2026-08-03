//! Where a passenger sits — 26.2's data-driven **entity attachment** system,
//! ported as a pure function so it can be unit-tested with no `World`, no
//! server and no version adapter.
//!
//! # The rule, read out of the real 26.2 tree
//!
//! Every citation below is `.cache/mc/26.2/src/net/minecraft/…`.
//!
//! `Entity.rideTick()` (`world/entity/Entity.java:2385-2391`) zeroes the
//! passenger's velocity and then hands positioning to the **vehicle**:
//!
//! ```text
//! Entity.positionRider(passenger, moveFunction)          // Entity.java:2399-2403
//!   position = this.getPassengerRidingPosition(passenger)
//!   offset   = passenger.getVehicleAttachmentPoint(this)
//!   passenger.setPos(position - offset)
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
//! * `getPassengerRidingPosition` = `position() + getPassengerAttachmentPoint(...)`
//!   (`Entity.java:2412-2414`; `LivingEntity.java:3981-3982` overrides it only to
//!   pass pose dimensions and a scale).
//! * `getPassengerAttachmentPoint` →
//!   `attachments.getClamped(EntityAttachment.PASSENGER, index, vehicle.yRot)`
//!   (`Entity.java:2416-2423`), where `index` is
//!   `vehicle.getPassengers().indexOf(passenger)`.
//! * `getVehicleAttachmentPoint` = `attachments.get(VEHICLE, 0, this.yRot)`
//!   (`Entity.java:2408-2410`) — always index `0`, and rotated by the
//!   **passenger's** own yaw.
//! * the rotation is `point.yRot(-rotY * π/180)`
//!   (`EntityAttachments.java:78-80`), i.e. `Vec3.yRot`
//!   (`world/phys/Vec3.java:241-248`): `x' = x·cos + z·sin`, `z' = z·cos − x·sin`.
//!
//! # Two constants that are easy to get wrong, and were checked
//!
//! * **The `PASSENGER` fallback is `(0, height, 0)` — `height × 1.0`, the *top* of
//!   the vehicle's box.** `EntityAttachment.java:7` declares
//!   `PASSENGER(Fallback.AT_HEIGHT)` and `EntityAttachment.java:25` is
//!   `AT_HEIGHT = (width, height) -> List.of(new Vec3(0.0, height, 0.0))`. The
//!   tempting `height × 0.85` is a *different* quantity —
//!   `EntityDimensions.defaultEyeHeight` (`EntityDimensions.java:11-13`) — and
//!   using it here would sink every unlisted mount by 15% of its height.
//! * **The player's `VEHICLE` attachment is not zero.** `VEHICLE`'s fallback *is*
//!   `AT_FEET = Vec3.ZERO` (`EntityAttachment.java:8`, `:24`), but the player
//!   declares one explicitly: `Avatar.DEFAULT_VEHICLE_ATTACHMENT =
//!   new Vec3(0.0, 0.6, 0.0)` (`world/entity/Avatar.java:17`), wired in at
//!   `EntityTypes.java:1143` and baked into `Avatar.STANDING_DIMENSIONS`
//!   (`Avatar.java:21-23`). It is **subtracted**, so it lowers the player 0.6
//!   below the seat point. Dropping it would float the rider 0.6 blocks above
//!   every saddle — the single largest error available in this function.
//!
//! Both are [`PASSENGER_HEIGHT_FACTOR`] and [`PLAYER_VEHICLE_ATTACHMENT_Y`].
//!
//! # Why this is a small table and not the whole registry
//!
//! ~70 entity types declare an explicit `PASSENGER` point in `EntityTypes.java`.
//! Transcribing all of them by hand here is the mistake `CLAUDE.md` names twice
//! over — the right home is a jar-generated table in `lodestone-data`, next to
//! `entity_dimensions`, produced by the same `Bootstrap.bootStrap()` walk the
//! collision-shape and hardness censuses use. That is filed as follow-up work.
//!
//! What is here instead is the **general rule** plus the handful of types a
//! *player* can actually be a passenger of, each transcribed with its
//! `EntityTypes.java` line. Everything else falls through to vanilla's own
//! `AT_HEIGHT` fallback, computed from the real generated height — so an unlisted
//! mount is a few centimetres high rather than structurally wrong, which is the
//! benign direction. See [`passenger_attachment_local`].
//!
//! # What is deliberately not modelled
//!
//! Each of these is a *per-instance animation* on top of the static point, not a
//! different rule, and each needs state this crate does not hold:
//!
//! * `AbstractHorse.java:1041-1044` adds
//!   `(0, 0.15·standAnim, −0.7·standAnim)` while the horse rears — `standAnimO`
//!   is a client-side animation clock with no wire field.
//! * `Strider.java:190-200` adds `0.12·cos(walkPos·1.5)·2·min(0.25, walkSpeed)`
//!   — the walk-animation bob, which is *explicitly* client-cosmetic (the server
//!   returns plain `super` at `:191-193`).
//! * `Camel.java:495-512` replaces the point entirely with a sit/stand
//!   interpolation (`SITTING_HEIGHT_DIFFERENCE = 1.43`, 40-tick sit / 52-tick
//!   stand). A camel therefore uses its `AT_HEIGHT` fallback here, which is its
//!   standing seat to within the sit animation's range.
//! * `AbstractMinecart.java:178-182` lowers the point to `ZERO` for villagers and
//!   wandering traders only — never for a player.
//! * the second boat seat's `Animal` nudge (`AbstractBoat.java:146-148`).

use lodestone_physics::Vec3d;

/// Vanilla's `EntityAttachment.Fallback.AT_HEIGHT` factor
/// (`EntityAttachment.java:25`): the default `PASSENGER` point sits at the
/// **full** height of the vehicle's box.
///
/// Named rather than inlined because the plausible-but-wrong neighbour —
/// `EntityDimensions.defaultEyeHeight`'s `0.85`
/// (`EntityDimensions.java:11-13`) — is a real constant in the same file family,
/// and the two are indistinguishable at a glance in a call site.
pub const PASSENGER_HEIGHT_FACTOR: f64 = 1.0;

/// `Avatar.DEFAULT_VEHICLE_ATTACHMENT.y` (`Avatar.java:17`), the player's own
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
/// `EntityAttachments.getClamped` (`EntityAttachments.java:68-76`,
/// `Mth.clamp(index, 0, size - 1)`) — a third rider on a two-seat vehicle
/// silently shares the last seat there too, and `getClamped` is what the
/// passenger path calls (the throwing `get`/`getNullable` are used elsewhere).
///
/// # The boat family bypasses the attachment table entirely
///
/// `AbstractBoat.getPassengerAttachmentPoint` (`AbstractBoat.java:135-152`) never
/// consults `dimensions.attachments()`; it builds the point from an abstract
/// `rideHeight(dimensions)` and a **Z** (forward/back) offset:
///
/// ```text
/// offset = getSinglePassengerXOffset()                    // 0.0, or 0.15 for chest boats
/// if passengers.size() > 1 { offset = if index == 0 { 0.2 } else { -0.6 } }
/// Vec3(0, rideHeight(dimensions), offset)
/// ```
///
/// `rideHeight` is `height / 3.0` for boats and chest boats
/// (`Boat.java:15-17`, `ChestBoat.java:15-17`) and `height × 0.8888889` for rafts
/// and chest rafts (`Raft.java:15-17`, `ChestRaft.java:15-17`). At the shared
/// `sized(1.375, 0.5625)` (`EntityTypes.java:149-156`) that is `0.1875` and
/// `0.5` respectively — so a raft seat is nearly three times higher than a
/// boat's, and reading one rule for both is a visible error.
///
/// Note the field is named `getSinglePassengerXOffset` and is applied to **Z**.
/// That is vanilla's own naming inconsistency, reproduced deliberately: the
/// citation only matches the source if the name is kept.
#[must_use]
pub fn passenger_attachment_local(entity_type_path: &str, height: f32, seat_index: usize) -> Vec3d {
    let height = f64::from(height);
    // The boat family, in `path()` form. `is_raft` before `is_boat` would be
    // wrong the other way round, so both are matched by suffix on the *whole*
    // path: every wood variant is `<wood>_boat` / `<wood>_raft` /
    // `<wood>_chest_boat` / `<wood>_chest_raft` (`EntityTypes.java:149-207`).
    let raft = entity_type_path.ends_with("raft");
    let boat = raft || entity_type_path.ends_with("boat");
    if boat {
        let ride_height = if raft {
            // `Raft.java:16` — a `float` literal in vanilla, widened here.
            height * f64::from(0.888_888_9_f32)
        } else {
            // `Boat.java:16` — `dimensions.height() / 3.0F`.
            height / 3.0
        };
        let chest = entity_type_path.contains("chest_");
        let z = if seat_index == 0 {
            // One-passenger case and the front seat of a two-passenger boat
            // differ: `AbstractChestBoat.java:43-45` returns `0.15` where
            // `AbstractBoat.java:612-614` returns `0.0`, but a *shared* boat
            // overrides both with `0.2` for index 0. We do not know the other
            // seats' occupancy here — the caller does, through `Passengers` —
            // and passing that in for a difference of 0.05 blocks is not worth
            // the extra parameter, so the single-passenger value is used. Noted
            // rather than hidden.
            if chest { 0.15 } else { 0.0 }
        } else {
            // `AbstractBoat.java:143` — every seat past the first.
            -0.6
        };
        return Vec3d::new(0.0, ride_height, z);
    }
    match declared_passenger_attachment(entity_type_path, seat_index) {
        Some(point) => point,
        // Vanilla's `AT_HEIGHT` fallback, from the real generated height.
        None => Vec3d::new(0.0, height * PASSENGER_HEIGHT_FACTOR, 0.0),
    }
}

/// The explicitly-declared `PASSENGER` points for the types a **player** can
/// ride, each with its `EntityTypes.java` line.
///
/// `None` means "this type declares none here", which the caller turns into
/// vanilla's `AT_HEIGHT` fallback — the same answer vanilla gives for a type that
/// genuinely declares none. So a type missing from this table is
/// *approximately* right rather than wrong-shaped; see the module docs on why
/// the full ~70-row registry belongs in a generated `lodestone-data` table
/// instead of here.
fn declared_passenger_attachment(entity_type_path: &str, seat_index: usize) -> Option<Vec3d> {
    // Every value below is `passengerAttachments(y)`, i.e. `(0, y, 0)` —
    // `EntityType.Builder.passengerAttachments(float...)`, `EntityType.java:516-521`.
    let y = match entity_type_path {
        // `EntityTypes.java:665-668`, shared by every minecart variant: chest,
        // furnace, hopper, tnt, spawner and command-block minecarts all repeat
        // `.sized(0.98F, 0.7F).passengerAttachments(0.1875F)` verbatim
        // (`:284-287`, `:468-471`, `:525-528`, `:964-967`, `:889-892`,
        // `:302-305`), so one arm covers the family. Note it is far *below* the
        // `0.7` box top the fallback would give — a minecart seat is inside the
        // cart, not on its roof.
        "minecart"
        | "chest_minecart"
        | "furnace_minecart"
        | "hopper_minecart"
        | "tnt_minecart"
        | "spawner_minecart"
        | "command_block_minecart" => 0.1875,
        // `EntityTypes.java:531`.
        "horse" => 1.443_75,
        // `EntityTypes.java:338`.
        "donkey" => 1.112_5,
        // `EntityTypes.java:675`.
        "mule" => 1.212_5,
        // `EntityTypes.java:852` and `:1104` — the same value, declared twice.
        "skeleton_horse" | "zombie_horse" => 1.318_75,
        // `EntityTypes.java:754`.
        "pig" => 0.868_75,
        // `EntityTypes.java:620` and `:973` — the only rideable-adjacent entry
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

/// Rotate a vehicle-local attachment point into world space —
/// `EntityAttachments.transformPoint` (`EntityAttachments.java:78-80`) composed
/// with `Vec3.yRot` (`world/phys/Vec3.java:241-248`).
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
    // `Mth.cos`/`Mth.sin` are `float` in vanilla and feed a `double` vector;
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
/// `vehicle_feet` — the whole of `Entity.positionRider`
/// (`Entity.java:2399-2403`) for the one passenger this client controls.
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
/// The camera needs nothing further: 26.2's `Camera.alignWithEntity`
/// (`client-src/net/minecraft/client/Camera.java:246-264`) has **no
/// `isPassenger()` branch** except one for lerped new-behaviour minecarts, and
/// riding does not change the player's pose or eye height — `Player.updatePlayerPose`
/// (`world/entity/player/Player.java:343-357`) has no riding case and there is no
/// `SITTING` pose, so a mounted player keeps `Avatar.DEFAULT_EYE_HEIGHT = 1.62`
/// (`Avatar.java:16`). Moving the feet here therefore moves the eye, and that is
/// the entire camera-on-the-vehicle mechanism.
#[must_use]
pub fn player_seat_position(
    vehicle_feet: Vec3d,
    vehicle_yaw: f32,
    entity_type_path: &str,
    vehicle_height: f32,
    seat_index: usize,
) -> Vec3d {
    let local = passenger_attachment_local(entity_type_path, vehicle_height, seat_index);
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
        // `EntityTypes.java:667`: minecart is `sized(0.98F, 0.7F)` with
        // `passengerAttachments(0.1875F)`. Seat = 0.1875, minus the player's own
        // 0.6 vehicle attachment.
        let seat = player_seat_position(Vec3d::new(0.0, 64.0, 0.0), 0.0, "minecart", 0.7, 0);
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
        // `Avatar.java:17`: `DEFAULT_VEHICLE_ATTACHMENT = new Vec3(0.0, 0.6, 0.0)`.
        let with = player_seat_position(Vec3d::new(0.0, 0.0, 0.0), 0.0, "pig", 0.9, 0);
        // `EntityTypes.java:754`: pig declares `passengerAttachments(0.86875F)`.
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
        const BOAT_HEIGHT: f32 = 0.5625; // `EntityTypes.java:151`
        let boat = passenger_attachment_local("oak_boat", BOAT_HEIGHT, 0);
        let raft = passenger_attachment_local("bamboo_raft", BOAT_HEIGHT, 0);
        // `Boat.java:16` / `Raft.java:16`, evaluated by hand:
        // 0.5625 / 3 = 0.1875; 0.5625 * 0.8888889 = 0.5.
        assert!((boat.y - 0.1875).abs() < 1e-6, "boat ride height {}", boat.y);
        assert!((raft.y - 0.5).abs() < 1e-6, "raft ride height {}", raft.y);
        // A chest boat keeps the boat ride height but shifts Z.
        let chest = passenger_attachment_local("oak_chest_boat", BOAT_HEIGHT, 0);
        assert!((chest.y - 0.1875).abs() < 1e-6);
        // `AbstractChestBoat.java:44`.
        assert!((chest.z - 0.15).abs() < 1e-6, "chest boat z {}", chest.z);
        // `AbstractBoat.java:613` — a plain boat's single-passenger Z is zero.
        assert!(boat.z.abs() < 1e-9, "plain boat z {}", boat.z);
        // `AbstractBoat.java:143` — the second seat sits behind the first.
        let second_seat = passenger_attachment_local("oak_boat", BOAT_HEIGHT, 1);
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
        // `EntityTypes.java:941`: strider is `sized(0.9F, 1.7F)` and declares no
        // `passengerAttachments`, so vanilla itself uses `(0, 1.7, 0)`.
        let local = passenger_attachment_local("strider", 1.7, 0);
        assert!((local.y - 1.7).abs() < 1e-9, "strider fallback {}", local.y);
        // And the wrong-but-plausible neighbour: `defaultEyeHeight`'s 0.85 factor
        // would give 1.445. Predicting both is the point.
        assert!(
            (local.y - 1.7 * 0.85).abs() > 0.2,
            "the fallback must be height x 1.0, not the eye-height x 0.85"
        );
    }

    /// A seat index past the end clamps rather than panicking or wrapping —
    /// `EntityAttachments.getClamped` (`EntityAttachments.java:74`).
    #[test]
    fn an_out_of_range_seat_index_clamps() {
        let first = passenger_attachment_local("horse", 1.6, 0);
        let tenth = passenger_attachment_local("horse", 1.6, 9);
        assert_eq!(first, tenth, "a one-seat mount must clamp every index to 0");
    }
}
