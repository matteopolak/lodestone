//! `MobSim`'s vehicle (boat) slice — spawn, mount/dismount, per-tick physics
//! against a real terrain oracle, and the query API. Moved out of
//! `mobs/mod.rs` verbatim as part of the `mobs.rs` file split (see
//! `docs/plans/crate-and-file-splits.md`). Zero visibility churn: every
//! `impl MobSim` method here was already `pub`, and `VehicleCollision` is
//! used only inside `tick_vehicles`, in this same file.

use lodestone_data::{block_states, collision_shapes};
use lodestone_physics::{CollisionView, EntityDimensions, Vec3d};
use lodestone_model::{ResourceKey, Vec3};
use uuid::Uuid;

use super::{MobSim, TrackedVehicle, block_state_id};

impl<'w> MobSim<'w> {
    /// Creates one `AbstractBoat` at `position` facing `yaw` and returns its
    /// network entity id — `level.addFreshEntity(boat)`.
    ///
    /// `entity_type` is a full boat/raft key; [`crate::boat`] is the only producer
    /// and validates the name against the entity registry before calling, so a
    /// wrong key cannot reach the wire here (where `entity_type_id(..).unwrap_or(0)`
    /// would silently encode `minecraft:acacia_boat`).
    ///
    /// **No AI, no attributes, no goals** — see [`TrackedVehicle`]. The boat
    /// streams on the next [`snapshots`](Self::snapshots) diff and is mountable
    /// immediately.
    pub fn spawn_vehicle(&mut self, entity_type: ResourceKey, position: Vec3, yaw: f32) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.vehicles.insert(
            id,
            TrackedVehicle {
                uuid: Uuid::new_v4(),
                entity_type,
                motion: lodestone_physics::EntityMotion::at(Vec3d::new(
                    position.x, position.y, position.z,
                )),
                yaw,
                boat: lodestone_physics::vehicle::BoatState::default(),
                rider: None,
            },
        );
        id
    }

    /// The entity type of a tracked vehicle, if `id` is one.
    ///
    /// This is also the **"is this entity a vehicle"** test a right-click
    /// dispatcher needs before it consults [`interact`](Self::interact), whose
    /// whole chain is `Animal.mobInteract` and has no arm for a boat.
    #[must_use]
    pub fn vehicle_type(&self, id: i32) -> Option<&ResourceKey> {
        self.vehicles.get(&id).map(|v| &v.entity_type)
    }

    /// A tracked vehicle's `(position, yaw)`.
    #[must_use]
    pub fn vehicle_transform(&self, id: i32) -> Option<(Vec3, f32)> {
        self.vehicles.get(&id).map(|v| {
            (
                Vec3::new(v.motion.position.x, v.motion.position.y, v.motion.position.z),
                v.yaw,
            )
        })
    }

    /// The controlling passenger's player entity id, if the vehicle is occupied.
    #[must_use]
    pub fn vehicle_rider(&self, id: i32) -> Option<i32> {
        self.vehicles.get(&id).and_then(|v| v.rider)
    }

    /// The vehicle `player_entity_id` is riding, if any.
    #[must_use]
    pub fn vehicle_ridden_by(&self, player_entity_id: i32) -> Option<i32> {
        self.vehicles
            .iter()
            .find(|(_, v)| v.rider == Some(player_entity_id))
            .map(|(&id, _)| id)
    }

    /// `AbstractBoat.interact` → `player.startRiding(this)`.
    ///
    /// Returns `true` when the player is now aboard, which is the caller's signal
    /// to send `SET_PASSENGERS`. Refuses — vanilla's `PASS` — when:
    ///
    /// * `id` is not a vehicle;
    /// * `using_secondary_action` is set (`player.isSecondaryUseActive()`, i.e.
    ///   sneak-clicking a boat does *not* board it);
    /// * the boat is out of control (`outOfControlTicks >= 60`, a fully submerged
    ///   hull);
    /// * someone else is already aboard. Vanilla's real limit is
    ///   `getMaxPassengers()` — **2** for a boat and **1** for a chest boat — and
    ///   this crate seats one for every type. A narrower gap than it looks: the
    ///   second seat needs a passenger *list* on the wire and a second seat
    ///   attachment, and seating two players in the same spot would be worse than
    ///   refusing.
    ///
    /// A player already riding something else is dismounted from it first, so a
    /// stale link cannot leave one player recorded in two boats.
    pub fn mount_vehicle(
        &mut self,
        id: i32,
        player_entity_id: i32,
        using_secondary_action: bool,
    ) -> bool {
        if using_secondary_action {
            return false;
        }
        let Some(vehicle) = self.vehicles.get(&id) else {
            return false;
        };
        if vehicle.rider.is_some_and(|rider| rider != player_entity_id) {
            return false;
        }
        // `!(this.outOfControlTicks < 60.0F)` — a capsized boat cannot be boarded.
        if vehicle.boat.out_of_control_ticks >= 60.0 {
            return false;
        }
        if let Some(previous) = self.vehicle_ridden_by(player_entity_id) {
            if previous != id {
                if let Some(old) = self.vehicles.get_mut(&previous) {
                    old.rider = None;
                }
            }
        }
        if let Some(vehicle) = self.vehicles.get_mut(&id) {
            vehicle.rider = Some(player_entity_id);
        }
        true
    }

    /// `Entity.stopRiding` for whatever `player_entity_id` is aboard, returning the
    /// vehicle it left.
    ///
    /// Called on disconnect as well as on an explicit dismount: a vehicle whose
    /// rider vanished must resume its own server-side tick, or it stays frozen
    /// mid-lake forever.
    pub fn dismount_rider(&mut self, player_entity_id: i32) -> Option<i32> {
        let id = self.vehicle_ridden_by(player_entity_id)?;
        if let Some(vehicle) = self.vehicles.get_mut(&id) {
            vehicle.rider = None;
        }
        Some(id)
    }

    /// Accepts a client-authoritative `MoveVehicle` for the vehicle
    /// `player_entity_id` is riding.
    ///
    /// Returns `true` if it was applied. It is refused when the player rides
    /// nothing, which is the guard that stops a connection moving a boat it is not
    /// in — vanilla's own `handleMoveVehicle` starts with
    /// `Entity rootVehicle = player.getRootVehicle(); if (rootVehicle == player) return;`.
    ///
    /// The velocity is **derived from the reported displacement**, not taken from
    /// the packet (there is no velocity field on the wire). That matters for the
    /// tick after a dismount: the boat carries on with the momentum the client
    /// last gave it rather than stopping dead, which is what
    /// `AbstractBoat.floatBoat`'s drag then bleeds off.
    ///
    /// No "moved too quickly" rejection is implemented, so
    /// [`ServerProtocol::encode_move_vehicle`](crate::protocol::ServerProtocol) has
    /// no producer — see `docs/boat-placement.md`. The client's
    /// `apply_vehicle_moved` handles the packet if one ever arrives.
    pub fn apply_vehicle_move(
        &mut self,
        player_entity_id: i32,
        position: Vec3,
        yaw: f32,
    ) -> Option<i32> {
        let id = self.vehicle_ridden_by(player_entity_id)?;
        let vehicle = self.vehicles.get_mut(&id)?;
        let next = Vec3d::new(position.x, position.y, position.z);
        vehicle.motion.velocity = next.subtract(vehicle.motion.position);
        vehicle.motion.position = next;
        vehicle.yaw = yaw;
        // The client's own boat state is authoritative while it rides, and ours is
        // stale by definition. Clearing the status forces the next unridden tick
        // through `floatBoat`'s classification rather than resuming from a status
        // latched before the player boarded.
        vehicle.boat.status = None;
        vehicle.boat.old_status = None;
        Some(id)
    }

    /// One tick of every **unridden** vehicle — `AbstractBoat.tick`'s
    /// buoyancy/drag half, without `controlBoat`.
    ///
    /// A ridden boat is skipped entirely, which is the handover: the moment a
    /// player boards, this stops touching the boat and
    /// [`apply_vehicle_move`](Self::apply_vehicle_move) becomes the only writer.
    /// Running both is what produces a boat that fights the player.
    ///
    /// `block_state` is the live world oracle, taken as a closure for the reason
    /// [`items_settled`](Self::items_settled) takes one: this sim holds
    /// `world: &ChunkWorld` and the collision shapes need the full block-state
    /// string, not the coarse solidity `ChunkWorld` answers.
    ///
    /// `float_boat` and `move_entity` come from [`lodestone_physics::vehicle`] —
    /// literally the same functions the client's `tick_controlled_vehicle` calls,
    /// so a boat cannot behave one way while watched and another while ridden.
    pub fn tick_vehicles(&mut self, block_state: &dyn Fn(i32, i32, i32) -> String) {
        use lodestone_physics::vehicle::{BOAT_STEP_HEIGHT, boat_status, float_boat};
        use lodestone_physics::{MoveContext, PhysicsProfile, move_entity};

        // **The disconnect self-heal.** A rider is cleared by an explicit
        // dismount, and a client that simply *vanishes* sends none — so without
        // this a boat whose rider crashed or quit stays `Some(id)` forever and is
        // skipped by every tick below, frozen mid-lake and unmountable by anyone.
        //
        // Guarded on a **non-empty** roster, which is the whole subtlety:
        // [`set_players`](Self::set_players) is refreshed from a movement packet,
        // so the list is legitimately empty before anyone has moved, and treating
        // that as "nobody is connected" would evict a rider the instant they
        // boarded. Empty means "no information", not "no players".
        if !self.players.is_empty() {
            let connected: Vec<i32> = self
                .players
                .iter()
                .filter_map(|p| p.identity.map(|identity| identity.entity_id))
                .collect();
            if !connected.is_empty() {
                for vehicle in self.vehicles.values_mut() {
                    if vehicle.rider.is_some_and(|rider| !connected.contains(&rider)) {
                        vehicle.rider = None;
                    }
                }
            }
        }

        let view = VehicleCollision { block_state };
        let profile = PhysicsProfile::default();
        let mut ids: Vec<i32> = self.vehicles.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(vehicle) = self.vehicles.get_mut(&id) else {
                continue;
            };
            if vehicle.rider.is_some() {
                continue;
            }
            let dims =
                EntityDimensions::new(crate::boat::BOAT_WIDTH as f32, crate::boat::BOAT_HEIGHT as f32, 0.0);
            let bb = dims.bounding_box(vehicle.motion.position);
            vehicle.boat.old_status = vehicle.boat.status;
            vehicle.boat.status = Some(boat_status(&mut vehicle.boat, &view, bb));
            if matches!(
                vehicle.boat.status,
                Some(
                    lodestone_physics::vehicle::BoatStatus::UnderWater
                        | lodestone_physics::vehicle::BoatStatus::UnderFlowingWater
                )
            ) {
                vehicle.boat.out_of_control_ticks += 1.0;
            } else {
                vehicle.boat.out_of_control_ticks = 0.0;
            }
            // `player_aboard = false`: the per-tick halving of `landFriction` is
            // gated on `getControllingPassenger() instanceof Player`, and there is
            // nobody aboard here by construction. Passing `true` would let a
            // beached empty boat slide off on its own.
            float_boat(&mut vehicle.motion, &mut vehicle.boat, dims, &view, false);
            let hull = EntityDimensions::new(dims.width, dims.height, BOAT_STEP_HEIGHT);
            move_entity(
                &mut vehicle.motion,
                hull,
                &view,
                &profile,
                MoveContext::default(),
            );
            vehicle.boat.last_yd = vehicle.motion.velocity.y;
        }
    }
}

/// A [`CollisionView`] for the vehicle tick: [`ItemCollision`]'s shapes plus the
/// three hooks a boat's buoyancy needs and a dropped item's settle does not.
///
/// `fluid_at` is the load-bearing addition. `AbstractBoat.getStatus` classifies
/// its surroundings from per-cell fluid **amount**, not from a boolean — the
/// difference between a source (`8/9` tall) and a flow (`1/9`..`7/9`) is the whole
/// of `waterLevel`, and with a coarse `is_water` every boat would compute a
/// surface `1/9` of a block off and sink slowly through deep water.
///
/// `friction` is the other one: `getGroundFriction` averages `Block.getFriction`
/// over the cells the hull touches, and it is what decides `ON_LAND` from
/// `IN_AIR`. Returning the trait's `0.6` default unconditionally would be right
/// for most blocks and would also classify **air** as land, which freezes a boat
/// in mid-fall.
struct VehicleCollision<'a> {
    block_state: &'a dyn Fn(i32, i32, i32) -> String,
}

impl VehicleCollision<'_> {
    /// The resolved block-state id at a cell, `None` outside the table.
    fn state_id(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        let name = (self.block_state)(x, y, z);
        block_state_id(&name).or_else(|| block_states::state_id(&name))
    }
}

impl CollisionView for VehicleCollision<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<lodestone_physics::Aabb>) {
        let Some(shape) = self.state_id(x, y, z).and_then(collision_shapes::collision_boxes) else {
            return;
        };
        let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
        for b in shape {
            out.push(lodestone_physics::Aabb::new(
                bx + f64::from(b.min[0]),
                by + f64::from(b.min[1]),
                bz + f64::from(b.min[2]),
                bx + f64::from(b.max[0]),
                by + f64::from(b.max[1]),
                bz + f64::from(b.max[2]),
            ));
        }
    }

    fn friction(&self, x: i32, y: i32, z: i32) -> f32 {
        // `Block.getFriction` is `0.6` for everything but ice (`0.98`), packed and
        // blue ice (`0.98`/`0.989`) and slime (`0.8`). Air has no friction *and no
        // collision*, and `getGroundFriction` only consults cells whose shape
        // actually touches the hull — so answering `0.6` for a shapeless cell is
        // unreachable rather than wrong, and the census read here is the honest
        // version either way.
        let name = (self.block_state)(x, y, z);
        // The block name without its `[…]` state properties — none of the four
        // slippery blocks has any, but a `ChunkSource` hands back canonical
        // states, so an unstripped compare would silently never match.
        let base = name.split_once('[').map_or(name.as_str(), |(base, _)| base);
        match base {
            "minecraft:ice" | "minecraft:frosted_ice" | "minecraft:packed_ice" => 0.98,
            "minecraft:blue_ice" => 0.989,
            "minecraft:slime_block" => 0.8,
            _ => 0.6,
        }
    }

    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        self.fluid_at(x, y, z)
            .is_some_and(|cell| cell.kind == lodestone_physics::fluid::FluidKind::Water)
    }

    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<lodestone_physics::fluid::FluidCell> {
        let name = (self.block_state)(x, y, z);
        let state = crate::fluid::fluid_state_of(&name)?;
        Some(lodestone_physics::fluid::FluidCell {
            kind: match state.kind {
                crate::fluid::FluidKind::Water => lodestone_physics::fluid::FluidKind::Water,
                crate::fluid::FluidKind::Lava => lodestone_physics::fluid::FluidKind::Lava,
            },
            amount: state.amount,
            falling: state.falling,
        })
    }
}

#[cfg(test)]
mod vehicle_tests {
    use super::*;
    use super::super::{ChunkWorld, PerceivedPlayer, PlayerIdentity, PlayerPerception};

    /// A stone seabed at `y = 60` and water at `y = 61..=63`, so a boat can float
    /// and a lake has a bottom. Everything above is air.
    fn lake() -> impl Fn(i32, i32, i32) -> String {
        |_x, y, _z| {
            if y <= 60 {
                "minecraft:stone".to_owned()
            } else if y <= 63 {
                "minecraft:water[level=0]".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        }
    }

    /// Same world as a [`ChunkWorld`], for the sim's own `world` borrow. The
    /// vehicle tick never reads it (it reads the closure), but `MobSim::new` needs
    /// one.
    fn world() -> ChunkWorld {
        ChunkWorld::new(-64, 384)
    }

    /// **Mounting, and the two refusals that make it mean something.**
    ///
    /// Sneak-clicking is the one with a visible symptom: without
    /// `player.isSecondaryUseActive()`, shift-right-clicking a boat with a block in
    /// hand boards it instead of placing, and there is no way to interact past a
    /// boat at all.
    #[test]
    fn a_boat_seats_one_player_and_refuses_a_sneak_click() {
        let world = world();
        let mut sim = MobSim::new(&world);
        let boat = sim.spawn_vehicle(
            "minecraft:oak_boat".parse().expect("a valid key"),
            Vec3::new(8.5, 63.4, 8.5),
            41.0,
        );
        let mut wrong = Vec::new();
        if sim.mount_vehicle(boat, 7, true) {
            wrong.push("a sneak-click must not board");
        }
        if sim.vehicle_rider(boat).is_some() {
            wrong.push("and must leave the boat empty");
        }
        if !sim.mount_vehicle(boat, 7, false) {
            wrong.push("an ordinary click boards");
        }
        if sim.vehicle_rider(boat) != Some(7) {
            wrong.push("and records the rider");
        }
        // A second player is refused: this crate seats one, and vanilla's
        // `getMaxPassengers` of 2 for a plain boat is a documented gap rather than
        // an accident. Seating two in the same spot would be worse than refusing.
        if sim.mount_vehicle(boat, 8, false) {
            wrong.push("an occupied boat refuses a second rider");
        }
        if sim.vehicle_rider(boat) != Some(7) {
            wrong.push("and keeps the one it has");
        }
        // A non-vehicle id is not a boat.
        if sim.mount_vehicle(boat + 500, 7, false) {
            wrong.push("an id that is not a vehicle cannot be boarded");
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// **The handover, which is the whole point of the vehicle registry.**
    ///
    /// While a player is aboard, the server must not move the boat: the client owns
    /// it (`Player.isClientAuthoritative()`), and a server that also simulated it
    /// would fight the player. Once the boat is empty the server's float pass takes
    /// over again.
    ///
    /// The discriminating input is a boat parked in **mid-air** at `y = 70`, so the
    /// two arms differ by a whole gravity step rather than by a hair: a floating
    /// boat's own drag makes a ridden-vs-unridden comparison at the water surface
    /// nearly coincident, which is exactly the shape that passes for both
    /// hypotheses. Mismatches are collected so a failure reports every arm rather
    /// than aborting at the first.
    #[test]
    fn a_ridden_boat_is_not_ticked_by_the_server_and_an_empty_one_is() {
        let world = world();
        let mut sim = MobSim::new(&world);
        let boat = sim.spawn_vehicle(
            "minecraft:oak_boat".parse().expect("a valid key"),
            Vec3::new(8.5, 70.0, 8.5),
            0.0,
        );
        assert!(sim.mount_vehicle(boat, 7, false));

        let before = sim.vehicle_transform(boat).expect("the boat exists");
        for _ in 0..5 {
            sim.tick_vehicles(&lake());
        }
        let after_ridden = sim.vehicle_transform(boat).expect("the boat exists");

        let mut wrong = Vec::new();
        if (after_ridden.0.y - before.0.y).abs() > 1e-12 {
            wrong.push(format!(
                "a ridden boat must not be moved by the server: {} -> {}",
                before.0.y, after_ridden.0.y
            ));
        }

        // Dismount, then the same five ticks. `AbstractBoat.getDefaultGravity()` is
        // 0.04, so five ticks of free fall move it by strictly more than one tick's
        // worth — the prediction is a floor derived from the constant rather than a
        // "did it move at all" sign check.
        assert_eq!(sim.dismount_rider(7), Some(boat));
        for _ in 0..5 {
            sim.tick_vehicles(&lake());
        }
        let after_empty = sim.vehicle_transform(boat).expect("the boat exists");
        let fall = before.0.y - after_empty.0.y;
        let one_step = f64::from(lodestone_physics::vehicle::BOAT_GRAVITY);
        if fall <= one_step {
            wrong.push(format!(
                "an empty boat in mid-air must fall by more than one {one_step}-block \
                 gravity step in five ticks, fell {fall}"
            ));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// **Steering: a `MoveVehicle` from the rider moves the boat, and one from
    /// anybody else does not.**
    ///
    /// The second arm is the security half and the one a "does the position update"
    /// gate cannot see: `apply_vehicle_move` resolves the vehicle from the *player*,
    /// which is vanilla's `getRootVehicle()` rule, so a connection cannot drag a
    /// boat it is not sitting in.
    ///
    /// The reported transform uses pairwise-distinct coordinates so a transposition
    /// of two of the three axes cannot survive, and a yaw that is neither `0` nor
    /// equal to any coordinate.
    #[test]
    fn only_the_rider_may_move_the_boat() {
        let world = world();
        let mut sim = MobSim::new(&world);
        let boat = sim.spawn_vehicle(
            "minecraft:bamboo_raft".parse().expect("a valid key"),
            Vec3::new(8.5, 63.4, 8.5),
            0.0,
        );
        assert!(sim.mount_vehicle(boat, 7, false));

        let mut wrong = Vec::new();
        if sim
            .apply_vehicle_move(8, Vec3::new(1.0, 2.0, 3.0), 90.0)
            .is_some()
        {
            wrong.push("a player who rides nothing must not move a boat".to_owned());
        }
        let reported = Vec3::new(11.25, 62.75, -4.5);
        if sim.apply_vehicle_move(7, reported, 137.0) != Some(boat) {
            wrong.push("the rider's report must be applied".to_owned());
        }
        let (position, yaw) = sim.vehicle_transform(boat).expect("the boat exists");
        if position != reported {
            wrong.push(format!("{position:?} != {reported:?}"));
        }
        if (yaw - 137.0).abs() > f32::EPSILON {
            wrong.push(format!("yaw {yaw} != 137"));
        }
        // And the wire carries it, which is what another viewer's `move_entity`
        // diff reads.
        let streamed = sim
            .snapshots()
            .into_iter()
            .find(|s| s.id == boat)
            .expect("a live boat must be streamed");
        if streamed.position != reported {
            wrong.push(format!("snapshot {:?} != {reported:?}", streamed.position));
        }
        if (streamed.rotation.yaw - 137.0).abs() > f32::EPSILON {
            wrong.push(format!("snapshot yaw {:?}", streamed.rotation));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// **The disconnect self-heal.** A rider who vanishes without dismounting must
    /// not freeze the boat forever.
    ///
    /// The control is the second arm: with an **empty** roster the rider is kept,
    /// because `set_players` is position-driven and legitimately empty before anyone
    /// has moved. Without that guard this eviction would fire the instant a player
    /// boarded, which is the failure direction that looks like "mounting does not
    /// work".
    #[test]
    fn a_rider_who_leaves_the_roster_is_evicted_and_an_empty_roster_is_not_evidence() {
        let world = world();

        let mut kept = MobSim::new(&world);
        let boat = kept.spawn_vehicle(
            "minecraft:oak_boat".parse().expect("a valid key"),
            Vec3::new(8.5, 70.0, 8.5),
            0.0,
        );
        assert!(kept.mount_vehicle(boat, 7, false));
        kept.tick_vehicles(&lake());
        assert_eq!(
            kept.vehicle_rider(boat),
            Some(7),
            "an empty roster means 'no information', not 'nobody is connected'"
        );

        let mut evicted = MobSim::new(&world);
        let boat = evicted.spawn_vehicle(
            "minecraft:oak_boat".parse().expect("a valid key"),
            Vec3::new(8.5, 70.0, 8.5),
            0.0,
        );
        assert!(evicted.mount_vehicle(boat, 7, false));
        // Somebody else is connected; player 7 is not.
        evicted.set_players(vec![PerceivedPlayer {
            identity: Some(PlayerIdentity {
                uuid: Uuid::new_v4(),
                entity_id: 12,
            }),
            perception: PlayerPerception {
                position: Vec3::new(8.5, 64.0, 8.5),
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }]);
        evicted.tick_vehicles(&lake());
        assert_eq!(
            evicted.vehicle_rider(boat),
            None,
            "a rider absent from a non-empty roster has gone"
        );
    }
}
