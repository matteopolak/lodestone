//! Mobs that ride another entity.
//!
//! A vehicle's own rider slot holds a player's network id and nothing else, so a
//! mob rider lives in a separate ordered link table, `(rider, vehicle)` by
//! network id. Any vehicle kind with a seat (boat, minecart, cushion, mob) can
//! carry any number of mob riders, and a rider may itself be a vehicle.
//!
//! Each tick [`MobSim::sync_mob_riders`] drops links whose ends no longer exist,
//! carries every rider to its vehicle, and returns the riders so their goal
//! tick is skipped. [`MobSim::snapshots`] lists a vehicle's mob riders in
//! [`EntitySnapshot::passengers`], which the entity streamer sends as a
//! passenger-list packet whenever it changes. A vehicle that also carries a
//! player is refused (the player's own connection sends a different id for
//! itself), and links are not saved.

use std::collections::HashSet;

use super::sim_command_entities::Located;
use super::*;

/// Height of a mob seat as a fraction of the mount's body.
const MOB_SEAT_FRACTION: f64 = 0.75;

/// Why a mob rider could not board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobMountOutcome {
    /// The rider now rides network entity `vehicle`.
    Mounted { vehicle: i32 },
    /// The rider or vehicle exists but this pairing is not modelled: the rider
    /// is not a mob, the vehicle has no seat, or a player already sits on it.
    Refused,
    /// No live entity has one of the uuids.
    Missing,
}

impl<'w> MobSim<'w> {
    /// The network id of the entity `rider` (a mob id) sits on.
    #[must_use]
    pub fn mob_rider_vehicle(&self, rider: i32) -> Option<i32> {
        self.mob_riders.iter().find(|&&(r, _)| r == rider).map(|&(_, v)| v)
    }

    /// The mobs riding `vehicle`, in boarding order.
    #[must_use]
    pub fn mob_riders_of(&self, vehicle: i32) -> Vec<i32> {
        self.mob_riders.iter().filter(|&&(_, v)| v == vehicle).map(|&(r, _)| r).collect()
    }

    /// `entity` and everything that rides it, directly or through other riders.
    #[must_use]
    pub fn self_and_passengers(&self, entity: i32) -> HashSet<i32> {
        let mut found = HashSet::from([entity]);
        let mut frontier = vec![entity];
        while let Some(next) = frontier.pop() {
            for rider in self.mob_riders_of(next) {
                if found.insert(rider) {
                    frontier.push(rider);
                }
            }
        }
        found
    }

    /// Seats the mob `rider` on the entity `vehicle`, leaving whatever it rode.
    pub fn mount_mob_rider(&mut self, vehicle: Uuid, rider: Uuid) -> MobMountOutcome {
        let (Some(vehicle), Some(Located::Mob(rider_id))) = (self.locate(vehicle), self.locate(rider)) else {
            return MobMountOutcome::Missing;
        };
        let vehicle_id = match vehicle {
            Located::Vehicle(id) | Located::Minecart(id) | Located::Cushion(id) | Located::Mob(id) => id,
            _ => return MobMountOutcome::Refused,
        };
        let player_aboard = match vehicle {
            Located::Vehicle(id) => self.vehicle_rider(id).is_some(),
            Located::Minecart(id) => self.minecart_rider(id).is_some(),
            Located::Cushion(id) => self.cushion_rider(id).is_some(),
            _ => self.mob_rider(vehicle_id).is_some(),
        };
        if player_aboard || self.self_and_passengers(rider_id).contains(&vehicle_id) {
            return MobMountOutcome::Refused;
        }
        self.mob_riders.retain(|&(r, _)| r != rider_id);
        self.mob_riders.push((rider_id, vehicle_id));
        MobMountOutcome::Mounted { vehicle: vehicle_id }
    }

    /// Lifts the mob `rider` off its vehicle; the vehicle's network id.
    pub fn dismount_mob_rider(&mut self, rider: i32) -> Option<i32> {
        let vehicle = self.mob_rider_vehicle(rider)?;
        self.mob_riders.retain(|&(r, _)| r != rider);
        Some(vehicle)
    }

    /// Whether a player sits on the entity `vehicle`.
    #[must_use]
    pub fn has_player_rider(&self, vehicle: i32) -> bool {
        self.vehicle_rider(vehicle).is_some()
            || self.minecart_rider(vehicle).is_some()
            || self.cushion_rider(vehicle).is_some()
            || self.mob_rider(vehicle).is_some()
    }

    /// Drops links whose rider or vehicle is gone, carries each remaining rider
    /// to its vehicle's seat, and returns the riders (their goals must not run).
    pub(super) fn sync_mob_riders(&mut self) -> HashSet<i32> {
        if self.mob_riders.is_empty() {
            return HashSet::new();
        }
        let seats: HashMap<i32, (Vec3, f64)> = self
            .snapshots()
            .into_iter()
            .map(|s| {
                let rise = self.get(s.id).map_or(0.0, |m| f64::from(m.shape().height) * MOB_SEAT_FRACTION);
                (s.id, (s.position, rise))
            })
            .collect();
        let alive: HashSet<i32> = self.mobs.iter().map(|m| m.id).collect();
        self.mob_riders.retain(|(rider, vehicle)| alive.contains(rider) && seats.contains_key(vehicle));
        for &(rider, vehicle) in &self.mob_riders.clone() {
            let (at, rise) = seats[&vehicle];
            if let Some(mob) = self.get_mut(rider) {
                let seat = Vec3::new(at.x, at.y + rise, at.z);
                if mob.position() != seat {
                    mob.teleport_to(seat);
                }
            }
        }
        self.mob_riders.iter().map(|&(rider, _)| rider).collect()
    }
}
