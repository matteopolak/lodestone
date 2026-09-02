//! The client is authoritative over the vehicle it rides, and this is where that
//! authority is exercised.
//!
//! # The blocker this removes
//!
//! Vanilla's own client-authoritative check delegates to the controlling passenger, and
//! for a player passenger it is always `true`, so a ridden vehicle's server-side
//! ridden-travel step takes the zero-out-movement branch and
//! the server only **accepts** what we report. Every riding-shaped item on the
//! wire was therefore stuck behind one missing thing: a local simulation.
//!
//! `ClientAction::MoveVehicle` and `ClientAction::PaddleBoat` were already
//! encoded byte-exactly by the v26-2 adapter with **zero producers** — the same
//! shape as `ClientAction::SetFlying`, which got this client kicked for flying.
//! [`send_vehicle_actions`] is the producer.
//!
//! # The tick, and where each piece sits
//!
//! ```text
//! GameTick
//!   TickSet::Physics   charge_riding_jump      (the horse jump ramp + the falling edge)
//!                      player_physics          (unchanged: the rider drifts, as vanilla's does)
//!                      tick_controlled_vehicle (the vehicle actually moves)
//!                      pin_passenger_to_vehicle (unchanged: the rider snaps to the seat)
//!   TickSet::Send      send_vehicle_actions    (MoveVehicle, and PaddleBoat for a boat)
//!
//! NetIngest
//!   IngestSet::Apply   apply_vehicle_moved     (the server's rejection snap)
//! ```
//!
//! Ordering `tick_controlled_vehicle` **before** the seat pin is the whole
//! mechanism by which riding reaches pixels: the pin reads the vehicle's
//! [`Position`](crate::entity::Position), so a vehicle moved after the pin would
//! carry the camera one tick late, and one moved before it carries the camera
//! with it in the same tick.
//!
//! # Why `VehicleMoved` folds in `ingest` and not in `session`
//!
//! The event carries no entity id, which makes it *look* like a local-player
//! scalar. It is not: the two things it changes are the vehicle's
//! [`Position`](crate::entity::Position) and
//! [`Rotation`](crate::entity::Rotation) — per-entity components, keyed by server
//! id, written by no other router. `session::Riding` supplies the id, exactly as
//! [`crate::player::pin_passenger_to_vehicle`] already resolves the vehicle
//! through `EntityIndex` from that same scalar. The rule
//! `docs/event-routing.md` states is about *what is written*, not about what the
//! packet happens to carry, and what is written here is per-entity state.
//!
//! Concretely, `session` could not do the job: no session fold has the
//! `Query<&mut Position>` this needs, and adding one would put a second writer on
//! a component `ingest::apply_entity_movement` owns.
//!
//! # What the correction actually is
//!
//! `ClientboundMoveVehiclePacket` is **not** a periodic sync. The server sends it
//! from exactly two places in `ServerGamePacketListenerImpl.handleMoveVehicle` —
//! a "moved too quickly" rejection and a "moved wrongly / collided with something
//! new" rejection — and both are followed by `vehicle.absSnapTo(old…)`. So
//! receiving one means *our prediction was refused*, and the right response is to
//! discard the local motion and restart from the server's position, which is what
//! [`apply_vehicle_moved`] does.

use bevy_ecs::prelude::{Query, Res, ResMut, With};
use bevy_ecs::resource::Resource;
use lodestone_entity::attribute::{attribute_value, movement_speed_key};
use lodestone_model::{ClientAction, ClientEvent, PlayerCommand, Vec3};
use lodestone_physics::vehicle::{
    BoatInput, BoatState, HORSE_BASE_JUMP_STRENGTH, MountRule, PIG_RIDDEN_SPEED_FACTOR,
    STRIDER_RIDDEN_SPEED_FACTOR, clamp_rider_yaw, jump_riding_scale, ridden_speed, tick_boat,
    tick_ridden_mount,
};
use lodestone_physics::{EntityDimensions, EntityMotion, Vec3d};

use crate::entity::{Attributes, EntityIndex, EntityKind, OnGround, Position, Rotation};
use crate::ingest::IngestBatch;
use crate::player::{
    ActionQueue, Egress, LocalPlayer, MovementIntent, PhysicsState, PlayerCollision, Profile,
};
use crate::session::{Riding, ServerEntityId};

/// The canonical id of vanilla's `jump_strength` attribute — a mount's
/// `Attributes.JUMP_STRENGTH`, which `AbstractHorse.getJumpPower` multiplies by
/// the charge scale.
fn jump_strength_key() -> lodestone_model::Identifier {
    use std::str::FromStr as _;
    lodestone_model::Identifier::from_str("minecraft:jump_strength")
        .expect("valid built-in identifier")
}

/// Which of the two vanilla rules a vehicle follows.
///
/// Keyed off the entity type's `ResourceKey` **path**, the only identity that
/// survives ingest — the same key [`crate::riding::passenger_attachment_local`]
/// switches on, and matched the same way (by suffix) so every wood variant is
/// covered by one arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VehicleFamily {
    /// Vanilla's own boat base class — boats, chest boats, rafts, chest rafts.
    /// Never runs its own travel method; see
    /// [`lodestone_physics::vehicle::tick_boat`].
    Boat,
    /// A living entity steered through vanilla's own ridden-travel method —
    /// horse, donkey, mule,
    /// skeleton/zombie horse, pig, strider, camel.
    LandMount,
}

impl VehicleFamily {
    /// The family for an entity type path, or `None` for a type this client does
    /// not know how to simulate.
    ///
    /// **Default-deny, and the minecart is the reason.** An unrecognised type
    /// returns `None` and is left entirely to the server rather than being
    /// simulated under a guessed rule. A minecart is deliberately absent: its
    /// motion is rail-following (vanilla's own minecart behaviour class), the
    /// server broadcasts it
    /// through its own move-minecart packet, and treating it as a land mount
    /// would fight that with plain gravity.
    #[must_use]
    pub fn for_type_path(path: &str) -> Option<Self> {
        if path.ends_with("boat") || path.ends_with("raft") {
            return Some(Self::Boat);
        }
        mount_rule(path).map(|_| Self::LandMount)
    }
}

/// Which of vanilla's own horse-base/pig/strider/camel overrides a land
/// mount uses, or `None` for a type this client will not simulate.
///
/// **Vanilla's own horse-base rule is not universal, and this table is the
/// whole reason
/// there is a switch rather than one arm.** Vanilla's own pig and strider override
/// their own ridden-input method to a constant forward vector — they are
/// steered by the mouse
/// alone — and scale their speed down by `0.225` and `0.55`. Reading one rule for
/// all of them gives a pig you can strafe and reverse, moving 4.4× too fast, and
/// nothing about the call site would look wrong.
///
/// The list is also this module's whole notion of "a type we simulate", so
/// [`VehicleFamily::for_type_path`] derives its land-mount answer from here rather
/// than repeating the names — two lists would be a place for them to disagree.
#[must_use]
pub fn mount_rule(path: &str) -> Option<MountRule> {
    match path {
        // `AbstractHorse` itself, unoverridden. Llamas are not player-rideable in
        // vanilla, but a caravan makes one a vehicle, and being wrong about a type
        // we can never be seated on costs nothing while being *silent* about it
        // would leave a hole if that ever changed.
        "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" | "llama"
        | "trader_llama" => Some(MountRule::Horse),
        "pig" => Some(MountRule::Steered {
            speed_factor: PIG_RIDDEN_SPEED_FACTOR,
        }),
        "strider" => Some(MountRule::Steered {
            speed_factor: STRIDER_RIDDEN_SPEED_FACTOR,
        }),
        "camel" => Some(MountRule::Camel),
        _ => None,
    }
}

/// The local simulation of the one vehicle we control, or `None` when on foot or
/// riding something we do not simulate.
///
/// # A resource, not a component on the vehicle
///
/// There is exactly one controlled vehicle per client — vanilla's own
/// client-side controlled-vehicle accessor returns a single reference — so a component
/// would need inserting and removing through `Commands` on every mount and
/// dismount, and a stale one left on a vehicle we stopped riding would keep
/// simulating it. A resource cannot go stale: [`tick_controlled_vehicle`] rebuilds
/// it the moment [`Riding`] names a different id.
#[derive(Resource, Debug, Clone, Default)]
pub struct ControlledVehicle(pub Option<ControlledVehicleState>);

/// One fixed-tick endpoint of a locally controlled vehicle's render transform.
///
/// Physics remains entirely in [`ControlledVehicleState::motion`] and advances
/// only from [`tick_controlled_vehicle`]. This copy exists so a renderer can
/// sample the last completed tick at frame cadence without either integrating
/// motion again or making the physics result depend on frame rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VehicleRenderPose {
    /// The vehicle's feet/origin position.
    pub position: Vec3d,
    /// Body yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
}

/// The per-tick state [`ControlledVehicle`] holds.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlledVehicleState {
    /// The vehicle's server entity id, so a mount onto a *different* vehicle is
    /// detected rather than inherited.
    pub server_id: i32,
    /// Which rule set drives it.
    pub family: VehicleFamily,
    /// Position, velocity and the collision flags — the same
    /// [`EntityMotion`] the player pipeline threads through
    /// [`lodestone_physics::move_entity`], so a boat and a player cannot disagree
    /// about a slab.
    pub motion: EntityMotion,
    /// The vehicle's own yaw, which vanilla's own boat-control routine writes
    /// and a land mount copies
    /// from its rider.
    pub yaw: f32,
    /// The vehicle's own pitch. A boat never changes it; a land mount takes
    /// **half** its rider's.
    pub pitch: f32,
    /// The authoritative pose at the start of the most recently completed
    /// fixed tick. Render code samples from this to [`Self::current_pose`] with
    /// [`crate::FrameClock::interp_alpha`]; it never mutates simulation state.
    pub previous: VehicleRenderPose,
    /// Vanilla's own boat base class's between-tick state. Carried for a land mount too (unused,
    /// at its [`Default`]) rather than making the whole struct an enum — the two
    /// families share every other field and an enum would double every read site.
    pub boat: BoatState,
    /// This tick's paddle-state pair, for `ClientAction::PaddleBoat`.
    pub paddles: (bool, bool),
}

impl ControlledVehicleState {
    /// The authoritative endpoint produced by the latest fixed physics tick.
    #[must_use]
    pub fn current_pose(&self) -> VehicleRenderPose {
        VehicleRenderPose {
            position: self.motion.position,
            yaw: self.yaw,
            pitch: self.pitch,
        }
    }

    /// Discards local prediction after the server rejects a vehicle move.
    fn reset_to(&mut self, pose: VehicleRenderPose) {
        self.motion = EntityMotion::at(pose.position);
        self.yaw = pose.yaw;
        self.pitch = pose.pitch;
        self.previous = pose;
        self.boat = BoatState::default();
        self.paddles = (false, false);
    }
}

/// The rider's jump-key charge, mirroring vanilla's own local-player
/// riding-jump-ticks and riding-jump-scale fields.
///
/// On the local player rather than on the vehicle because that is where vanilla
/// keeps it: the charge survives a dismount-and-remount, and it is the *rider*
/// who is holding the key.
#[derive(bevy_ecs::component::Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct RidingJumpCharge {
    /// Vanilla's own riding-jump-ticks field. Negative is the 10-tick cooldown after a release.
    pub ticks: i32,
    /// Vanilla's own riding-jump-scale field, the `0.0..=1.0` charge.
    pub scale: f32,
    /// The previous tick's jump key, vanilla's own "was jumping" local.
    pub was_jumping: bool,
    /// Vanilla's own horse-base "player jump pending scale" field — set on the
    /// release edge and spent
    /// by the next grounded vehicle tick.
    pub pending: Option<f32>,
}

/// `TickSet::Physics`, **before** [`tick_controlled_vehicle`]: advance the horse
/// jump charge and send `PlayerCommand::StartRidingJump` on the jump key's
/// **falling** edge.
///
/// This is vanilla's own local-player AI-step's "jumpable vehicle" block, and
/// it runs before
/// travel there too — which is what lets the same tick's vehicle tick spend the
/// impulse.
///
/// # `STOP_RIDING_JUMP` is never sent
///
/// It exists on the wire and in vanilla's own serverbound-player-command
/// action enum, and the
/// vanilla client has no "send riding stop" call at all: only "send riding
/// jump" exists,
/// and vanilla's own horse-base "handle stop jump" is empty. Sending it would be a packet
/// vanilla never emits, so it is deliberately absent rather than forgotten.
///
/// # Why the charge runs even when the vehicle cannot jump
///
/// It does not: vanilla gates the whole block on
/// its own "jumpable vehicle" check being non-null and the jump cooldown
/// being zero, where the "jumpable vehicle" check
/// requires "can jump" — "is saddled" for a horse. This client cannot see the
/// saddle (it is entity metadata we do not decode for equines), so the gate here
/// is the weaker "we control a land mount". The consequence is a
/// `START_RIDING_JUMP` sent for an unsaddled mount, which the server's
/// `AbstractHorse.onPlayerJump` discards under its own `isSaddled()` check — the
/// safe direction, and named here rather than left as a silent divergence.
pub fn charge_riding_jump(
    egress: Res<Egress>,
    vehicle: Res<ControlledVehicle>,
    mut queue: ResMut<ActionQueue>,
    mut players: Query<(&MovementIntent, &mut RidingJumpCharge, &ServerEntityId), With<LocalPlayer>>,
) {
    let jumpable = matches!(
        vehicle.0.as_ref().map(|v| v.family),
        Some(VehicleFamily::LandMount)
    );
    for (intent, mut charge, own_id) in &mut players {
        if !jumpable {
            // `else { this.jumpRidingScale = 0.0F; }` — the charge is dropped
            // outright while there is nothing to jump, so a key held on foot does
            // not arm a jump for the next mount.
            charge.scale = 0.0;
            charge.pending = None;
            charge.was_jumping = intent.0.jump;
            continue;
        }
        let jumping = intent.0.jump;
        let was_jumping = charge.was_jumping;

        if charge.ticks < 0 {
            charge.ticks += 1;
            if charge.ticks == 0 {
                charge.scale = 0.0;
            }
        }

        if was_jumping && !jumping {
            // The release edge. `jumpRidingTicks = -10` is the cooldown latch.
            charge.ticks = -10;
            let boost = (charge.scale * 100.0).floor() as i32;
            charge.pending = Some(
                lodestone_physics::vehicle::player_jump_pending_scale(boost),
            );
            if egress.in_world && egress.live && let Some(entity_id) = own_id.0 {
                queue.0.push(ClientAction::PlayerCommand {
                    entity_id,
                    command: PlayerCommand::StartRidingJump { boost },
                });
            }
        } else if !was_jumping && jumping {
            charge.ticks = 0;
            charge.scale = 0.0;
        } else if was_jumping {
            charge.ticks += 1;
            charge.scale = jump_riding_scale(charge.ticks);
        }
        charge.was_jumping = jumping;
    }
}

/// `TickSet::Physics`, after [`crate::player::player_physics`] and **before**
/// [`crate::player::pin_passenger_to_vehicle`]: one tick of the vehicle we
/// control, written straight back onto its ECS components.
///
/// # Every reason this declines, and why each is the safe direction
///
/// * not riding, or riding something [`VehicleFamily::for_type_path`] does not
///   know — the server keeps its authority and the vehicle simply follows its
///   broadcast position;
/// * no [`PlayerCollision::View`] — no terrain to collide against, so a
///   simulation would sink the vehicle through absent ground and get us snapped
///   back;
/// * no [`crate::VersionData`] adapter, or the adapter does not know the type —
///   the vehicle's box height is unavailable and a *guessed* box changes both
///   buoyancy (`waterLevel - y) / bbHeight`) and where the rider sits. Same
///   default-deny rule the seat pin already documents;
/// * a land mount whose `minecraft:movement_speed` the server has not reported —
///   see [`mount_speed`].
pub fn tick_controlled_vehicle(
    collision: Res<PlayerCollision>,
    profile: Res<Profile>,
    version: Option<Res<crate::VersionData>>,
    index: Res<EntityIndex>,
    mut controlled: ResMut<ControlledVehicle>,
    mut vehicles: Query<(
        &mut Position,
        &mut Rotation,
        &EntityKind,
        Option<&mut OnGround>,
        Option<&Attributes>,
    )>,
    mut players: Query<
        (&mut PhysicsState, &MovementIntent, &Riding, &mut RidingJumpCharge),
        With<LocalPlayer>,
    >,
) {
    let Some(version) = version else {
        controlled.0 = None;
        return;
    };
    let PlayerCollision::View(source) = &*collision else {
        controlled.0 = None;
        return;
    };
    // One local player, as everywhere else in this crate.
    let Some((mut state, intent, riding, mut charge)) = players.iter_mut().next() else {
        controlled.0 = None;
        return;
    };
    let Some(vehicle_id) = riding.0 else {
        controlled.0 = None;
        return;
    };
    let Some(entity) = index.get(vehicle_id) else {
        controlled.0 = None;
        return;
    };
    let Ok((mut position, mut rotation, kind, mut grounded, attributes)) = vehicles.get_mut(entity)
    else {
        controlled.0 = None;
        return;
    };
    let Some(family) = VehicleFamily::for_type_path(kind.0.path()) else {
        controlled.0 = None;
        return;
    };
    let Some(facts) = version.entity_facts(&kind.0) else {
        controlled.0 = None;
        return;
    };

    // Re-seed from the server's reported transform whenever this is a different
    // vehicle than last tick. Mounting is exactly this case, and seeding from the
    // packet rather than from zero is what stops the first tick teleporting the
    // boat to the origin.
    let stale = controlled
        .0
        .as_ref()
        .is_none_or(|held| held.server_id != vehicle_id);
    if stale {
        let seeded_pose = VehicleRenderPose {
            position: Vec3d::new(position.0.x, position.0.y, position.0.z),
            yaw: rotation.0.yaw,
            pitch: rotation.0.pitch,
        };
        controlled.0 = Some(ControlledVehicleState {
            server_id: vehicle_id,
            family,
            motion: EntityMotion::at(seeded_pose.position),
            yaw: rotation.0.yaw,
            pitch: rotation.0.pitch,
            previous: seeded_pose,
            boat: BoatState::default(),
            paddles: (false, false),
        });
    }
    let held = controlled
        .0
        .as_mut()
        .expect("seeded immediately above when stale");
    held.family = family;

    let dims = EntityDimensions::new(
        facts.dimensions.width,
        facts.dimensions.height,
        // Overwritten per family inside the physics entry points; the value here
        // is never the one that collides.
        0.0,
    );
    let intent = intent.0;

    // Capture the start endpoint before the one and only fixed-rate integrator
    // mutates `motion`/rotation below. Per-frame rendering reads these two
    // endpoints; it never runs any part of this tick again.
    held.previous = held.current_pose();

    let mut speed_known = true;
    source.with_view(&mut |view| match family {
        VehicleFamily::Boat => {
            let input = BoatInput {
                left: intent.strafe > 0.0,
                right: intent.strafe < 0.0,
                up: intent.forward > 0.0,
                down: intent.forward < 0.0,
            };
            held.paddles = tick_boat(
                &mut held.motion,
                &mut held.boat,
                &mut held.yaw,
                input,
                dims,
                view,
                &profile.0,
            );
        }
        VehicleFamily::LandMount => {
            let Some(rule) = mount_rule(kind.0.path()) else {
                speed_known = false;
                return;
            };
            let Some(attribute_speed) = mount_speed(attributes) else {
                speed_known = false;
                return;
            };
            let speed = ridden_speed(rule, attribute_speed, intent.sprint);
            let jump_strength = attributes.map_or(HORSE_BASE_JUMP_STRENGTH, |attrs| {
                let value = attribute_value(&attrs.0, &jump_strength_key());
                // `attribute_value`'s no-snapshot fallback for an unmodelled key is
                // `0.0`, which would silently disable jumping. The declared default
                // is `AbstractHorse`'s own `0.7`.
                if value > 0.0 {
                    value
                } else {
                    HORSE_BASE_JUMP_STRENGTH
                }
            });
            tick_ridden_mount(
                &mut held.motion,
                &mut held.yaw,
                &mut held.pitch,
                rule,
                state.0.yaw,
                state.0.pitch,
                intent.strafe,
                intent.forward,
                charge.pending.take(),
                speed,
                jump_strength,
                dims,
                view,
                &profile.0,
            );
        }
    });
    if !speed_known {
        controlled.0 = None;
        return;
    }

    // The pieces that reach pixels. `Position`/`Rotation` are what
    // `pin_passenger_to_vehicle` reads for the seat and what the renderer draws
    // the vehicle at, so these three writes *are* the deliverable's visible half.
    position.0 = Vec3 {
        x: held.motion.position.x,
        y: held.motion.position.y,
        z: held.motion.position.z,
    };
    rotation.0.yaw = held.yaw;
    rotation.0.pitch = held.pitch;
    if let Some(grounded) = grounded.as_mut() {
        grounded.0 = held.motion.on_ground;
    }

    // Vanilla's own boat rider-positioning step: a rider who is
    // not tagged as able to turn freely in boats is carried by the boat's own turn and then
    // clamped to ±105° of its heading. The player is not in that tag (it holds
    // the boat-turning mobs), so this applies to us.
    if family == VehicleFamily::Boat {
        state.0.yaw += held.boat.delta_rotation;
        state.0.yaw = clamp_rider_yaw(state.0.yaw, held.yaw);
    }
}

/// A ridden mount's **own** `minecraft:movement_speed`, never the rider's — the
/// raw attribute, before [`ridden_speed`] applies the family's scale.
///
/// `None` when the server has reported no snapshot for the key, and that is
/// deliberate rather than a fallback waiting to be filled in. A horse's speed is
/// generated per instance (`AbstractHorse.generateSpeed`, roughly `0.1125..0.3375`)
/// so there is no correct default to guess, and `attribute_value`'s own
/// no-snapshot answer is the **generic mob** `0.7` — three times the fastest real
/// horse. Declining leaves the mount to the server, which is visible and
/// diagnosable; guessing produces a horse that outruns every server correction.
#[must_use]
pub fn mount_speed(attributes: Option<&Attributes>) -> Option<f64> {
    let attributes = attributes?;
    let key = movement_speed_key();
    let snapshot = attributes.0.iter().find(|s| s.attribute == key)?;
    // Kept in `f64` because `Pig`/`Strider` narrow only *after* multiplying by
    // their scale, and narrowing here first would round twice.
    Some(attribute_value(std::slice::from_ref(snapshot), &key))
}

/// `TickSet::Send`: the producers this whole unit exists to add.
///
/// * `ClientAction::MoveVehicle` — vanilla's own client-side player-tick passenger
///   branch sends the move-vehicle packet once per
///   tick, unconditionally, for as long as
///   the controlled vehicle is a different entity and is locally authoritative.
/// * `ClientAction::PaddleBoat` — sent from inside vanilla's own boat-tick
///   authoritative branch, **every tick and not on change**. Vanilla has no edge
///   tracker here, so neither does this: the animation is driven by the packet
///   arriving, and an edge-triggered version leaves a stuck paddle whenever one is
///   dropped.
///
/// Gated on [`Egress`] for the same reason [`crate::player::send_fall_flying_command`]
/// is: a version adapter correctly has no Play-state packet before the server
/// places us, so sending earlier is dropped-action noise.
pub fn send_vehicle_actions(
    egress: Res<Egress>,
    vehicle: Res<ControlledVehicle>,
    mut queue: ResMut<ActionQueue>,
) {
    if !(egress.in_world && egress.live) {
        return;
    }
    let Some(held) = vehicle.0.as_ref() else {
        return;
    };
    if held.family == VehicleFamily::Boat {
        // Before `MoveVehicle`, matching vanilla's order: the paddle send is
        // inside vanilla's own boat-tick step, which runs from the base entity
        // tick inside the client-side player-tick step, and the vehicle-move send is after that call.
        queue.0.push(ClientAction::PaddleBoat {
            left: held.paddles.0,
            right: held.paddles.1,
        });
    }
    queue.0.push(ClientAction::MoveVehicle {
        pos: Vec3 {
            x: held.motion.position.x,
            y: held.motion.position.y,
            z: held.motion.position.z,
        },
        rotation: lodestone_model::Rotation {
            yaw: held.yaw,
            pitch: held.pitch,
        },
        on_ground: held.motion.on_ground,
    });
}

/// `IngestSet::Apply`: `ClientEvent::VehicleMoved` → the ridden vehicle's
/// [`Position`]/[`Rotation`], and a reset of the local prediction.
///
/// This is a **rejection**, not a periodic sync — see this module's docs. So the
/// local [`EntityMotion`] is rebuilt at the server's position with zero velocity
/// rather than merely nudged: continuing from our own velocity is what would
/// produce the rubber-band, because the velocity is exactly what the server just
/// refused.
///
/// The boat's between-tick state is reset too. Vanilla's own water-level and
/// land-friction fields
/// were both measured at a position the server has now discarded, and
/// vanilla's own delta-rotation field is the angular velocity that helped
/// earn the rejection.
pub fn apply_vehicle_moved(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut controlled: ResMut<ControlledVehicle>,
    mut vehicles: Query<(&mut Position, &mut Rotation)>,
    players: Query<&Riding, With<LocalPlayer>>,
) {
    for event in batch.events() {
        let ClientEvent::VehicleMoved { pos, yaw, pitch } = event else {
            continue;
        };
        // The packet names no entity, so the subject is whatever `session::Riding`
        // says we are in. A correction that arrives after a dismount has no
        // subject and is dropped, which is right: the vehicle is the server's
        // again.
        let Some(vehicle_id) = players.iter().next().and_then(|riding| riding.0) else {
            continue;
        };
        if let Some(entity) = index.get(vehicle_id)
            && let Ok((mut position, mut rotation)) = vehicles.get_mut(entity)
        {
            position.0 = *pos;
            rotation.0.yaw = *yaw;
            rotation.0.pitch = *pitch;
        }
        if let Some(held) = controlled.0.as_mut()
            && held.server_id == vehicle_id
        {
            held.reset_to(VehicleRenderPose {
                position: Vec3d::new(pos.x, pos.y, pos.z),
                yaw: *yaw,
                pitch: *pitch,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The family switch, at the two boundaries that matter: the boat suffix match
    /// must cover every wood variant, and a minecart must **not** be simulated.
    #[test]
    fn the_family_switch_covers_the_wood_variants_and_declines_a_minecart() {
        for boat in [
            "oak_boat",
            "bamboo_raft",
            "oak_chest_boat",
            "bamboo_chest_raft",
            "cherry_boat",
        ] {
            assert_eq!(
                VehicleFamily::for_type_path(boat),
                Some(VehicleFamily::Boat),
                "{boat} must be a boat"
            );
        }
        for mount in ["horse", "pig", "strider", "camel", "donkey"] {
            assert_eq!(
                VehicleFamily::for_type_path(mount),
                Some(VehicleFamily::LandMount),
                "{mount} must be a land mount"
            );
        }
        // The negative control this pairs with: a minecart is rail-following and
        // server-broadcast, so simulating it as a land mount would fight
        // `ClientboundMoveMinecartPacket` with plain gravity.
        assert_eq!(VehicleFamily::for_type_path("minecart"), None);
        assert_eq!(VehicleFamily::for_type_path("chest_minecart"), None);
        assert_eq!(VehicleFamily::for_type_path("cow"), None);
    }

    #[test]
    fn a_vehicle_correction_resets_both_render_endpoints() {
        let old = VehicleRenderPose {
            position: Vec3d::new(1.0, 64.0, 2.0),
            yaw: 45.0,
            pitch: 3.0,
        };
        let corrected = VehicleRenderPose {
            position: Vec3d::new(9.0, 70.0, -4.0),
            yaw: 270.0,
            pitch: -8.0,
        };
        let mut held = ControlledVehicleState {
            server_id: 42,
            family: VehicleFamily::Boat,
            motion: EntityMotion::at(old.position),
            yaw: old.yaw,
            pitch: old.pitch,
            previous: old,
            boat: BoatState::default(),
            paddles: (true, true),
        };
        held.motion.velocity = Vec3d::new(1.0, 2.0, 3.0);

        held.reset_to(corrected);

        assert_eq!(held.previous, corrected);
        assert_eq!(held.current_pose(), corrected);
        assert_eq!(held.motion.velocity, Vec3d::ZERO);
        assert_eq!(held.paddles, (false, false));
    }
}
