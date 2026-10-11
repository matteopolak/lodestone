//! The riding model: one rider-to-vehicle relation for players and mobs.
//!
//! A rider is a player (by network id) or a mob; a vehicle is a boat, minecart,
//! cushion or mob. [`MobSim::riders`] holds the links in boarding order and
//! nothing else records who sits where, so a vehicle can carry a player and mobs
//! together and every reader sees the same passenger list.
//!
//! The first passenger controls the vehicle when it is a player: a ridden boat,
//! cart or mount is then steered by that player's client and skips its own tick
//! ([`MobSim::controlled_vehicles`]). A boarding player goes ahead of mobs unless
//! a player is already first.
//!
//! Interaction boarding checks the vehicle's capacity ([`MobSim::seat_capacity`]);
//! the forced boarding of `/ride` skips it and every kind-specific refusal.
//! [`MobSim::sync_riders`] runs each tick: it drops links whose ends are gone and
//! carries mob riders to their seat. [`MobSim::snapshots`] lists a vehicle's
//! passengers in [`EntitySnapshot::passengers`], and the entity streamer is the
//! only sender of the passenger-list packet.

use std::collections::HashSet;

use super::sim_command_entities::Located;
use super::*;

/// Height of a mob seat as a fraction of the mount's body.
const MOB_SEAT_FRACTION: f64 = 0.75;

/// Who boards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rider {
    /// The player with this network id.
    Player(i32),
    /// The live mob with this uuid.
    Mob(Uuid),
}

/// What a boarding attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountOutcome {
    /// The rider now rides network entity `vehicle`.
    Mounted { vehicle: i32 },
    /// The pairing is refused: already on it, a rider that cannot ride, a vehicle
    /// kind with no seat, or a seat cycle.
    Refused,
    /// No live entity has one of the uuids.
    Missing,
}

/// One rider on one vehicle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Ride {
    pub(super) rider: i32,
    pub(super) player: bool,
    pub(super) vehicle: i32,
}

impl<'w> MobSim<'w> {
    /// The network id of the vehicle `rider` sits on.
    #[must_use]
    pub fn vehicle_of(&self, rider: i32) -> Option<i32> {
        self.riders.iter().find(|r| r.rider == rider).map(|r| r.vehicle)
    }

    /// The riders on `vehicle`, in boarding order.
    #[must_use]
    pub fn riders_of(&self, vehicle: i32) -> Vec<i32> {
        self.riders.iter().filter(|r| r.vehicle == vehicle).map(|r| r.rider).collect()
    }

    /// The player steering `vehicle`: its first rider, when that is a player.
    #[must_use]
    pub fn controlling_player(&self, vehicle: i32) -> Option<i32> {
        self.riders.iter().find(|r| r.vehicle == vehicle).filter(|r| r.player).map(|r| r.rider)
    }

    /// Every vehicle a player steers.
    pub(super) fn controlled_vehicles(&self) -> HashSet<i32> {
        let mut seen = HashSet::new();
        let mut out = HashSet::new();
        for ride in &self.riders {
            if seen.insert(ride.vehicle) && ride.player {
                out.insert(ride.vehicle);
            }
        }
        out
    }

    /// `entity` and everything that rides it, directly or through other riders.
    #[must_use]
    pub fn self_and_passengers(&self, entity: i32) -> HashSet<i32> {
        let mut found = HashSet::from([entity]);
        let mut frontier = vec![entity];
        while let Some(next) = frontier.pop() {
            for rider in self.riders_of(next) {
                if found.insert(rider) {
                    frontier.push(rider);
                }
            }
        }
        found
    }

    /// How many riders `vehicle` takes by interaction: two for a boat or raft,
    /// one for a chest boat and for everything else.
    fn seat_capacity(&self, vehicle: i32) -> usize {
        match self.vehicles.get(&vehicle) {
            Some(boat) if !boat.entity_type.path().contains("chest") => 2,
            _ => 1,
        }
    }

    /// The seat origin of `id` and the rise of a mob rider above it; `None`
    /// unless `id` is a live entity kind that carries riders.
    fn seat_origin(&self, id: i32) -> Option<(Vec3, f64)> {
        if let Some(mob) = self.get(id) {
            return Some((mob.position(), f64::from(mob.shape().height) * MOB_SEAT_FRACTION));
        }
        let at = |p: lodestone_physics::Vec3d| Vec3::new(p.x, p.y, p.z);
        self.vehicles
            .get(&id)
            .map(|v| (at(v.motion.position), 0.0))
            .or_else(|| self.minecarts.get(&id).map(|c| (at(c.motion.position), 0.0)))
            .or_else(|| self.cushions.get(&id).map(|c| (c.position, 0.0)))
    }

    /// Seats `rider` on `vehicle`, leaving whatever it rode; a player goes ahead
    /// of the mobs unless a player already leads.
    fn seat(&mut self, vehicle: i32, rider: i32, player: bool) {
        self.riders.retain(|r| r.rider != rider);
        let ride = Ride { rider, player, vehicle };
        let first = self.riders.iter().position(|r| r.vehicle == vehicle);
        match first {
            Some(at) if player && !self.riders[at].player => self.riders.insert(at, ride),
            _ => self.riders.push(ride),
        }
    }

    /// Boards `rider` on `vehicle` unless that is refused: already aboard, a seat
    /// cycle, or (unless `force`) no capacity left.
    pub(super) fn board(&mut self, vehicle: i32, rider: i32, player: bool, force: bool) -> bool {
        if self.seat_origin(vehicle).is_none()
            || self.vehicle_of(rider) == Some(vehicle)
            || self.self_and_passengers(rider).contains(&vehicle)
            || (!force && self.riders_of(vehicle).len() >= self.seat_capacity(vehicle))
        {
            return false;
        }
        self.seat(vehicle, rider, player);
        true
    }

    /// Boards `rider` on the entity `vehicle` as `/ride` does: no capacity,
    /// sneaking or kind refusal, only the cycle and already-aboard ones.
    pub fn mount_forced(&mut self, vehicle: Uuid, rider: Rider) -> MountOutcome {
        let Some(located) = self.locate(vehicle) else { return MountOutcome::Missing };
        let (rider_id, player) = match rider {
            Rider::Player(id) => (id, true),
            Rider::Mob(uuid) => match self.locate(uuid) {
                Some(Located::Mob(id)) => (id, false),
                Some(_) => return MountOutcome::Refused,
                None => return MountOutcome::Missing,
            },
        };
        let vehicle_id = match located {
            Located::Vehicle(id) | Located::Minecart(id) | Located::Cushion(id) | Located::Mob(id) => id,
            _ => return MountOutcome::Refused,
        };
        if self.board(vehicle_id, rider_id, player, true) {
            MountOutcome::Mounted { vehicle: vehicle_id }
        } else {
            MountOutcome::Refused
        }
    }

    /// Lifts `rider` off its vehicle; the vehicle's network id.
    pub fn dismount(&mut self, rider: i32) -> Option<i32> {
        let vehicle = self.vehicle_of(rider)?;
        self.riders.retain(|r| r.rider != rider);
        Some(vehicle)
    }

    /// Lifts every rider off `vehicle`.
    pub(super) fn eject_riders(&mut self, vehicle: i32) {
        self.riders.retain(|r| r.vehicle != vehicle);
    }

    /// Drops player links whose player is not in the authoritative roster.
    pub(crate) fn reconcile_player_riders(&mut self, connected: &[i32]) {
        self.riders.retain(|r| !r.player || connected.contains(&r.rider));
        self.dismount_placements.retain(|(player, ..)| connected.contains(player));
    }

    /// Asks for `player` to be placed at `position` once their client has been
    /// told it left `vehicle`; the connection delivers it
    /// ([`Self::dismount_placement`]).
    pub fn queue_dismount_placement(&mut self, player: i32, vehicle: i32, position: Vec3, rotation: Rotation) {
        self.dismount_placements.retain(|(queued, ..)| *queued != player);
        self.dismount_placements.push((player, vehicle, position, rotation));
    }

    /// The queued `(vehicle, position, rotation)` placement for `player`.
    #[must_use]
    pub fn dismount_placement(&self, player: i32) -> Option<(i32, Vec3, Rotation)> {
        self.dismount_placements
            .iter()
            .find(|(queued, ..)| *queued == player)
            .map(|&(_, vehicle, position, rotation)| (vehicle, position, rotation))
    }

    /// Forgets the queued placement for `player`.
    pub fn take_dismount_placement(&mut self, player: i32) {
        self.dismount_placements.retain(|(queued, ..)| *queued != player);
    }

    /// The disconnect self-heal: a player who vanished without dismounting must
    /// not leave a vehicle frozen with a phantom occupant. An empty roster means
    /// "no information" (the list is empty before anyone has moved), not "nobody
    /// is connected", so it evicts no one.
    pub(super) fn clear_disconnected_riders(&mut self) {
        let connected: Vec<i32> =
            self.players.iter().filter_map(|p| p.identity.map(|identity| identity.entity_id)).collect();
        if !connected.is_empty() {
            self.reconcile_player_riders(&connected);
        }
    }

    /// Drops links whose rider or vehicle is gone, carries each mob rider to its
    /// vehicle's seat, and returns the mob riders (their goals must not run).
    pub(super) fn sync_riders(&mut self) -> HashSet<i32> {
        if self.riders.is_empty() {
            return HashSet::new();
        }
        let alive: HashSet<i32> = self.mobs.iter().map(|m| m.id).collect();
        let links = std::mem::take(&mut self.riders);
        self.riders = links
            .into_iter()
            .filter(|r| (r.player || alive.contains(&r.rider)) && self.seat_origin(r.vehicle).is_some())
            .collect();
        let mut carried = HashSet::new();
        for ride in self.riders.clone() {
            if ride.player {
                continue;
            }
            carried.insert(ride.rider);
            let Some((at, rise)) = self.seat_origin(ride.vehicle) else { continue };
            if let Some(mob) = self.get_mut(ride.rider) {
                let seat = Vec3::new(at.x, at.y + rise, at.z);
                if mob.position() != seat {
                    mob.teleport_to(seat);
                }
            }
        }
        carried
    }
}
