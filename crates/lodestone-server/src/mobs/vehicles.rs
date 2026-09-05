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

/// `VehicleEntity.hurtServer`'s `setHurtTime(10)` — how long the hull rocks
/// after a hit, in ticks. The client's roll formula reads the same counter
/// *twice* (inside its sine and as a linear falloff), so this number sets both
/// the duration and the number of swings.
const VEHICLE_HURT_TICKS: i32 = 10;

/// `VehicleEntity.hurtServer`'s destruction threshold on accumulated damage.
/// Used here as a **clamp** rather than as a trigger — see
/// [`MobSim::attack_vehicle`] for why this crate does not destroy the vehicle.
const VEHICLE_DESTROY_DAMAGE: f32 = 40.0;

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
                paddle_left: false,
                paddle_right: false,
                // `VehicleEntity.defineSynchedData`'s registered defaults, and
                // the `1` is the one that matters -- see `TrackedVehicle::hurt_dir`.
                hurt_time: 0,
                hurt_dir: 1,
                damage: 0.0,
            },
        );
        id
    }

    /// `VehicleEntity.hurtServer` — the whole of what a punch does to a boat,
    /// raft or minecart: flip the rock direction, restart the ten-tick clock and
    /// add the damage that scales the rock's amplitude.
    ///
    /// Called from [`MobSim::attack_from_player`], which routes here before its
    /// generic mob pipeline because a vehicle lives in its own map and has no
    /// health, armour, knockback or gossip for that pipeline to touch. The
    /// returned [`AttackOutcome`] reports zero health and no kill: nothing
    /// downstream reads a vehicle's health, and reporting a kill would have the
    /// caller announce a death for an entity that is still afloat.
    ///
    /// # Two clauses of vanilla's method are deliberately not here
    ///
    /// `hurtServer` destroys the vehicle once accumulated damage passes `40.0`
    /// (or immediately, for a creative-mode attacker) and drops its item. This
    /// crate has no boat-item drop path reachable from the mob sim, so
    /// destroying here would delete a player's boat with nothing to pick up.
    /// The damage is **clamped** at that same `40.0` instead: the rock amplitude
    /// stops growing exactly where vanilla's does, and the boat survives. That is
    /// a known gap, not an approximation of destruction.
    ///
    /// `isInvulnerableToBase` is likewise not consulted — nothing in this crate
    /// marks a vehicle invulnerable.
    pub(super) fn attack_vehicle(&mut self, target_id: i32, raw_damage: f32) -> Option<super::AttackOutcome> {
        let vehicle = self.vehicles.get_mut(&target_id)?;
        // `setHurtDir(-getHurtDir())` first, then `setHurtTime(10)`. The negation
        // is what makes a second punch tip the hull the *other* way; dropping it
        // leaves every hit rocking the same direction, which reads as a stutter
        // rather than as a swing.
        vehicle.hurt_dir = -vehicle.hurt_dir;
        vehicle.hurt_time = VEHICLE_HURT_TICKS;
        // `setDamage(getDamage() + damage * 10.0F)`. The x10 is vanilla's, and it
        // is why a one-heart punch produces a visible rock at all: the renderer
        // divides by ten again.
        vehicle.damage = (vehicle.damage + raw_damage * 10.0).min(VEHICLE_DESTROY_DAMAGE);
        Some(super::AttackOutcome {
            health: 0.0,
            killed: false,
            damage_dealt: raw_damage,
            velocity: Vec3::new(0.0, 0.0, 0.0),
        })
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

    /// Vanilla `AbstractBoat.getDismountLocationForPassenger` for one tracked
    /// boat, evaluated against the live chunk source the connection is using.
    ///
    /// The preferred point is one collision-width outside the hull in the
    /// passenger's facing direction. Vanilla tries the floor in that cell, then
    /// the floor one cell below, across the player's dismount poses; if neither
    /// fits (or the lower cell is water), `Entity`'s fallback is the centre of
    /// the boat's top face. Returns `None` only when `id` is not a tracked boat.
    #[must_use]
    pub fn vehicle_dismount_position(
        &self,
        id: i32,
        passenger_yaw: f32,
        block_state: &dyn Fn(i32, i32, i32) -> String,
    ) -> Option<Vec3> {
        let vehicle = self.vehicles.get(&id)?;
        Some(boat_dismount_position(
            vehicle.motion.position,
            passenger_yaw,
            block_state,
        ))
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

    /// Accepts a `ServerboundPaddleBoatPacket` for the vehicle
    /// `player_entity_id` is riding — purely cosmetic bookkeeping for
    /// [`snapshots`](Self::snapshots)'s `MetadataField::BoatPaddles`, refused
    /// the same way [`apply_vehicle_move`](Self::apply_vehicle_move) is when
    /// the reporting player rides nothing.
    ///
    /// The rider's own client never reads this back — `controlBoat`'s paddle
    /// animation is driven by the rider's own local input, not by a
    /// server-streamed field — so this only ever matters to a *second*
    /// connected player watching the boat from outside.
    pub fn apply_boat_paddle(&mut self, player_entity_id: i32, left: bool, right: bool) -> Option<i32> {
        let id = self.vehicle_ridden_by(player_entity_id)?;
        let vehicle = self.vehicles.get_mut(&id)?;
        vehicle.paddle_left = left;
        vehicle.paddle_right = right;
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
            // `AbstractBoat.tick`'s first two clauses, and they run **before** the
            // ridden-boat bail below rather than after it: a rider's client owns
            // the boat's *motion*, not its damage state, so a boat punched while
            // someone is aboard must still count its rock down or it stays tipped
            // over for as long as that player keeps sitting in it.
            if vehicle.hurt_time > 0 {
                vehicle.hurt_time -= 1;
            }
            if vehicle.damage > 0.0 {
                vehicle.damage -= 1.0;
            }
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
    /// The validated block-state id at a cell, `None` outside the table.
    fn state_id(&self, x: i32, y: i32, z: i32) -> Option<block_states::StateId> {
        let name = (self.block_state)(x, y, z);
        block_state_id(&name)
    }
}

impl CollisionView for VehicleCollision<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<lodestone_physics::Aabb>) {
        let Some(state) = self.state_id(x, y, z) else {
            return;
        };
        let shape = collision_shapes::collision_boxes(state);
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

/// The block-floor rule shared by both target cells in vanilla's boat dismount
/// search (`BlockGetter.getBlockFloorHeight`).
fn block_floor_height(view: &dyn CollisionView, x: i32, y: i32, z: i32) -> f64 {
    let top = view.collision_top(x, y, z);
    if top > 0.0 {
        return top;
    }
    let below_top = view.collision_top(x, y - 1, z);
    if below_top >= 1.0 {
        below_top - 1.0
    } else {
        f64::NEG_INFINITY
    }
}

fn boat_dismount_position(
    boat_position: Vec3d,
    passenger_yaw: f32,
    block_state: &dyn Fn(i32, i32, i32) -> String,
) -> Vec3 {
    const BOAT_HEIGHT: f64 = 0.5625;

    // `Entity.getCollisionHorizontalEscapeVector`: every trigonometric and max
    // operation is `float` in vanilla before widening into the returned Vec3.
    let collider_width = f64::from(1.375f32 * std::f32::consts::SQRT_2);
    let colliding_width = f64::from(0.6f32);
    let distance = (collider_width
        + colliding_width
        + f64::from(1.0E-5f32))
        / 2.0;
    let radians = passenger_yaw * (std::f64::consts::PI / 180.0) as f32;
    let direction_x = -radians.sin();
    let direction_z = radians.cos();
    let scale = direction_x.abs().max(direction_z.abs());
    let target_x = boat_position.x + f64::from(direction_x) * distance / f64::from(scale);
    let target_z = boat_position.z + f64::from(direction_z) * distance / f64::from(scale);
    let target_y = (boat_position.y + BOAT_HEIGHT).floor() as i32;
    let target_block_x = target_x.floor() as i32;
    let target_block_z = target_z.floor() as i32;
    let view = VehicleCollision { block_state };

    if !view.is_water(target_block_x, target_y - 1, target_block_z) {
        let mut targets = Vec::with_capacity(2);
        let target_floor = block_floor_height(&view, target_block_x, target_y, target_block_z);
        if target_floor.is_finite() && target_floor < 1.0 {
            targets.push(Vec3d::new(
                target_x,
                f64::from(target_y) + target_floor,
                target_z,
            ));
        }
        let below_floor = block_floor_height(
            &view,
            target_block_x,
            target_y - 1,
            target_block_z,
        );
        if below_floor.is_finite() && below_floor < 1.0 {
            targets.push(Vec3d::new(
                target_x,
                f64::from(target_y - 1) + below_floor,
                target_z,
            ));
        }

        // `Player.getDismountPoses`: standing, crouching, then swimming.
        for dims in [
            EntityDimensions::PLAYER,
            EntityDimensions::new(0.6, 1.5, 0.6),
            EntityDimensions::new(0.6, 0.6, 0.6),
        ] {
            for target in &targets {
                if lodestone_physics::collision::no_collision(
                    &view,
                    dims.bounding_box(*target),
                ) {
                    return Vec3::new(target.x, target.y, target.z);
                }
            }
        }
    }

    // `Entity.getDismountLocationForPassenger`: the vehicle centre at maxY.
    Vec3::new(
        boat_position.x,
        boat_position.y + BOAT_HEIGHT,
        boat_position.z,
    )
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

    #[test]
    fn boat_dismount_prefers_the_passenger_facing_side_and_falls_back_to_the_deck() {
        let floor = |_x: i32, y: i32, _z: i32| {
            if y <= 7 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        };
        let preferred = boat_dismount_position(Vec3d::new(8.0, 8.0, 8.0), 0.0, &floor);
        assert_eq!(preferred.x, 8.0);
        assert_eq!(preferred.y, 8.0);
        assert!(
            preferred.z > 9.27 && preferred.z < 9.28,
            "yaw zero escapes toward +Z by the combined collision widths: {preferred:?}"
        );

        let blocked = |_x: i32, y: i32, _z: i32| {
            if y <= 8 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        };
        assert_eq!(
            boat_dismount_position(Vec3d::new(8.0, 8.0, 8.0), 0.0, &blocked),
            Vec3::new(8.0, 8.5625, 8.0),
            "when neither side floor is below one block, Entity's deck fallback wins"
        );
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

    /// **Punching a boat writes `VehicleEntity`'s hurt triple, it reaches the
    /// streamed snapshot, and the tick counts it back down.**
    ///
    /// This is the wiring proof for the whole rocking animation, and the arm
    /// that would have failed before this branch: `attack_from_player`'s generic
    /// pipeline reads `self.mobs`, a vehicle lives in `self.vehicles`, so a
    /// punch on a boat found nothing, returned `None`, and wrote no state at all
    /// — the client's renderer had nothing to read however correct it was.
    ///
    /// The damage is `2.5`, not a whole number, so the `x10` cannot be confused
    /// with the raw damage and the `-1.0` per-tick decay cannot be confused with
    /// a reset to zero.
    #[test]
    fn punching_a_boat_writes_the_hurt_triple_and_the_tick_decays_it() {
        let world = world();
        let mut sim = MobSim::new(&world);
        let boat = sim.spawn_vehicle(
            "minecraft:oak_boat".parse().expect("a valid key"),
            Vec3::new(8.5, 63.4, 8.5),
            0.0,
        );

        let mut wrong = Vec::new();
        let hurt_of = |sim: &MobSim<'_>, id: i32| {
            sim.snapshots()
                .into_iter()
                .find(|s| s.id == id)
                .and_then(|s| {
                    s.metadata.into_iter().find_map(|f| match f {
                        crate::protocol::MetadataField::VehicleHurt { time, dir, damage } => {
                            Some((time, dir, damage))
                        }
                        _ => None,
                    })
                })
        };

        // `defineSynchedData`'s own defaults reach the wire, and the direction's
        // is `1` rather than `0` -- the client multiplies the roll by it.
        match hurt_of(&sim, boat) {
            Some((0, 1, d)) if d == 0.0 => {}
            other => wrong.push(format!("resting triple was {other:?}, expected (0, 1, 0.0)")),
        }

        sim.attack_from_player(
            boat,
            None,
            Vec3::new(8.5, 63.4, 6.0),
            2.5,
            crate::mobs::DamageFlags::default(),
            0.0,
        )
        .map_or_else(
            || Some("attacking a boat must report an outcome".to_owned()),
            |outcome| {
                outcome
                    .killed
                    .then(|| "a punched boat must not be reported killed".to_owned())
            },
        )
        .map(|complaint| wrong.push(complaint));

        // `setHurtDir(-getHurtDir())`, `setHurtTime(10)`,
        // `setDamage(getDamage() + damage * 10)`.
        match hurt_of(&sim, boat) {
            Some((10, -1, d)) if (d - 25.0).abs() < f32::EPSILON => {}
            other => wrong.push(format!("after one hit the triple was {other:?}, expected (10, -1, 25.0)")),
        }

        // One tick of `AbstractBoat.tick`: the clock and the damage each fall by
        // one. A reset to zero, or a decay of only one of the two, both leave a
        // visibly wrong animation and both look like "it decays" from here.
        sim.tick_vehicles(&|_, _, _| "minecraft:air".to_owned());
        match hurt_of(&sim, boat) {
            Some((9, -1, d)) if (d - 24.0).abs() < f32::EPSILON => {}
            other => wrong.push(format!("after one tick the triple was {other:?}, expected (9, -1, 24.0)")),
        }

        // A second hit flips the direction back, which is what makes consecutive
        // punches rock the hull alternately rather than stutter one way.
        sim.attack_from_player(
            boat,
            None,
            Vec3::new(8.5, 63.4, 6.0),
            1.0,
            crate::mobs::DamageFlags::default(),
            0.0,
        );
        match hurt_of(&sim, boat) {
            Some((10, 1, d)) if (d - 34.0).abs() < f32::EPSILON => {}
            other => wrong.push(format!("after the second hit the triple was {other:?}, expected (10, 1, 34.0)")),
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

    /// **Paddle input: only the rider may set it, and it reaches the streamed
    /// snapshot.** The second half of
    /// `only_the_rider_may_move_the_boat`'s security property, plus the
    /// wiring proof that the boolean pair actually shows up in
    /// `EntitySnapshot::metadata` rather than being stored and never read.
    #[test]
    fn only_the_rider_may_set_the_paddle_state_and_it_reaches_the_snapshot() {
        let world = world();
        let mut sim = MobSim::new(&world);
        let boat = sim.spawn_vehicle(
            "minecraft:oak_boat".parse().expect("a valid key"),
            Vec3::new(8.5, 63.4, 8.5),
            0.0,
        );
        assert!(sim.mount_vehicle(boat, 7, false));

        let mut wrong = Vec::new();
        if sim.apply_boat_paddle(8, true, false).is_some() {
            wrong.push("a player who rides nothing must not set the paddle state".to_owned());
        }
        // Pairwise-distinct: left true, right false, so a transposition of
        // the two booleans cannot survive the round trip.
        if sim.apply_boat_paddle(7, true, false) != Some(boat) {
            wrong.push("the rider's report must be applied".to_owned());
        }
        let streamed = sim
            .snapshots()
            .into_iter()
            .find(|s| s.id == boat)
            .expect("a live boat must be streamed");
        let field = streamed
            .metadata
            .iter()
            .find(|f| matches!(f, crate::protocol::MetadataField::BoatPaddles { .. }));
        match field {
            Some(crate::protocol::MetadataField::BoatPaddles { left, right }) => {
                if !*left || *right {
                    wrong.push(format!("snapshot paddles left={left} right={right}, expected true/false"));
                }
            }
            _ => wrong.push("no BoatPaddles metadata field in the snapshot".to_owned()),
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
