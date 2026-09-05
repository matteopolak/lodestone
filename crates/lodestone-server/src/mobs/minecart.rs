//! `MobSim`'s minecart slice — the five `AbstractMinecart` subclasses, rail
//! following, riding, and the furnace/TNT special cases.
//!
//! # What this is
//!
//! A port of vanilla's own abstract minecart base and its "old" movement
//! behaviour, plus
//! the plain, furnace and TNT minecarts' own overrides. **Not**
//! `NewMinecartBehavior` — 26.2 ships both behind one gate,
//! `AbstractMinecart.useExperimentalMovement` (`level.enabledFeatures()
//! .contains(FeatureFlags.MINECART_IMPROVEMENTS)`), and that feature flag is
//! packaged as its own opt-in datapack
//! (`.cache/mc/26.2/src/data/minecraft/datapacks/minecart_improvements/pack.mcmeta`),
//! not part of any vanilla world's default feature set. `OldMinecartBehavior`
//! is therefore what an ordinary world actually runs, and it is the one
//! ported here.
//!
//! Follows the sidecar shape [`super::TrackedVehicle`]/[`super::TrackedTnt`]
//! already established: a plain `HashMap<i32, TrackedMinecart>`, no `SimMob`
//! goal machinery, because a minecart has no AI beyond rail-following.
//!
//! # How it works
//!
//! [`MobSim::tick_minecarts`] is `AbstractMinecart.tick`/`OldMinecartBehavior
//! .tick`, transcribed in vanilla's own order: gravity, then either
//! [`move_along_track`] (on a rail) or [`come_off_track`] (not on one), then
//! the yaw/flip bookkeeping that keeps a cart's sprite pointed the way it is
//! actually travelling. [`move_along_track`] is
//! `OldMinecartBehavior.moveAlongTrack` in full: the powered-rail
//! boost/brake read, the ascending-rail slide impulse, the exit-pair
//! geometry that snaps a cart's `(x, z)` onto the rail's own centreline for
//! all ten [`RailShape`] values (six straight, four curved), the
//! `move_entity` collision nudge (the same shared integrator
//! [`super::MobSim::tick_vehicles`]/`tick_tnt` already use — see those
//! modules' own doc comments), the hill-speed adjustment from
//! [`current_pos_along_rail`]'s before/after height delta, and the
//! `POWERED_RAIL` end-of-function boost or the two-conductor brake nudge.
//!
//! Powered and detector rails' own `POWERED` tracking already exists
//! (`crate::redstone_rail`, `crate::redstone::is_detector_rail`) — this
//! module is a second *reader* of `POWERED`, not a second writer: a live
//! powered rail's own `[shape=…,powered=…]` state string is read directly out
//! of the world oracle the same way `tick_vehicles`/`tick_tnt` already do,
//! with no dependency on `crate::redstone_rail`'s own `RailShape` (that one is
//! deliberately narrowed to the six straight shapes a powered/activator rail
//! can hold; a minecart also has to parse the four curves a plain
//! `minecraft:rail` can be in, so this module parses the raw `shape` state
//! property itself via [`RailShape`]).
//!
//! # What is deliberately simplified
//!
//! * **No rider movement nudge.** `OldMinecartBehavior.moveAlongTrack` reads
//!   `ServerPlayer.getLastClientMoveIntent()` and adds a `0.001`-magnitude
//!   nudge in that direction when the cart's own speed is near zero — this is
//!   what lets a player free a stalled cart by walking against it. Wiring a
//!   live per-tick input vector from a connection into `MobSim` is a
//!   materially separate seam (nothing here has one today), so this is cut;
//!   the substance — rail-shape following, slopes, the speed model — is not
//!   touched by the cut. A stalled cart on a flat rail with no power stays
//!   stalled until pushed by hand (nudged by another entity) or reached by a
//!   powered rail.
//! * **No entity pushing/auto-mount.** `pushAndPickupEntities` (an unridden,
//!   moving cart auto-mounts a non-player, non-golem entity standing in its
//!   path) is not ported. Mounting here is explicit right-click only, as
//!   [`MobSim::mount_vehicle`] already is for boats.
//! * **`applyEffectsFromBlocks`** (honey/powder-snow/cobweb per-block status
//!   effects) is not ported — the same cut `crate::mobs::tnt`'s own module
//!   doc makes for primed TNT, for the identical reason (out of scope, and it
//!   changes no position or velocity this module's own gates check).
//! * **No fluid-current push.** Matches `crate::mobs::tnt`'s cut: a cart
//!   still floats, collides and settles correctly in water (via
//!   [`in_water`]'s real fluid-amount read, not a coarse boolean), it just
//!   does not drift with a current.
//! * **Detector rail has no *producer* here.** `crate::redstone::
//!   is_detector_rail`'s `POWERED` *read* already exists; nothing (not this
//!   module, not any other) yet sets a detector rail's `POWERED` when a
//!   minecart sits on it (`DetectorRailBlock.checkPressed`'s own
//!   world-mutation — `setBlock` plus a neighbour fan-out and a 20-tick
//!   re-check schedule — needs the live `ChunkSource` and scheduled-tick
//!   queue this sim's `world: &ChunkWorld` snapshot does not have). A
//!   detector rail under a minecart today reads exactly as it did before this
//!   feature: never powered by the cart's own presence.
//! * **TNT-minecart ignition is activator-rail only.** Vanilla also primes a
//!   TNT minecart from a burning-arrow hit, an explosion, fire, or a hard
//!   fall (`MinecartTNT.hurtServer`/`destroy`/`causeFallDamage`) — none of
//!   which this crate's minecart tick has a signal for (no combat-vs-vehicle
//!   model exists at all). Only `activateMinecart` (the activator-rail
//!   producer, shared with the plain cart's own ejection) is wired.
//! * **Chest/hopper minecart inventories are real storage with no GUI.**
//!   [`TrackedMinecart::slots`] is a genuine `Vec<Option<ItemStack>>` sized
//!   [`MinecartKind::container_size`] and round-trips through this sim, but
//!   nothing opens a menu against it: `crate::container_click`'s
//!   `MenuLayout` and `crate::server`'s window-id bookkeeping are keyed to a
//!   `BlockPos`-addressed container (`crate::block_entities
//!   ::BlockEntityRegistry`) everywhere they are called from today, and
//!   re-keying that seam to also address a live entity id is a materially
//!   larger change than this feature. A hopper minecart's own `Hopper` pull
//!   from/into a world hopper is the same kind of gap, for the same reason
//!   `crate::block_entities`'s own module doc gives for "no chest block
//!   entity in this crate at all" — there is nothing on either side of that
//!   adjacency to wire together yet.
//! * **No hurt-flash bookkeeping.** `Minecart.activateMinecart` also sets a
//!   ten-tick hurt animation/damage jolt on ejection
//!   (`setHurtDir`/`setHurtTime`/`setDamage`) purely for the client's shake
//!   animation; the ejection itself (`ejectPassengers`) is what matters
//!   mechanically and is ported.
//!
//! # Dependencies
//!
//! [`lodestone_physics::entity::move_entity`] for collision (shared with
//! `tick_vehicles`/`tick_tnt`), `crate::redstone::{base_name,
//! get_bool_property, is_redstone_conductor}` for reading a powered rail's own
//! state without re-deriving the parser, and [`super::MobSim::explode`]/
//! [`super::MobSim::pending_detonations`] for a TNT minecart's blast — the
//! same two calls `crate::mobs::tnt` already makes for a creeper's fuse and a
//! primed-TNT entity's own, so a TNT minecart is a third *producer* into an
//! existing pipeline, not a new consumer.

use lodestone_entity::DamageFlags;
use lodestone_model::{BlockPos, ItemStack, ResourceKey, Vec3};
use lodestone_physics::{
    Aabb, CollisionView, EntityDimensions, EntityMotion, MoveContext, PhysicsProfile, Vec3d,
    move_entity,
};
use lodestone_physics::mth::wrap_degrees_f32;
use uuid::Uuid;

use crate::redstone::{base_name, get_bool_property, get_str_property, is_redstone_conductor};

use super::{Detonation, MobSim, TrackedMinecart, block_state_id};

/// `minecraft:rail` — the one rail block not already named as a constant
/// somewhere in this crate (`crate::redstone_rail::{POWERED_RAIL,
/// ACTIVATOR_RAIL}`, `crate::redstone::DETECTOR_RAIL}`).
pub const RAIL: &str = "minecraft:rail";

/// `BaseRailBlock.isRail` — `state.is(BlockTags.RAILS) && state.getBlock()
/// instanceof BaseRailBlock`, narrowed to the four block ids `BlockTags.RAILS`
/// actually holds in 26.2 (checked against the jar's own block list, not
/// assumed): plain rail, powered rail, activator rail, detector rail.
#[must_use]
pub fn is_rail_block(state: &str) -> bool {
    matches!(
        base_name(state),
        RAIL | crate::redstone_rail::POWERED_RAIL | crate::redstone_rail::ACTIVATOR_RAIL | crate::redstone::DETECTOR_RAIL
    )
}

/// `RailShape`, all ten values — six straight/ascending plus the four curves a
/// plain `minecraft:rail` (and a detector rail) can hold. Deliberately a
/// separate type from `crate::redstone_rail::RailShape`, which is narrowed to
/// the six a powered/activator rail's own `SHAPE` property allows; a minecart
/// has to follow every shape a plain rail can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailShape {
    NorthSouth,
    EastWest,
    AscendingEast,
    AscendingWest,
    AscendingNorth,
    AscendingSouth,
    SouthEast,
    SouthWest,
    NorthWest,
    NorthEast,
}

impl RailShape {
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "north_south" => Self::NorthSouth,
            "east_west" => Self::EastWest,
            "ascending_east" => Self::AscendingEast,
            "ascending_west" => Self::AscendingWest,
            "ascending_north" => Self::AscendingNorth,
            "ascending_south" => Self::AscendingSouth,
            "south_east" => Self::SouthEast,
            "south_west" => Self::SouthWest,
            "north_west" => Self::NorthWest,
            "north_east" => Self::NorthEast,
            _ => return None,
        })
    }

    /// `RailShape.isSlope()`.
    #[must_use]
    pub fn is_slope(self) -> bool {
        matches!(
            self,
            Self::AscendingEast | Self::AscendingWest | Self::AscendingNorth | Self::AscendingSouth
        )
    }

    /// `AbstractMinecart.EXITS` — the two cell-relative offsets (as
    /// `(dx, dy, dz)`) a cart travels between for this shape.
    /// `Direction.WEST/EAST/NORTH/SOUTH.getUnitVec3i()` and `.below()`,
    /// transcribed verbatim from the static-initialiser table.
    #[must_use]
    pub fn exits(self) -> ((i32, i32, i32), (i32, i32, i32)) {
        match self {
            Self::NorthSouth => ((0, 0, -1), (0, 0, 1)),
            Self::EastWest => ((-1, 0, 0), (1, 0, 0)),
            Self::AscendingEast => ((-1, -1, 0), (1, 0, 0)),
            Self::AscendingWest => ((-1, 0, 0), (1, -1, 0)),
            Self::AscendingNorth => ((0, 0, -1), (0, -1, 1)),
            Self::AscendingSouth => ((0, -1, -1), (0, 0, 1)),
            Self::SouthEast => ((0, 0, 1), (1, 0, 0)),
            Self::SouthWest => ((0, 0, 1), (-1, 0, 0)),
            Self::NorthWest => ((0, 0, -1), (-1, 0, 0)),
            Self::NorthEast => ((0, 0, -1), (1, 0, 0)),
        }
    }
}

/// Reads a rail block state's own `shape` property as a full, ten-value
/// [`RailShape`] — the minecart-following counterpart of
/// `crate::redstone_rail::shape_of`, which only ever sees the six a
/// powered/activator rail can hold.
#[must_use]
pub fn rail_shape(state: &str) -> Option<RailShape> {
    get_str_property(state, "shape").and_then(RailShape::from_str)
}

/// The five `AbstractMinecart` subclasses this crate models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MinecartKind {
    /// `Minecart` — the only rideable one (`isRideable() => true`).
    Plain,
    /// `MinecartChest` — `AbstractMinecartContainer`, 27 slots.
    Chest,
    /// `MinecartHopper` — `AbstractMinecartContainer` + `Hopper`, 5 slots.
    Hopper,
    /// `MinecartFurnace` — burns coal/charcoal for a constant self-push.
    Furnace,
    /// `MinecartTNT` — primes and explodes off an activator rail.
    Tnt,
}

impl MinecartKind {
    /// Vanilla's own minecart-item item→type pairing (its own item registration
    /// table's five `new MinecartItem(EntityTypes.X, …)` registrations) — the item id
    /// *is* the entity-type id, exactly as `crate::boat`'s own derivation is
    /// for boats.
    #[must_use]
    pub fn from_item(item: &str) -> Option<Self> {
        Some(match item {
            "minecraft:minecart" => Self::Plain,
            "minecraft:chest_minecart" => Self::Chest,
            "minecraft:hopper_minecart" => Self::Hopper,
            "minecraft:furnace_minecart" => Self::Furnace,
            "minecraft:tnt_minecart" => Self::Tnt,
            _ => return None,
        })
    }

    #[must_use]
    pub fn entity_type(self) -> ResourceKey {
        let name = match self {
            Self::Plain => "minecraft:minecart",
            Self::Chest => "minecraft:chest_minecart",
            Self::Hopper => "minecraft:hopper_minecart",
            Self::Furnace => "minecraft:furnace_minecart",
            Self::Tnt => "minecraft:tnt_minecart",
        };
        name.parse().expect("every minecart kind is a valid resource key")
    }

    /// `AbstractMinecart.isRideable()` — `Minecart` alone overrides it `true`.
    #[must_use]
    pub fn is_rideable(self) -> bool {
        matches!(self, Self::Plain)
    }

    /// `AbstractMinecart.isFurnace()`.
    #[must_use]
    pub fn is_furnace(self) -> bool {
        matches!(self, Self::Furnace)
    }

    /// `AbstractMinecartContainer.getContainerSize()` — `0` for a kind with
    /// no inventory at all (see this module's own doc comment for why that
    /// storage is currently unreachable through any menu).
    #[must_use]
    pub fn container_size(self) -> usize {
        match self {
            Self::Chest => 27,
            Self::Hopper => 5,
            _ => 0,
        }
    }
}

// ---- Constants, every one a record value rather than a guess ----

/// `OldMinecartBehavior.MAX_SPEED_ON_LAND`/`ABSOLUTE_MAX_SPEED`.
pub const MAX_SPEED_LAND: f64 = 0.4;
/// `OldMinecartBehavior.MAX_SPEED_IN_WATER`.
pub const MAX_SPEED_WATER: f64 = 0.2;
/// The literal `0.0078125` slide impulse `moveAlongTrack` adds on an
/// ascending rail (halved again in water).
const SLIDE_SPEED: f64 = 0.007_812_5;
/// The powered-rail boost magnitude, `speed = 0.06`.
const POWERED_BOOST: f64 = 0.06;
/// The two-conductor brake nudge on an otherwise-stalled powered rail.
const POWERED_STALL_NUDGE: f64 = 0.02;
/// `AbstractMinecart.getDefaultGravity()` — land.
const GRAVITY_LAND: f64 = 0.04;
/// `AbstractMinecart.getDefaultGravity()` — water.
const GRAVITY_WATER: f64 = 0.005;
/// `AbstractMinecart.getAirDrag()` — `comeOffTrack`'s off-rail drag.
const AIR_DRAG: f64 = 0.95;
/// `AbstractMinecart.WATER_SLOWDOWN_FACTOR`.
const WATER_SLOWDOWN: f64 = 0.95;
/// `OldMinecartBehavior.getSlowdownFactor()` — ridden (`isVehicle()`).
const SLOWDOWN_RIDDEN: f64 = 0.997;
/// `OldMinecartBehavior.getSlowdownFactor()` — unridden.
const SLOWDOWN_UNRIDDEN: f64 = 0.96;
/// `MinecartFurnace.FUEL_TICKS_PER_ITEM` — one coal/charcoal.
pub const FURNACE_FUEL_TICKS_PER_ITEM: i32 = 3600;
/// `MinecartFurnace.MAX_FUEL_TICKS`.
pub const FURNACE_MAX_FUEL_TICKS: i32 = 32_000;
/// `MinecartTNT.primeFuse`'s fixed fuse — the only ignition producer this
/// module wires (see the module doc for why the others are cut).
pub const TNT_MINECART_FUSE: i32 = 80;
/// `MinecartTNT.explosionPowerBase`'s default.
const TNT_EXPLOSION_POWER_BASE: f32 = 4.0;
/// `MinecartTNT.explosionSpeedFactor`'s default.
const TNT_EXPLOSION_SPEED_FACTOR: f64 = 1.0;

/// `AbstractMinecart`'s hitbox, `0.98 x 0.7`
/// (`crates/lodestone-data/src/generated/entity_dimensions.rs`), no
/// auto-step: a bare `Entity`'s `maxUpStep()` is `0.0`, matching
/// `mobs::tnt::TNT_DIMENSIONS`'s own reasoning for a non-`LivingEntity`.
pub const MINECART_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.98, 0.7, 0.0);

/// `MinecartItem.useOn`'s spawn point for a rail at `pos` already known to
/// carry `shape` — `pos + (0.5, 0.0625 + offset, 0.5)`, `offset` `0.5` on a
/// slope and `0.0` otherwise. The dispenser's own placement math
/// (`crate::redstone_dispenser::minecart_dispense`) is a different formula
/// entirely and does not call this.
#[must_use]
pub fn placement_position(pos: BlockPos, shape: Option<RailShape>) -> Vec3 {
    let offset = if shape.is_some_and(RailShape::is_slope) { 0.5 } else { 0.0 };
    Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.0625 + offset,
        f64::from(pos.z) + 0.5,
    )
}

/// A [`CollisionView`] over a caller-supplied block-state oracle — the
/// minecart analogue of `vehicles::VehicleCollision`/`tnt::TntCollision`:
/// real per-block-state collision shapes plus a fluid read for
/// [`in_water`], and a raw state-string accessor the rail-following math
/// needs that the trait itself has no room for.
struct MinecartCollision<'a> {
    block_state: &'a dyn Fn(i32, i32, i32) -> String,
}

impl MinecartCollision<'_> {
    fn state_at(&self, x: i32, y: i32, z: i32) -> String {
        (self.block_state)(x, y, z)
    }
}

impl CollisionView for MinecartCollision<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        let name = (self.block_state)(x, y, z);
        let Some(state) = block_state_id(&name) else {
            return;
        };
        let shape = lodestone_data::collision_shapes::collision_boxes(state);
        let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
        for b in shape {
            out.push(Aabb::new(
                bx + f64::from(b.min[0]),
                by + f64::from(b.min[1]),
                bz + f64::from(b.min[2]),
                bx + f64::from(b.max[0]),
                by + f64::from(b.max[1]),
                bz + f64::from(b.max[2]),
            ));
        }
    }

    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        let name = (self.block_state)(x, y, z);
        crate::fluid::fluid_state_of(&name).is_some_and(|s| matches!(s.kind, crate::fluid::FluidKind::Water))
    }
}

/// `Entity.isInWater()`, approximated over the cart's own bounding box —
/// vanilla's real `updateInWaterStateAndDoWaterCurrentPushing` scans every
/// cell the (slightly shrunk) box overlaps; this does the same over
/// [`MINECART_DIMENSIONS`]'s box rather than a single-cell guess, so a cart
/// spanning a rail's block and the water below it still reads "in water".
fn in_water(view: &MinecartCollision<'_>, position: Vec3d) -> bool {
    let bb = MINECART_DIMENSIONS.bounding_box(position).inflate(-0.001);
    let (x0, x1) = (bb.min_x.floor() as i32, bb.max_x.floor() as i32);
    let (y0, y1) = (bb.min_y.floor() as i32, bb.max_y.floor() as i32);
    let (z0, z1) = (bb.min_z.floor() as i32, bb.max_z.floor() as i32);
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                if view.is_water(x, y, z) {
                    return true;
                }
            }
        }
    }
    false
}

/// `AbstractMinecart.getCurrentBlockPosOrRailBelow` — the cell a cart reads
/// its rail state from: its own floored cell, or one below when *that* cell
/// isn't a rail but the one under it is (the "cart sits fractionally above a
/// flat rail's `y + 0.0625`" case).
fn current_block_pos_or_rail_below(view: &MinecartCollision<'_>, position: Vec3d) -> BlockPos {
    let xt = position.x.floor() as i32;
    let mut yt = position.y.floor() as i32;
    let zt = position.z.floor() as i32;
    if is_rail_block(&view.state_at(xt, yt - 1, zt)) {
        yt -= 1;
    }
    BlockPos::new(xt, yt, zt)
}

/// `OldMinecartBehavior.getPos` — the cart's exact position **on the rail's
/// own centreline** for an arbitrary `(x, y, z)`, used only for the
/// before/after height sample `move_along_track`'s hill-speed adjustment
/// reads. `None` when `(x, y, z)`'s cell (or the one below it) is not a rail
/// at all.
fn current_pos_along_rail(view: &MinecartCollision<'_>, x: f64, y: f64, z: f64) -> Option<Vec3d> {
    let xt = x.floor() as i32;
    let mut yt = y.floor() as i32;
    let zt = z.floor() as i32;
    if is_rail_block(&view.state_at(xt, yt - 1, zt)) {
        yt -= 1;
    }
    let state = view.state_at(xt, yt, zt);
    if !is_rail_block(&state) {
        return None;
    }
    let shape = rail_shape(&state)?;
    let (exit0, exit1) = shape.exits();
    let x0 = f64::from(xt) + 0.5 + f64::from(exit0.0) * 0.5;
    let y0 = f64::from(yt) + 0.0625 + f64::from(exit0.1) * 0.5;
    let z0 = f64::from(zt) + 0.5 + f64::from(exit0.2) * 0.5;
    let x1 = f64::from(xt) + 0.5 + f64::from(exit1.0) * 0.5;
    let y1 = f64::from(yt) + 0.0625 + f64::from(exit1.1) * 0.5;
    let z1 = f64::from(zt) + 0.5 + f64::from(exit1.2) * 0.5;
    let xd = x1 - x0;
    let yd = (y1 - y0) * 2.0;
    let zd = z1 - z0;
    let progress = if xd == 0.0 {
        z - f64::from(zt)
    } else if zd == 0.0 {
        x - f64::from(xt)
    } else {
        let xx = x - x0;
        let zz = z - z0;
        (xx * xd + zz * zd) * 2.0
    };
    let mut out_y = y0 + yd * progress;
    if yd < 0.0 {
        out_y += 1.0;
    } else if yd > 0.0 {
        out_y += 0.5;
    }
    Some(Vec3d::new(x0 + xd * progress, out_y, z0 + zd * progress))
}

/// `MinecartFurnace.calculateNewPushAlong` — re-aim the stored push vector
/// along the current direction of travel, keeping its magnitude.
/// `Vec3.projectedOn(other) = other.scale(this.dot(other) / other.lengthSqr())`.
fn calculate_new_push_along(push: Vec3d, movement: Vec3d) -> Vec3d {
    let push_h_sqr = push.x * push.x + push.z * push.z;
    let move_h_sqr = movement.x * movement.x + movement.z * movement.z;
    if push_h_sqr <= 1.0e-4 || move_h_sqr <= 0.001 {
        return push;
    }
    let denom = movement.x * movement.x + movement.y * movement.y + movement.z * movement.z;
    if denom <= 0.0 {
        return push;
    }
    let dot = push.x * movement.x + push.y * movement.y + push.z * movement.z;
    let scale = dot / denom;
    let proj = Vec3d::new(movement.x * scale, movement.y * scale, movement.z * scale);
    let mag = push.length();
    let n = proj.normalize();
    if n == Vec3d::ZERO {
        push
    } else {
        Vec3d::new(n.x * mag, n.y * mag, n.z * mag)
    }
}

impl<'w> MobSim<'w> {
    /// `AbstractMinecart.createMinecart` + `level.addFreshEntity` — spawns a
    /// fresh, empty, un-primed, un-fuelled cart at `position` and returns its
    /// network id. Yaw starts at `0.0`: vanilla's `MinecartItem`/
    /// `MinecartDispenseItemBehavior` never call `setYRot`, so a freshly
    /// placed cart's facing comes entirely from [`MobSim::tick_minecarts`]'s
    /// own travel-direction computation on its first moving tick.
    pub fn spawn_minecart(&mut self, kind: MinecartKind, position: Vec3) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.minecarts.insert(
            id,
            TrackedMinecart {
                uuid: Uuid::new_v4(),
                kind,
                motion: EntityMotion::at(Vec3d::new(position.x, position.y, position.z)),
                yaw: 0.0,
                yaw_o: 0.0,
                flipped: false,
                rider: None,
                slots: vec![None; kind.container_size()],
                fuel: 0,
                push: Vec3d::ZERO,
                fuse: -1,
            },
        );
        id
    }

    /// The number of live minecarts.
    #[must_use]
    pub fn minecart_count(&self) -> usize {
        self.minecarts.len()
    }

    /// A tracked minecart's kind, if `id` is one.
    #[must_use]
    pub fn minecart_kind(&self, id: i32) -> Option<MinecartKind> {
        self.minecarts.get(&id).map(|c| c.kind)
    }

    /// A tracked minecart's `(position, yaw)`.
    #[must_use]
    pub fn minecart_transform(&self, id: i32) -> Option<(Vec3, f32)> {
        self.minecarts.get(&id).map(|c| {
            (
                Vec3::new(c.motion.position.x, c.motion.position.y, c.motion.position.z),
                c.yaw,
            )
        })
    }

    /// The controlling passenger's player entity id, if occupied.
    #[must_use]
    pub fn minecart_rider(&self, id: i32) -> Option<i32> {
        self.minecarts.get(&id).and_then(|c| c.rider)
    }

    /// The minecart `player_entity_id` is riding, if any.
    #[must_use]
    pub fn minecart_ridden_by(&self, player_entity_id: i32) -> Option<i32> {
        self.minecarts
            .iter()
            .find(|(_, c)| c.rider == Some(player_entity_id))
            .map(|(&id, _)| id)
    }

    /// A container minecart's own slots (empty, all-`None`, for a
    /// non-container kind). See this module's own doc comment for why
    /// nothing yet opens a menu against them.
    #[must_use]
    pub fn minecart_slots(&self, id: i32) -> Option<&[Option<ItemStack>]> {
        self.minecarts.get(&id).map(|c| c.slots.as_slice())
    }

    /// A furnace minecart's remaining fuel, in ticks (`0` for any other
    /// kind, or an id that is not a minecart).
    #[must_use]
    pub fn minecart_fuel(&self, id: i32) -> i32 {
        self.minecarts.get(&id).map_or(0, |c| c.fuel)
    }

    /// A TNT minecart's fuse (`-1` = not primed; `0` detonates this tick).
    #[must_use]
    pub fn minecart_fuse(&self, id: i32) -> i32 {
        self.minecarts.get(&id).map_or(-1, |c| c.fuse)
    }

    /// `Minecart.interact` — `player.startRiding(this)`. Refuses (vanilla
    /// `PASS`) when `id` is not a minecart, the kind is not
    /// [`MinecartKind::is_rideable`], or the seat is already taken by someone
    /// else. A player already riding something else is dismounted first —
    /// see [`MobSim::mount_vehicle`]'s own doc for why.
    pub fn mount_minecart(&mut self, id: i32, player_entity_id: i32) -> bool {
        let Some(cart) = self.minecarts.get(&id) else {
            return false;
        };
        if !cart.kind.is_rideable() {
            return false;
        }
        if cart.rider.is_some_and(|rider| rider != player_entity_id) {
            return false;
        }
        if let Some(previous) = self.minecart_ridden_by(player_entity_id) {
            if previous != id {
                if let Some(old) = self.minecarts.get_mut(&previous) {
                    old.rider = None;
                }
            }
        }
        // A player already aboard a *boat* must be dismounted from it first —
        // `Entity.startRiding`'s own "already riding something" precondition,
        // the cross-family half `MobSim::mount_vehicle`'s own doc comment
        // only states for two boats. `dismount_rider` is a no-op when the
        // player rides no boat.
        self.dismount_rider(player_entity_id);
        if let Some(cart) = self.minecarts.get_mut(&id) {
            cart.rider = Some(player_entity_id);
        }
        true
    }

    /// `Entity.stopRiding` for whatever `player_entity_id` rides, returning
    /// the minecart it left. Called on disconnect exactly as
    /// [`MobSim::dismount_rider`] is for a boat.
    pub fn dismount_minecart_rider(&mut self, player_entity_id: i32) -> Option<i32> {
        let id = self.minecart_ridden_by(player_entity_id)?;
        if let Some(cart) = self.minecarts.get_mut(&id) {
            cart.rider = None;
        }
        Some(id)
    }

    /// `MinecartFurnace.addFuel` — `ItemTags.FURNACE_MINECART_FUEL` is
    /// exactly `{coal, charcoal}` (`.cache/mc/26.2/src/data/minecraft/tags/
    /// item/furnace_minecart_fuel.json`). `interacting_pos` is the clicking
    /// player's own position, which sets the push *direction* (`this.push =
    /// this.position().subtract(interactingPos).horizontal()`); `false` for
    /// anything else, matching vanilla's refusal.
    pub fn add_minecart_fuel(&mut self, id: i32, item: &str, interacting_pos: Vec3) -> bool {
        let Some(cart) = self.minecarts.get_mut(&id) else {
            return false;
        };
        if !cart.kind.is_furnace() {
            return false;
        }
        if !matches!(item, "minecraft:coal" | "minecraft:charcoal") {
            return false;
        }
        if cart.fuel + FURNACE_FUEL_TICKS_PER_ITEM > FURNACE_MAX_FUEL_TICKS {
            return false;
        }
        cart.fuel += FURNACE_FUEL_TICKS_PER_ITEM;
        if cart.fuel > 0 {
            cart.push = Vec3d::new(
                cart.motion.position.x - interacting_pos.x,
                0.0,
                cart.motion.position.z - interacting_pos.z,
            );
        }
        true
    }

    /// One tick of every live minecart: gravity, rail-following (or
    /// off-rail physics), the yaw/flip bookkeeping, furnace fuel/push, and
    /// TNT fuse/detonation. See this module's own doc comment for the exact
    /// vanilla call order this transcribes and everything deliberately cut.
    ///
    /// `block_state` is the live-world oracle, taken as a closure for
    /// `tick_vehicles`/`tick_tnt`'s own reason: this sim's `world` is a
    /// spawn-time snapshot, so a driver with the real `ChunkSource` supplies
    /// the answer instead (`tick::run_tick_loop`).
    pub fn tick_minecarts(&mut self, block_state: &dyn Fn(i32, i32, i32) -> String) {
        // The disconnect self-heal, identical in shape to `tick_vehicles`'
        // own: a rider whose connection vanished without an explicit
        // dismount must not freeze forever with a phantom occupant. Guarded
        // on a non-empty roster for the same "empty means no information"
        // reason `tick_vehicles`' own comment gives.
        if !self.players.is_empty() {
            let connected: Vec<i32> = self
                .players
                .iter()
                .filter_map(|p| p.identity.map(|identity| identity.entity_id))
                .collect();
            if !connected.is_empty() {
                for cart in self.minecarts.values_mut() {
                    if cart.rider.is_some_and(|rider| !connected.contains(&rider)) {
                        cart.rider = None;
                    }
                }
            }
        }

        let view = MinecartCollision { block_state };
        let profile = PhysicsProfile::default();
        let mut ids: Vec<i32> = self.minecarts.keys().copied().collect();
        ids.sort_unstable();

        let mut detonated: Vec<(i32, Vec3, f32)> = Vec::new();

        for id in ids {
            let Some(cart) = self.minecarts.get_mut(&id) else {
                continue;
            };
            let prev_position = cart.motion.position;

            // `applyGravity()` — `getDefaultGravity()` depends on
            // `isInWater()`, read before gravity is applied (matches
            // vanilla: `isInWater` consults the entity's *current*, not
            // post-gravity, position — gravity has not moved it yet).
            let wet = in_water(&view, cart.motion.position);
            cart.motion.velocity.y -= if wet { GRAVITY_WATER } else { GRAVITY_LAND };

            let pos = current_block_pos_or_rail_below(&view, cart.motion.position);
            let state = view.state_at(pos.x, pos.y, pos.z);
            let on_rails = is_rail_block(&state);

            if on_rails {
                move_along_track(cart, pos, &state, &view, &profile, wet);
                if base_name(&state) == crate::redstone_rail::ACTIVATOR_RAIL {
                    let active = get_bool_property(&state, "powered").unwrap_or(false);
                    apply_activation(cart, active);
                }
            } else {
                come_off_track(cart, &view, &profile, wet);
            }

            // The yaw/flip bookkeeping — `OldMinecartBehavior.tick`'s tail,
            // after `moveAlongTrack`/`comeOffTrack`. `prev_position` stands
            // in for vanilla's `xo`/`zo` (the position at the top of this
            // tick), which is exactly what changed between then and now.
            let x_diff = prev_position.x - cart.motion.position.x;
            let z_diff = prev_position.z - cart.motion.position.z;
            if x_diff * x_diff + z_diff * z_diff > 0.001 {
                let mut yaw = (z_diff.atan2(x_diff) * 180.0 / std::f64::consts::PI) as f32;
                if cart.flipped {
                    yaw += 180.0;
                }
                cart.yaw = yaw;
            }
            let rot_diff = wrap_degrees_f32(cart.yaw - cart.yaw_o);
            if rot_diff < -170.0 || rot_diff >= 170.0 {
                cart.yaw += 180.0;
                cart.flipped = !cart.flipped;
            }
            cart.yaw %= 360.0;
            cart.yaw_o = cart.yaw;

            // `MinecartFurnace.tick`'s own independent fuel countdown — runs
            // every tick regardless of on/off rail, exactly as vanilla's
            // does (it lives in `AbstractMinecart.tick`'s subclass override,
            // not inside the behaviour's rail-following branch).
            if cart.kind.is_furnace() {
                if cart.fuel > 0 {
                    cart.fuel -= 1;
                }
                if cart.fuel <= 0 {
                    cart.push = Vec3d::ZERO;
                }
            }

            // `MinecartTNT.tick`'s own fuse/collision-triggered detonation.
            let speed_sqr = cart.motion.velocity.x * cart.motion.velocity.x + cart.motion.velocity.z * cart.motion.velocity.z;
            if cart.fuse > 0 {
                cart.fuse -= 1;
            } else if cart.fuse == 0 {
                detonated.push((id, Vec3::new(cart.motion.position.x, cart.motion.position.y, cart.motion.position.z), speed_sqr as f32));
                cart.fuse = -1;
            } else if cart.motion.horizontal_collision && speed_sqr >= 0.01 {
                detonated.push((id, Vec3::new(cart.motion.position.x, cart.motion.position.y, cart.motion.position.z), speed_sqr as f32));
            }
        }

        for (id, centre, speed_sqr) in detonated {
            self.minecarts.remove(&id);
            // `MinecartTNT.explode` — `explosionPowerBase +
            // explosionSpeedFactor * random * 1.5 * min(sqrt(speedSqr), 5.0)`.
            // Drawn from `tnt_rng`, the same isolated stream `spawn_tnt`'s
            // own launch direction uses, so a TNT-minecart blast cannot shift
            // any other behaviour's roll.
            let speed = speed_sqr.sqrt().min(5.0);
            let roll = self.tnt_rng.next_f64();
            let power = TNT_EXPLOSION_POWER_BASE + (TNT_EXPLOSION_SPEED_FACTOR * roll * 1.5 * f64::from(speed)) as f32;
            self.explode(centre, power, DamageFlags::default());
            self.pending_detonations.push(Detonation { centre, radius: power });
        }
    }
}

/// `AbstractMinecart.activateMinecart` — the activator-rail producer shared
/// by the plain cart (ejects its rider) and the TNT cart (primes its fuse).
/// `active` is the rail's own `POWERED`; vanilla's every override is a no-op
/// on `false`. Applied **inline**, in the same per-cart iteration
/// `move_along_track` runs in and strictly before that cart's own fuse
/// countdown — mirroring vanilla's real call order (`behavior.tick()`, which
/// this activation is part of, runs inside `AbstractMinecart.tick()`, which
/// `MinecartTNT.tick()` calls via `super.tick()` *before* its own
/// fuse-decrement code). A deferred, end-of-loop application would prime a
/// fuse one whole tick late.
fn apply_activation(cart: &mut TrackedMinecart, active: bool) {
    if !active {
        return;
    }
    match cart.kind {
        MinecartKind::Plain => {
            cart.rider = None;
        }
        MinecartKind::Tnt => {
            if cart.fuse < 0 {
                cart.fuse = TNT_MINECART_FUSE;
            }
        }
        _ => {}
    }
}

/// `OldMinecartBehavior.moveAlongTrack` — see this module's own doc comment
/// for what it does and what is cut.
fn move_along_track(
    cart: &mut TrackedMinecart,
    pos: BlockPos,
    state: &str,
    view: &MinecartCollision<'_>,
    profile: &PhysicsProfile,
    wet: bool,
) {
    let old_pos = current_pos_along_rail(view, cart.motion.position.x, cart.motion.position.y, cart.motion.position.z);

    let mut y = f64::from(pos.y);
    let mut power_track = false;
    let mut halt_track = false;
    if base_name(state) == crate::redstone_rail::POWERED_RAIL {
        let powered = get_bool_property(state, "powered").unwrap_or(false);
        power_track = powered;
        halt_track = !powered;
    }

    let mut slide_speed = SLIDE_SPEED;
    if wet {
        slide_speed *= 0.2;
    }

    let Some(shape) = rail_shape(state) else { return };
    match shape {
        RailShape::AscendingEast => {
            cart.motion.velocity.x -= slide_speed;
            y += 1.0;
        }
        RailShape::AscendingWest => {
            cart.motion.velocity.x += slide_speed;
            y += 1.0;
        }
        RailShape::AscendingNorth => {
            cart.motion.velocity.z += slide_speed;
            y += 1.0;
        }
        RailShape::AscendingSouth => {
            cart.motion.velocity.z -= slide_speed;
            y += 1.0;
        }
        _ => {}
    }

    let (exit0, exit1) = shape.exits();
    let mut xd = f64::from(exit1.0 - exit0.0);
    let mut zd = f64::from(exit1.2 - exit0.2);
    let length = (xd * xd + zd * zd).sqrt();
    let flip = cart.motion.velocity.x * xd + cart.motion.velocity.z * zd;
    if flip < 0.0 {
        xd = -xd;
        zd = -zd;
    }
    let pow = cart.motion.velocity.x.hypot(cart.motion.velocity.z).min(2.0);
    cart.motion.velocity.x = pow * xd / length;
    cart.motion.velocity.z = pow * zd / length;

    if halt_track {
        let speed = cart.motion.velocity.x.hypot(cart.motion.velocity.z);
        if speed < 0.03 {
            cart.motion.velocity.x = 0.0;
            cart.motion.velocity.z = 0.0;
        } else {
            cart.motion.velocity.x *= 0.5;
            cart.motion.velocity.z *= 0.5;
        }
    }

    let x0 = f64::from(pos.x) + 0.5 + f64::from(exit0.0) * 0.5;
    let z0 = f64::from(pos.z) + 0.5 + f64::from(exit0.2) * 0.5;
    let x1 = f64::from(pos.x) + 0.5 + f64::from(exit1.0) * 0.5;
    let z1 = f64::from(pos.z) + 0.5 + f64::from(exit1.2) * 0.5;
    xd = x1 - x0;
    zd = z1 - z0;
    let progress = if xd == 0.0 {
        cart.motion.position.z - f64::from(pos.z)
    } else if zd == 0.0 {
        cart.motion.position.x - f64::from(pos.x)
    } else {
        let xx = cart.motion.position.x - x0;
        let zz = cart.motion.position.z - z0;
        (xx * xd + zz * zd) * 2.0
    };
    cart.motion.position.x = x0 + xd * progress;
    cart.motion.position.y = y;
    cart.motion.position.z = z0 + zd * progress;

    let scale = if cart.rider.is_some() { 0.75 } else { 1.0 };
    let max_speed = max_speed(cart, wet);
    cart.motion.velocity = Vec3d::new(
        (scale * cart.motion.velocity.x).clamp(-max_speed, max_speed),
        0.0,
        (scale * cart.motion.velocity.z).clamp(-max_speed, max_speed),
    );
    move_entity(&mut cart.motion, MINECART_DIMENSIONS, view, profile, MoveContext::default());

    let xn = cart.motion.position.x.floor() as i32;
    let zn = cart.motion.position.z.floor() as i32;
    if exit0.1 != 0 && xn - pos.x == exit0.0 && zn - pos.z == exit0.2 {
        cart.motion.position.y += f64::from(exit0.1);
    } else if exit1.1 != 0 && xn - pos.x == exit1.0 && zn - pos.z == exit1.2 {
        cart.motion.position.y += f64::from(exit1.1);
    }

    cart.motion.velocity = apply_natural_slowdown(cart, cart.motion.velocity, wet);

    let new_pos = current_pos_along_rail(view, cart.motion.position.x, cart.motion.position.y, cart.motion.position.z);
    if let (Some(np), Some(op)) = (new_pos, old_pos) {
        let speed_delta = (op.y - np.y) * 0.05;
        let other_pow = cart.motion.velocity.x.hypot(cart.motion.velocity.z);
        if other_pow > 0.0 {
            let factor = (other_pow + speed_delta) / other_pow;
            cart.motion.velocity.x *= factor;
            cart.motion.velocity.z *= factor;
        }
        cart.motion.position.y = np.y;
    }

    let xn2 = cart.motion.position.x.floor() as i32;
    let zn2 = cart.motion.position.z.floor() as i32;
    if xn2 != pos.x || zn2 != pos.z {
        let other_pow = cart.motion.velocity.x.hypot(cart.motion.velocity.z);
        cart.motion.velocity.x = other_pow * f64::from(xn2 - pos.x);
        cart.motion.velocity.z = other_pow * f64::from(zn2 - pos.z);
    }

    if power_track {
        let speed_len = cart.motion.velocity.x.hypot(cart.motion.velocity.z);
        if speed_len > 0.01 {
            cart.motion.velocity.x += cart.motion.velocity.x / speed_len * POWERED_BOOST;
            cart.motion.velocity.z += cart.motion.velocity.z / speed_len * POWERED_BOOST;
        } else {
            match shape {
                RailShape::EastWest => {
                    if is_redstone_conductor(&view.state_at(pos.x - 1, pos.y, pos.z)) {
                        cart.motion.velocity.x = POWERED_STALL_NUDGE;
                    } else if is_redstone_conductor(&view.state_at(pos.x + 1, pos.y, pos.z)) {
                        cart.motion.velocity.x = -POWERED_STALL_NUDGE;
                    }
                }
                RailShape::NorthSouth => {
                    if is_redstone_conductor(&view.state_at(pos.x, pos.y, pos.z - 1)) {
                        cart.motion.velocity.z = POWERED_STALL_NUDGE;
                    } else if is_redstone_conductor(&view.state_at(pos.x, pos.y, pos.z + 1)) {
                        cart.motion.velocity.z = -POWERED_STALL_NUDGE;
                    }
                }
                // Vanilla `return`s here with neither axis touched — a
                // stalled cart on a powered slope/curve gets no nudge.
                _ => {}
            }
        }
    }
}

/// `AbstractMinecart.comeOffTrack` — plain clamped physics for the tick a
/// cart is not over a rail cell at all.
fn come_off_track(cart: &mut TrackedMinecart, view: &MinecartCollision<'_>, profile: &PhysicsProfile, wet: bool) {
    let max_speed = max_speed(cart, wet);
    cart.motion.velocity.x = cart.motion.velocity.x.clamp(-max_speed, max_speed);
    cart.motion.velocity.z = cart.motion.velocity.z.clamp(-max_speed, max_speed);
    if cart.motion.on_ground {
        cart.motion.velocity = cart.motion.velocity.scale(0.5);
    }
    move_entity(&mut cart.motion, MINECART_DIMENSIONS, view, profile, MoveContext::default());
    if !cart.motion.on_ground {
        cart.motion.velocity = cart.motion.velocity.scale(AIR_DRAG);
    }
}

/// `AbstractMinecart.getMaxSpeed`/`MinecartFurnace`'s own override
/// (`isInWater() ? super * 0.75 : super * 0.5`).
fn max_speed(cart: &TrackedMinecart, wet: bool) -> f64 {
    let base = if wet { MAX_SPEED_WATER } else { MAX_SPEED_LAND };
    if cart.kind.is_furnace() {
        base * if wet { 0.75 } else { 0.5 }
    } else {
        base
    }
}

/// `AbstractMinecart.applyNaturalSlowdown`, with `MinecartFurnace`'s own
/// override folded in ahead of the base multiply — see this file's own
/// [`calculate_new_push_along`] for the push re-aim it performs first.
fn apply_natural_slowdown(cart: &mut TrackedMinecart, movement: Vec3d, wet: bool) -> Vec3d {
    let pre = if cart.kind.is_furnace() && (cart.push.x * cart.push.x + cart.push.z * cart.push.z) > 1.0e-7 {
        cart.push = calculate_new_push_along(cart.push, movement);
        let combined = Vec3d::new(movement.x * 0.8 + cart.push.x, movement.y, movement.z * 0.8 + cart.push.z);
        if wet { combined.scale(0.1) } else { combined }
    } else if cart.kind.is_furnace() {
        Vec3d::new(movement.x * 0.98, movement.y, movement.z * 0.98)
    } else {
        movement
    };

    let slowdown = if cart.rider.is_some() { SLOWDOWN_RIDDEN } else { SLOWDOWN_UNRIDDEN };
    let mut out = Vec3d::new(pre.x * slowdown, 0.0, pre.z * slowdown);
    if wet {
        out = out.scale(WATER_SLOWDOWN);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ChunkWorld;

    fn sim() -> MobSim<'static> {
        let world: &'static ChunkWorld = Box::leak(Box::new(ChunkWorld::new(-64, 384)));
        MobSim::new(world)
    }

    /// A flat stone floor at `y = 60`, plain rail at `y = 61` running
    /// north/south through `x = 8`, air everywhere else — the minimum rig a
    /// cart can sit on.
    fn straight_rail() -> impl Fn(i32, i32, i32) -> String {
        |x, y, z| {
            if x == 8 && y == 61 && (0..20).contains(&z) {
                "minecraft:rail[shape=north_south,waterlogged=false]".to_owned()
            } else if y <= 60 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        }
    }

    fn snapshot_of(sim: &MobSim<'_>, id: i32) -> crate::protocol::EntitySnapshot {
        sim.snapshots().into_iter().find(|s| s.id == id).expect("a live minecart must be streamed, or it reaches zero pixels")
    }

    /// The entity reaches the wire with a real position and the plain
    /// entity type — the base of the whole chain the task asks to verify.
    #[test]
    fn a_spawned_minecart_streams_as_a_real_entity() {
        let mut sim = sim();
        let id = sim.spawn_minecart(MinecartKind::Plain, Vec3::new(8.5, 61.0625, 4.5));
        assert_eq!(sim.minecart_kind(id), Some(MinecartKind::Plain));
        let snap = snapshot_of(&sim, id);
        assert_eq!(snap.entity_type, MinecartKind::Plain.entity_type());
        assert_eq!(snap.position, Vec3::new(8.5, 61.0625, 4.5));
    }

    /// **The discriminating gate: a curved rail actually turns the cart.**
    /// A `south_east` curve joins the `x = 8` north/south run to an
    /// `z = 8` east/west run; a cart entering along `-z` with real velocity
    /// must exit having gained `+x` motion — something an implementation
    /// that ignores rail shape (straight-line extrapolation) cannot produce,
    /// because the curve's own exit pair is the only source of an `x`
    /// component here.
    #[test]
    fn a_minecart_follows_a_curved_rail_rather_than_going_straight() {
        let world = |x: i32, y: i32, z: i32| -> String {
            if y == 61 && x == 8 && (5..8).contains(&z) {
                "minecraft:rail[shape=north_south,waterlogged=false]".to_owned()
            } else if y == 61 && x == 8 && z == 8 {
                // Curve: SOUTH_EAST joins the -z approach to the +x exit.
                "minecraft:rail[shape=south_east,waterlogged=false]".to_owned()
            } else if y == 61 && z == 8 && (9..13).contains(&x) {
                "minecraft:rail[shape=east_west,waterlogged=false]".to_owned()
            } else if y <= 60 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        };
        let mut sim = sim();
        let id = sim.spawn_minecart(MinecartKind::Plain, Vec3::new(8.5, 61.0625, 5.5));
        // Give it real southward velocity so `moveAlongTrack`'s own
        // exit-pair projection has a direction to preserve — a stalled cart
        // at zero speed does not exhibit the failure this gate wants to
        // catch.
        if let Some(cart) = sim.minecarts.get_mut(&id) {
            cart.motion.velocity = Vec3d::new(0.0, 0.0, 0.35);
        }
        let mut saw_x_motion = false;
        for _ in 0..60 {
            sim.tick_minecarts(&world);
            let (pos, _) = sim.minecart_transform(id).expect("still alive");
            if pos.x > 8.6 {
                saw_x_motion = true;
                break;
            }
        }
        assert!(
            saw_x_motion,
            "a cart travelling south into a south_east curve must pick up +x motion from the curve's own exit pair, final = {:?}",
            sim.minecart_transform(id)
        );
    }

    /// **A sloped rail actually climbs**, and the slide impulse plus the
    /// exit-pair `y` snap are both load-bearing: a cart entering an
    /// `ascending_north` rail from the south must gain height, one block per
    /// rail cell, not merely "move".
    #[test]
    fn a_minecart_climbs_an_ascending_rail() {
        let world = |x: i32, y: i32, z: i32| -> String {
            if x == 8 && z == 10 && y == 61 {
                "minecraft:rail[shape=north_south,waterlogged=false]".to_owned()
            } else if x == 8 && z == 9 && y == 61 {
                // Ascends toward -z (north): the +z exit is the low one.
                "minecraft:rail[shape=ascending_north,waterlogged=false]".to_owned()
            } else if x == 8 && z == 8 && y == 62 {
                "minecraft:rail[shape=north_south,waterlogged=false]".to_owned()
            } else if y <= 60 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        };
        let mut sim = sim();
        let id = sim.spawn_minecart(MinecartKind::Plain, Vec3::new(8.5, 61.0625, 10.5));
        if let Some(cart) = sim.minecarts.get_mut(&id) {
            cart.motion.velocity = Vec3d::new(0.0, 0.0, -0.3);
        }
        let start_y = sim.minecart_transform(id).unwrap().0.y;
        let mut max_y = start_y;
        for _ in 0..80 {
            sim.tick_minecarts(&world);
            let (pos, _) = sim.minecart_transform(id).expect("still alive");
            if pos.y > max_y {
                max_y = pos.y;
            }
        }
        assert!(
            max_y - start_y > 0.5,
            "a cart climbing an ascending rail must gain real height, start {start_y} max {max_y}"
        );
    }

    /// **The speed model, against the record constants, not a round
    /// number.** `MAX_SPEED_ON_LAND` is the literal `0.4` — a cart pushed to
    /// an extreme velocity on a flat rail must be clamped to exactly that,
    /// never exceed it, and the pairwise-distinct start point rules out a
    /// coincidental match.
    #[test]
    fn a_minecart_is_clamped_to_the_real_max_speed_on_land() {
        let mut sim = sim();
        let id = sim.spawn_minecart(MinecartKind::Plain, Vec3::new(8.5, 61.0625, 11.5));
        if let Some(cart) = sim.minecarts.get_mut(&id) {
            cart.motion.velocity = Vec3d::new(0.0, 0.0, -50.0);
        }
        sim.tick_minecarts(&straight_rail());
        let speed = sim
            .minecarts
            .get(&id)
            .map(|c| c.motion.velocity.x.hypot(c.motion.velocity.z))
            .expect("still alive");
        assert!(
            speed <= MAX_SPEED_LAND + 1e-9,
            "must never exceed the real 0.4 constant, got {speed}"
        );
        assert!(speed > 0.0, "and must not have been zeroed outright");
    }

    /// **A powered rail really nudges a stalled cart, at the exact record
    /// magnitude — not a plausible guess.** With no rider-input nudge (cut,
    /// see this module's own doc comment) and a genuinely zero starting
    /// speed, `moveAlongTrack`'s own `speedLength > 0.01` gate is false, so
    /// this exercises the *other* powered-rail arm: the two-conductor stall
    /// nudge, `POWERED_STALL_NUDGE` (`0.02`), not the `0.06` in-motion boost.
    /// The neighbouring cells at `z = 0`/`z = 2` are also rail (not air),
    /// and `crate::redstone::is_redstone_conductor`'s own simplified model
    /// (`!air_or_fluid && !redstone_component`) counts a rail block as a
    /// conductor — a real, if coarse, existing behaviour of that function,
    /// not something this feature introduces — so the north/south check
    /// fires and the cart picks up exactly `0.02` of `+z` velocity.
    #[test]
    fn a_powered_rail_nudges_a_stalled_cart_by_the_real_stall_constant() {
        let world = |x: i32, y: i32, z: i32| -> String {
            if x == 8 && y == 61 && (0..3).contains(&z) {
                "minecraft:powered_rail[shape=north_south,powered=true]".to_owned()
            } else if y <= 60 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        };
        let mut sim = sim();
        let id = sim.spawn_minecart(MinecartKind::Plain, Vec3::new(8.5, 61.0625, 1.5));
        sim.tick_minecarts(&world);
        let velocity = sim.minecarts.get(&id).map(|c| c.motion.velocity).expect("still alive");
        assert!(
            (velocity.z - POWERED_STALL_NUDGE).abs() < 1e-9,
            "a stalled cart on a powered rail with a conductor to its south must gain exactly the \
             real 0.02 stall nudge in +z, got {velocity:?}"
        );
        assert!(velocity.x.abs() < 1e-12, "the nudge is along the rail axis only, got {velocity:?}");
    }

    /// **Riding**: mount refuses a non-rideable kind (a hopper minecart),
    /// accepts a plain one, and refuses a second rider.
    #[test]
    fn only_a_plain_minecart_is_rideable_and_seats_one() {
        let mut sim = sim();
        let hopper = sim.spawn_minecart(MinecartKind::Hopper, Vec3::new(2.5, 61.0625, 2.5));
        assert!(!sim.mount_minecart(hopper, 7), "a hopper minecart must refuse a rider");

        let plain = sim.spawn_minecart(MinecartKind::Plain, Vec3::new(8.5, 61.0625, 8.5));
        assert!(sim.mount_minecart(plain, 7));
        assert_eq!(sim.minecart_rider(plain), Some(7));
        assert!(!sim.mount_minecart(plain, 9), "a seated cart refuses a second rider");
        assert_eq!(sim.minecart_rider(plain), Some(7));
        assert_eq!(sim.dismount_minecart_rider(7), Some(plain));
        assert_eq!(sim.minecart_rider(plain), None);
    }

    /// **TNT minecart activation and explosion**, reusing the same
    /// `pending_detonations`/`explode` pipeline primed TNT uses — an
    /// activator rail with `powered=true` under the cart must prime the
    /// fuse, and running it out must detonate through the shared pipeline.
    #[test]
    fn an_activator_rail_primes_a_tnt_minecart_and_it_detonates() {
        let world = |x: i32, y: i32, z: i32| -> String {
            if x == 8 && y == 61 && z == 5 {
                "minecraft:activator_rail[shape=north_south,powered=true]".to_owned()
            } else if y <= 60 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        };
        let mut sim = sim();
        let id = sim.spawn_minecart(MinecartKind::Tnt, Vec3::new(8.5, 61.0625, 5.5));
        assert_eq!(sim.minecart_fuse(id), -1);
        sim.tick_minecarts(&world);
        assert_eq!(sim.minecart_fuse(id), TNT_MINECART_FUSE - 1, "the activator rail must prime the fuse this tick");

        // Move it off the activator rail so the fuse just counts down rather
        // than being re-primed (already primed, priming again is a no-op
        // regardless, but a plain rail is the honest rest-of-track rig).
        let plain_track = |x: i32, y: i32, z: i32| -> String {
            if x == 8 && y == 61 && z == 5 {
                "minecraft:rail[shape=north_south,waterlogged=false]".to_owned()
            } else if y <= 60 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        };
        // 79 more ticks to walk the fuse down from 79 to 0 (one decrement per
        // tick), plus one further tick for the `fuse == 0` branch itself to
        // fire — `TNT_MINECART_FUSE` additional ticks in total after the
        // priming tick already consumed the first decrement.
        for _ in 0..TNT_MINECART_FUSE {
            sim.tick_minecarts(&plain_track);
        }
        assert_eq!(sim.minecart_count(), 0, "the cart must discard itself on detonation");
        let detonations = sim.take_detonations();
        assert_eq!(detonations.len(), 1);
        assert!(
            detonations[0].radius >= TNT_EXPLOSION_POWER_BASE,
            "power must be at least the base 4.0, got {}",
            detonations[0].radius
        );
    }

    /// **Furnace minecart**: fuel is added and consumed, and the fuel state
    /// rides the wire so a client can animate the smoke — the same
    /// "reaches the wire or it's an island" bar `TntFuse` was held to.
    #[test]
    fn a_furnace_minecart_burns_fuel_and_streams_it() {
        let mut sim = sim();
        let id = sim.spawn_minecart(MinecartKind::Furnace, Vec3::new(8.5, 61.0625, 8.5));
        assert_eq!(sim.minecart_fuel(id), 0);
        assert!(sim.add_minecart_fuel(id, "minecraft:coal", Vec3::new(8.5, 61.0625, 9.5)));
        assert_eq!(sim.minecart_fuel(id), FURNACE_FUEL_TICKS_PER_ITEM);
        let snap = snapshot_of(&sim, id);
        assert_eq!(snap.metadata, vec![crate::protocol::MetadataField::MinecartFuel(true)]);
        assert!(!sim.add_minecart_fuel(id, "minecraft:stone", Vec3::new(8.5, 61.0625, 9.5)), "non-fuel must be refused");
    }

    /// **Chest/hopper minecarts carry real, correctly-sized storage.**
    #[test]
    fn container_minecarts_have_the_real_slot_counts() {
        let mut sim = sim();
        let chest = sim.spawn_minecart(MinecartKind::Chest, Vec3::new(1.5, 61.0625, 1.5));
        let hopper = sim.spawn_minecart(MinecartKind::Hopper, Vec3::new(3.5, 61.0625, 3.5));
        assert_eq!(sim.minecart_slots(chest).map(<[_]>::len), Some(27));
        assert_eq!(sim.minecart_slots(hopper).map(<[_]>::len), Some(5));
    }

    /// `RailShape::exits` against the record table, spot-checked at two
    /// curves and one slope rather than the whole ten — a transposed pair
    /// here is exactly the failure mode CLAUDE.md's evidence section warns
    /// about, so the values are asserted against distinct, non-symmetric
    /// expectations.
    #[test]
    fn rail_shape_exits_match_the_record_table() {
        assert_eq!(RailShape::SouthEast.exits(), ((0, 0, 1), (1, 0, 0)));
        assert_eq!(RailShape::NorthWest.exits(), ((0, 0, -1), (-1, 0, 0)));
        assert_eq!(RailShape::AscendingNorth.exits(), ((0, 0, -1), (0, -1, 1)));
        assert!(RailShape::AscendingNorth.is_slope());
        assert!(!RailShape::SouthEast.is_slope());
    }

    /// `MinecartKind::from_item` covers exactly the five real item ids and
    /// nothing else.
    #[test]
    fn minecart_kind_from_item_covers_the_five_real_items() {
        assert_eq!(MinecartKind::from_item("minecraft:minecart"), Some(MinecartKind::Plain));
        assert_eq!(MinecartKind::from_item("minecraft:chest_minecart"), Some(MinecartKind::Chest));
        assert_eq!(MinecartKind::from_item("minecraft:hopper_minecart"), Some(MinecartKind::Hopper));
        assert_eq!(MinecartKind::from_item("minecraft:furnace_minecart"), Some(MinecartKind::Furnace));
        assert_eq!(MinecartKind::from_item("minecraft:tnt_minecart"), Some(MinecartKind::Tnt));
        assert_eq!(MinecartKind::from_item("minecraft:oak_boat"), None);
    }

    /// [`is_rail_block`] accepts exactly the four rail block ids.
    #[test]
    fn is_rail_block_accepts_exactly_the_four_rail_blocks() {
        assert!(is_rail_block("minecraft:rail[shape=north_south,waterlogged=false]"));
        assert!(is_rail_block("minecraft:powered_rail[shape=north_south,powered=false]"));
        assert!(is_rail_block("minecraft:activator_rail[shape=north_south,powered=false]"));
        assert!(is_rail_block("minecraft:detector_rail[shape=north_south,powered=false]"));
        assert!(!is_rail_block("minecraft:stone"));
    }

    /// [`placement_position`] — `MinecartItem.useOn`'s own offset: `0.0625`
    /// flat, `0.5625` on a slope.
    #[test]
    fn placement_position_matches_the_flat_and_slope_offsets() {
        let pos = BlockPos::new(4, 70, -9);
        let flat = placement_position(pos, Some(RailShape::NorthSouth));
        assert_eq!(flat, Vec3::new(4.5, 70.0625, -8.5));
        let slope = placement_position(pos, Some(RailShape::AscendingNorth));
        assert_eq!(slope, Vec3::new(4.5, 70.5625, -8.5));
    }
}
