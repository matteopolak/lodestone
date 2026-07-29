//! Client-side entity interpolation: turning the 20 Hz stream of
//! [`EntityView`](lodestone_client::EntityView) snapshots into smooth per-frame
//! render transforms.
//!
//! The server reports entity positions at tick rate (20 Hz); the shell draws at
//! 50–120 fps. Snapping each mob to its latest reported position would stutter
//! visibly, so — exactly as vanilla does — we render *behind* the latest
//! snapshot and ease from the previous position to the current one over a fixed
//! window. When a fresh position arrives we start the new interpolation from
//! where the mob is *currently being drawn*, not from the stale target, so
//! motion is C0-continuous and never jumps.
//!
//! # Why the window is three ticks, not one
//!
//! Vanilla eases entity movement over **three** ticks, not one. Its
//! `InterpolationHandler` (26.2 client) sets `DEFAULT_INTERPOLATION_STEPS = 3`
//! and `interpolateTo` resets the step counter to 3 on every position packet,
//! then `interpolate()` consumes `1/steps` of the remaining gap each of the next
//! three client ticks. The consequence is load-bearing: the server only sends a
//! movement packet when a mob's position *changes*, so packets routinely arrive
//! less often than once per tick. If the ease completes in a single tick (50 ms)
//! the mob reaches its target and then **sits frozen** until the next packet —
//! move, freeze, move, freeze — which reads as "not interpolated" even though
//! interpolation is running. A three-tick (150 ms) window keeps the mob gliding
//! across the gap between sparse packets, matching vanilla's feel. A continuous
//! linear ease over three ticks is the faithful continuous form of vanilla's
//! discrete `alpha = 1/steps` schedule (its per-tick positions land on 1/3, 2/3,
//! 1 — linearly spaced).
//!
//! # Why the walk cycle is measured off the *drawn* position
//!
//! Vanilla's `updateWalkAnimation` feeds `min(distance * 4, 1)` where `distance`
//! is how far the entity moved **this tick**. The tempting local quantity is the
//! gap a fresh snapshot opens up — "the mob was here, the server says it is now
//! there" — and it is wrong by exactly [`INTERP_STEPS`].
//!
//! Steady state, with a mob walking `v` blocks per tick and a packet every tick:
//! each tick the drawn position closes `1/3` of the outstanding gap `g`, while
//! the target runs on by `v`, so `g' = (2/3)g + v` and `g` settles at `3v`. Feed
//! `3v` to `walk_target_speed` and the amplitude saturates at three times the
//! speed it should, and since `WalkAnimation::position` accumulates `speed` per
//! tick, the *phase* advances up to 3× too fast as well — legs that swing both
//! too far and far too quickly, which is precisely how it was reported.
//!
//! Sampling `render_pos` once per 20 Hz tick measures `v` instead, because that
//! is what vanilla is measuring: on the client the entity's own position has
//! already been advanced by `InterpolationHandler`, so `getX() - xo` is the
//! *interpolated* step, not the packet delta. The two agree under dense packets
//! and under sparse ones, which the gap measure never does.
//!
//! This module is deliberately GPU-free, so the interpolation is unit-testable
//! without a device or a server: the sim converts each
//! [`EntityView`](lodestone_client::EntityView) into an [`EntitySnapshot`]
//! (version-free, `glam`-only aside from the physics dependency below) and
//! feeds those in. The output is a flat list of [`EntityDraw`]s — type path,
//! feet position, body yaw and scale — that the renderer resolves into
//! instanced draws.
//!
//! # Why dropped items get their own physics, not just an ease
//!
//! A **dropped item is not eased between position packets like every other
//! entity** — it is simulated. `ItemEntity`'s `EntityType` registers
//! `updateInterval(20)` and vanilla's `ServerEntity.sendChanges` only
//! re-evaluates whether to send a position/motion packet at all once every
//! `updateInterval` ticks (or immediately on a ground-state change, or when
//! `needsSync`/dirty metadata forces it) — so **an airborne item gets exactly
//! one position correction per second**, not one per tick like the module docs
//! above assume for mobs. Easing that one-per-second correction over the usual
//! three-tick (150 ms) window reproduces precisely the reported defect: the
//! item spawns at the right spot, sits rendered at that spot for ~850 ms while
//! nothing arrives to ease toward, then snaps through a 150 ms ease to wherever
//! gravity has since carried it on the server — which reads as "pops out right,
//! then teleports down" instead of arcing.
//!
//! Vanilla's own client does not treat this as an interpolation problem: it
//! ticks `ItemEntity.tick()` locally every client tick, exactly like the
//! server does, driven by the velocity `set_entity_motion`/`add_entity` report
//! and the same gravity/drag constants — the rare server correction just
//! nudges the local simulation back onto the authoritative track. This module
//! does the same for entities whose `type_path` is [`ITEM_ENTITY_TYPE_PATH`]:
//! [`Track::item_physics`] runs [`step_item_physics`]
//! (gravity `0.04`, air drag `0.98` — [`lodestone_entity::item_entity`]'s
//! vanilla constants, not reimplemented) once per real 20 Hz tick, and the
//! render ease ([`Track::t`] / [`INTERP_WINDOW`]) is re-anchored off *that*
//! simulated position each tick rather than off the sparse network packet. A
//! server correction (when one arrives) resets the simulated
//! position/velocity to the authoritative value rather than fighting it.
//! While the last-known snapshot reports the item at rest on the ground, the
//! simulation is paused rather than resimulated needlessly — see
//! [`EntitySnapshot::on_ground`].
//!
//! # Collision: falling through the floor between corrections
//!
//! [`step_item_physics`] moves the item through
//! [`lodestone_physics::move_entity`] — the same shared collision core the
//! player uses, not a second collider — rather than
//! [`lodestone_entity::item_entity::ItemMotion::tick`]'s bare `position +=
//! velocity`. Without a collision query an airborne item only had the
//! server's once-a-second correction to keep it out of the ground, and
//! visibly sank through blocks in between; [`EntityInterpolator::update`]
//! (the default, used by tests and any caller with no world) still has no
//! world to query and keeps the old free-fall behaviour, but
//! [`EntityInterpolator::update_with_view`] — what [`crate::sim::Sim`]
//! actually drives — resolves real collision every tick. This is bounded by
//! the `view`'s own coverage (the live path's is the loaded-chunk radius
//! around the player), not global: a drop far outside that radius still
//! free-falls until it is back in range, same as before this existed.
//!
//! Every other entity type is unaffected: [`Track::item_physics`] stays `None`
//! and the original pure position ease runs exactly as before.

use std::collections::HashMap;

use glam::Vec3;
use lodestone_assets::ResourceLocation;
use lodestone_entity::item_entity::{ITEM_AIR_DRAG, ITEM_GRAVITY, ItemMotion};
use lodestone_entity::pose::{
    ADULT_LIMB_SCALE, BABY_LIMB_SCALE, LIMB_SWING_SMOOTHING, MAX_HEAD_YAW, WalkAnimation,
    clamp_head_to_body, walk_target_speed,
};
use lodestone_model::event::EquipmentSlot;
use lodestone_physics::{
    CollisionView, EntityDimensions, EntityMotion, MoveContext, PhysicsProfile, Vec3d, mth,
    move_entity,
};
use lodestone_render::AnimInput;

/// Converts a render-space [`glam::Vec3`] into the `f64` [`lodestone_model::Vec3`]
/// [`ItemMotion`] is expressed in.
fn to_model_vec3(v: Vec3) -> lodestone_model::Vec3 {
    lodestone_model::Vec3::new(f64::from(v.x), f64::from(v.y), f64::from(v.z))
}

/// Converts an [`ItemMotion`]-space `f64` [`lodestone_model::Vec3`] into the
/// [`Vec3d`] [`lodestone_physics::move_entity`] is expressed in. Both are plain
/// `{x, y, z}` `f64` triples from different crates — this is a field copy, not a
/// unit conversion.
fn to_physics_vec3d(v: lodestone_model::Vec3) -> Vec3d {
    Vec3d::new(v.x, v.y, v.z)
}

/// The inverse of [`to_physics_vec3d`].
fn from_physics_vec3d(v: Vec3d) -> lodestone_model::Vec3 {
    lodestone_model::Vec3::new(v.x, v.y, v.z)
}

/// A dropped item's collision hitbox: `EntityTypes.ITEM` is `sized(0.25F,
/// 0.25F)`, and `ItemEntity` (not a `LivingEntity`) never overrides
/// `Entity.maxUpStep()`, whose base implementation returns `0.0F` — items do
/// not auto-step at all.
const ITEM_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.25, 0.25, 0.0);

/// `ItemEntity.getBlockPosBelowThatAffectsMyMovement()` — the block an item
/// reads ground friction from. This overrides the base entity's
/// `getOnPos(0.500001F)` with `getOnPos(0.999999F)`: an item's 0.25-tall
/// hitbox sits *inside* the block it rests on, not straddling the block
/// below, so the friction sample has to reach almost a full block down, not
/// half of one. Reproduced here (rather than reused from
/// `lodestone_physics::player::friction_block`) because that helper is
/// `pub(crate)` to the physics crate *and* bakes in the different,
/// generic-entity `0.500001F` offset — the wrong constant for an item even if
/// it were reachable.
fn item_friction_block(position: Vec3d) -> (i32, i32, i32) {
    (
        mth::floor(position.x),
        mth::floor(position.y - f64::from(0.999_999_f32)),
        mth::floor(position.z),
    )
}

/// One tick of a dropped item's *own* client-run physics, run against a real
/// [`CollisionView`] instead of [`ItemMotion::tick`]'s bare `position +=
/// velocity`. Without this an airborne item only ever gets a floor from the
/// server's own once-a-second correction (`EntityTypes.ITEM`'s
/// `updateInterval(20)`) and visibly sinks through terrain in between — the
/// gap the module docs on [`ItemPhysics`] call out.
///
/// Mirrors `ItemEntity.tick()`'s real order, traced against
/// `net/minecraft/world/entity/item/ItemEntity.java` in the 26.2 decompile:
/// gravity is subtracted from `velocity.y` *before* the move
/// (`applyGravity()`); [`move_entity`] is vanilla's own
/// `Entity.move(MoverType.SELF, deltaMovement)` — the single shared collider
/// this crate must not fork, per the module's own architecture note — and it
/// both commits the collided position and derives this tick's authoritative
/// `on_ground` (not last tick's stale server-reported value, which is what
/// `on_ground` held before this fix); drag is then applied to the
/// *post-collision* velocity exactly as `ItemEntity.tick()` re-reads
/// `getDeltaMovement()` after `move()`, using the real block friction under
/// the item via [`CollisionView::friction`] rather than [`ItemMotion`]'s
/// unqueried constant; and the `-0.5` landing bounce follows drag, matching
/// vanilla's tail end of the branch exactly.
///
/// `profile` supplies only the land-bounce threshold's gravity term inside
/// [`move_entity`] (relevant solely if the item ever rests on a bouncy block
/// like slime) — items have their own gravity/drag constants
/// ([`ITEM_GRAVITY`]/[`ITEM_AIR_DRAG`]) applied explicitly here, so passing
/// the player's [`PhysicsProfile`] does not mix the two up for the fall
/// itself, only for that one rare edge case.
fn step_item_physics(sim: &mut ItemMotion, view: &dyn CollisionView, profile: &PhysicsProfile) {
    // `ItemEntity.applyGravity()`, before the move.
    sim.velocity.y -= ITEM_GRAVITY;

    let mut motion = EntityMotion {
        position: to_physics_vec3d(sim.position),
        velocity: to_physics_vec3d(sim.velocity),
        on_ground: sim.on_ground,
        horizontal_collision: false,
        stuck_speed_multiplier: Vec3d::ZERO,
    };
    // The shared collision core, not a second one — `MoveContext::default()`
    // matches an item: never Slow Falling, never bounce-suppressing (that
    // flag is the sneaking-player case, `LivingEntity`-only).
    move_entity(&mut motion, ITEM_DIMENSIONS, view, profile, MoveContext::default());

    // Drag on the post-collision velocity, exactly as `ItemEntity.tick()`
    // scales `getDeltaMovement()` after `move()` returns.
    let mut ground_friction = ITEM_AIR_DRAG;
    if motion.on_ground {
        let (fx, fy, fz) = item_friction_block(motion.position);
        ground_friction *= f64::from(view.friction(fx, fy, fz));
    }
    motion.velocity.x *= ground_friction;
    motion.velocity.z *= ground_friction;
    motion.velocity.y *= ITEM_AIR_DRAG;
    if motion.on_ground && motion.velocity.y < 0.0 {
        motion.velocity.y *= -0.5;
    }

    sim.position = from_physics_vec3d(motion.position);
    sim.velocity = from_physics_vec3d(motion.velocity);
    sim.on_ground = motion.on_ground;
}

/// A [`CollisionView`] with no collision boxes anywhere — open air forever.
///
/// Backs [`EntityInterpolator::update`]'s no-world-known default so every
/// existing caller (tests, and any future offline/no-net path) keeps the
/// pre-collision free-fall behaviour unchanged. The live path does not use
/// this — see [`EntityInterpolator::update_with_view`], which
/// [`crate::sim::Sim`] drives with a real [`CollisionView`] built from the
/// player's loaded chunks.
struct OpenAir;

impl CollisionView for OpenAir {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
}

/// Converts an [`ItemMotion`]-space `f64` [`lodestone_model::Vec3`] back into
/// render-space [`glam::Vec3`].
fn to_glam_vec3(v: lodestone_model::Vec3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

/// One physics tick, in seconds.
const TICK: f32 = 0.05;

/// Vanilla's `InterpolationHandler::DEFAULT_INTERPOLATION_STEPS`: entity moves
/// ease over three ticks, not one. See the module docs for why a one-tick window
/// reads as "not interpolated" against the server's sparse move packets.
const INTERP_STEPS: f32 = 3.0;

/// The interpolation window in seconds: `TICK * INTERP_STEPS` (150 ms). A fresh
/// snapshot is reached this long after it arrives, re-anchored from the current
/// render pose so motion stays continuous.
const INTERP_WINDOW: f32 = TICK * INTERP_STEPS;

/// Position change (blocks) below which a snapshot is treated as "no movement",
/// so idle mobs don't restart their interpolation clock every frame.
/// Seconds per server tick, the cadence the walk animation is advanced at.
const TICK_SECONDS: f32 = TICK;
/// Server ticks per second, for the continuous `ageInTicks` clock.
const TICKS_PER_SECOND: f32 = 20.0;

const POS_EPS: f32 = 1.0e-4;

/// Yaw change (degrees) below which a snapshot is treated as "no turn". Applies
/// to body yaw, head yaw and pitch alike.
const YAW_EPS: f32 = 1.0e-2;

/// A version-free entity snapshot as reported by the client for one tick. Built
/// by the sim from an [`EntityView`](lodestone_client::EntityView); carries only
/// what the renderer needs, in glam types, so this module needs no client or
/// model dependency.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySnapshot {
    /// The server-assigned entity id (interpolation key).
    pub id: i32,
    /// The entity type's canonical path (e.g. `"pig"`), for model resolution.
    pub type_path: String,
    /// Feet position in world space.
    pub feet: Vec3,
    /// Body yaw in degrees.
    pub yaw: f32,
    /// Head yaw in degrees (absolute). Tracked separately from the body: a
    /// walking mob keeps its body facing its movement while its head turns to
    /// track a target, so this is never derived from `yaw`.
    pub head_yaw: f32,
    /// Head pitch in degrees (look up/down).
    pub pitch: f32,
    /// Uniform render scale (baby mobs are drawn smaller).
    pub scale: f32,
    /// Which item a dropped item (or other item-displaying entity) is showing.
    ///
    /// Nested exactly like the read-model's own field: the outer `Option` is
    /// "has the server ever reported a stack for this entity", `Some(None)` is
    /// an explicitly *empty* stack. `None` therefore means "unknown", and
    /// [`EntityInterpolator::update`] leaves any previously recorded stack
    /// alone rather than clearing it — a drop names itself once and then goes
    /// quiet, so treating silence as "empty" would blank it a frame later.
    ///
    /// This is a [`ResourceLocation`], not a model `ItemStack`: `EntitySnapshot`
    /// is deliberately model-free, and the renderer only ever needs the item
    /// *id* to pick a model. The stack's `count` and data components are dropped
    /// at the boundary that builds this (`net::entity_snapshot`) — see the note
    /// there, since count is visible in vanilla.
    ///
    /// This is a `Reported<T>`-shaped field
    /// (`lodestone_model::event::Reported`) in every way but its actual type:
    /// it should be `Reported<ResourceLocation>`, naming "never reported" vs
    /// "explicitly cleared" instead of nesting `Option`, but retyping it here
    /// alone does not compile. The producer is `net::entity_snapshot` in
    /// `crates/lodestone-shell/src/net.rs` (outside this change's scope),
    /// which builds the nested-`Option` `item` local by matching on
    /// `EntityView::item` — itself `Option<Option<ItemStack>>` in
    /// `lodestone-client`, one crate further out. A follow-up that touches
    /// `net.rs` (and, to go all the way to the wire, `lodestone-client`) in
    /// the same pass as this field can finish the conversion.
    pub item: Option<Option<ResourceLocation>>,
    /// The entity's last-reported velocity in blocks per tick
    /// (`set_entity_motion`/`add_entity`), when the server has ever sent one.
    ///
    /// This is what [`Track::item_physics`] seeds and re-anchors its ballistic
    /// simulation from — see the module docs on why a dropped item needs real
    /// physics rather than a position ease. `None` is "never reported", not
    /// "zero"; a zero velocity is reported as `Some(Vec3::ZERO)`.
    pub velocity: Option<Vec3>,
    /// Whether the server last reported this entity resting on the ground
    /// (`on_ground` on `add_entity`/`teleport_entity`/`move_entity`).
    ///
    /// [`Track::item_physics`] pauses its simulation while this is `true`,
    /// because this module has no world/collision query to know *where* the
    /// ground is — see the module docs.
    pub on_ground: bool,
    /// What this entity is wearing and holding, keyed by slot, as
    /// `SET_EQUIPMENT` last reported it.
    ///
    /// The inner `Option` is the *slot's* nesting, not the field's: a slot
    /// **absent** from this list is "the server has never mentioned it", while a
    /// slot present with `None` is an explicit "this slot is empty". That is
    /// [`EntityView::equipment`](lodestone_client::EntityView::equipment)'s
    /// contract preserved verbatim, and it is why this is a list of pairs rather
    /// than a fixed-size array of `Option`s.
    ///
    /// The whole list is *accumulated server-side of this type*
    /// (`lodestone_client::state` merges each update into the view and never
    /// clears), so every snapshot carries the complete current set and
    /// [`EntityInterpolator::update_with_view`] replaces its record wholesale —
    /// unlike [`Self::item`], which arrives once and must never be cleared by
    /// silence.
    ///
    /// Only `MainHand`/`OffHand` reach a pixel today; see [`EntityDraw::equipment`].
    pub equipment: Vec<(EquipmentSlot, Option<ResourceLocation>)>,
}

/// The entity-type path a **dropped item** reports (`minecraft:item`).
///
/// It has no [`entity_models`](lodestone_render::EntityModelSet) entry and never
/// will: an item entity is not a cuboid part rig, it is an *item model* drawn in
/// the world. `EntityModelSet::resolve` therefore skips it, which is why a drop
/// reaches [`EntityDraw`] but no pixels — the renderer picks these out by type
/// path and draws them through the model pipeline instead.
pub const ITEM_ENTITY_TYPE_PATH: &str = "item";

/// A single entity ready to draw this frame: its model type and interpolated
/// transform inputs. The renderer turns this into an
/// [`EntityInstance`](lodestone_render::EntityInstance).
#[derive(Debug, Clone, PartialEq)]
pub struct EntityDraw {
    /// The server-assigned entity id. Carried through to the draw because a
    /// dropped item's bob/spin phase is derived from it
    /// ([`item_bob_offset`](lodestone_render::entity::item_bob_offset)) — vanilla rolls
    /// that phase from a client RNG we cannot observe, so a stable hash of the
    /// id stands in for it.
    pub id: i32,
    /// The entity type's canonical path (e.g. `"pig"`).
    pub type_path: String,
    /// For a dropped item ([`ITEM_ENTITY_TYPE_PATH`]), which item's model to
    /// draw. `None` for every other entity type, and also for an item entity
    /// whose stack has not been reported — see
    /// [`EntityInterpolator::set_item_stack`] for why that is currently every
    /// one of them.
    pub item: Option<ResourceLocation>,
    /// What this entity is holding/wearing, narrowed to the slots that actually
    /// have something in them: an entry here means "there is an item in this
    /// slot", so the renderer needs no second `Option` check.
    ///
    /// **Only `MainHand` and `OffHand` can reach a pixel.** The renderer poses
    /// those off the arm part matrix
    /// ([`lodestone_render::entity::held_item_matrix`]) and deliberately leaves
    /// the six armour/`Body`/`Saddle` slots unhandled rather than faking them:
    /// vanilla draws armour from a *separate humanoid mesh set* baked at two
    /// inflations (`HumanoidArmorModel`'s inner/outer `CubeDeformation`), plus
    /// trim overlays and leather dye tinting. The `entity_models` corpus has 81
    /// models and no armour layer at all, so there is no geometry to hang a
    /// helmet on — an armour slot needs new meshes, not new plumbing. Passing
    /// them through here anyway keeps the *data* honest and makes the gap
    /// visible at the point of use.
    ///
    /// Order follows [`EquipmentSlot::ALL`] only by accident of what the server
    /// sent; treat it as an unordered set.
    pub equipment: Vec<(EquipmentSlot, ResourceLocation)>,
    /// Interpolated feet position in world space.
    pub feet: Vec3,
    /// Interpolated body yaw in degrees.
    pub yaw: f32,
    /// Interpolated head yaw in degrees (absolute), for head tracking.
    pub head_yaw: f32,
    /// Interpolated head pitch in degrees.
    pub pitch: f32,
    /// Uniform render scale.
    pub scale: f32,
    /// Per-part animation drive (head tracking, walk cycle, idle age), already
    /// interpolated for this frame and in the units
    /// [`Skeleton::pose`](lodestone_render::Skeleton::pose) expects — note
    /// `head_yaw_deg` is **relative to the body**, matching vanilla's
    /// `netHeadYaw`.
    pub anim: AnimInput,
}

/// Per-entity interpolation track: the position/yaw we are easing *from*, the
/// latest reported target we are easing *to*, and how far through the tick we
/// are.
#[derive(Debug, Clone)]
struct Track {
    type_path: String,
    scale: f32,
    prev: Vec3,
    curr: Vec3,
    prev_yaw: f32,
    curr_yaw: f32,
    prev_head_yaw: f32,
    curr_head_yaw: f32,
    prev_pitch: f32,
    curr_pitch: f32,
    /// Seconds since the ease was last re-anchored, capped at [`INTERP_WINDOW`].
    t: f32,
    /// Vanilla's `WalkAnimationState`, ticked at 20 Hz.
    walk: WalkAnimation,
    /// The drawn position at the previous 20 Hz walk tick. The distance between
    /// this and the current drawn position *is* the per-tick travel
    /// [`walk_target_speed`] wants — see the module note on why the eased gap is
    /// not.
    walk_pos: Vec3,
    /// Continuous age in ticks (`ageInTicks`), driving idle bob.
    age: f32,
    /// Ballistic simulation state for a dropped item ([`ITEM_ENTITY_TYPE_PATH`]);
    /// `None` for every other entity type, which keeps the original pure
    /// position ease. See the module docs for why items need this.
    item_physics: Option<ItemPhysics>,
    /// The occupied equipment slots, narrowed from
    /// [`EntitySnapshot::equipment`]. Lives on the track rather than in a side
    /// map (as [`EntityInterpolator::item_stacks`] does) precisely *because* it
    /// is replaced wholesale every poll: there is no "reported once, then
    /// silence" hazard to guard against, so there is nothing for a separate
    /// map's `retain` to protect, and hanging it here means a despawn prunes it
    /// for free.
    equipment: Vec<(EquipmentSlot, ResourceLocation)>,
}

/// Narrows a snapshot's per-slot equipment to the slots that actually hold an
/// item — dropping both the never-reported slots (absent already) and the
/// explicitly-empty ones, which draw nothing either way.
fn occupied_equipment(
    equipment: &[(EquipmentSlot, Option<ResourceLocation>)],
) -> Vec<(EquipmentSlot, ResourceLocation)> {
    equipment
        .iter()
        .filter_map(|(slot, item)| item.clone().map(|id| (*slot, id)))
        .collect()
}

/// A dropped item's client-run physics: the same gravity/drag
/// [`ItemMotion`] the server itself steps, advanced once per real 20 Hz tick
/// and corrected toward each authoritative server report rather than driven
/// by it.
#[derive(Debug, Clone, Copy)]
struct ItemPhysics {
    /// The locally-simulated position/velocity.
    sim: ItemMotion,
    /// The most recently reported *authoritative* feet position — kept
    /// separate from [`Track::curr`], which the simulation itself advances
    /// every tick, so re-polling the same still-current server value doesn't
    /// look like a fresh "moved" event every frame.
    last_reported: Vec3,
    /// Whether the last-reported snapshot said the item is resting. The
    /// simulation is paused while `true` — a resting item does not need
    /// resimulating every tick, and this avoids any drift between the local
    /// collision result and the server's own resting position. See
    /// [`step_item_physics`] for the (now collision-aware) airborne case.
    grounded: bool,
}

/// Seeds a fresh [`ItemPhysics`] from an item entity's first-seen (or
/// freshly re-anchored) snapshot. A missing velocity seeds zero — gravity
/// still applies to it, it just has nothing to arc with, which is exactly
/// the discriminating behaviour the hermetic tests below pin.
fn new_item_physics(snap: &EntitySnapshot) -> ItemPhysics {
    let mut sim = ItemMotion::new(
        to_model_vec3(snap.feet),
        snap.velocity.map(to_model_vec3).unwrap_or_default(),
    );
    sim.on_ground = snap.on_ground;
    ItemPhysics {
        sim,
        last_reported: snap.feet,
        grounded: snap.on_ground,
    }
}

impl Track {
    /// The fraction `[0, 1]` through the current interpolation window.
    fn alpha(&self) -> f32 {
        (self.t / INTERP_WINDOW).clamp(0.0, 1.0)
    }

    /// The currently-drawn position: `prev` eased toward `curr` by `alpha`.
    fn render_pos(&self) -> Vec3 {
        self.prev.lerp(self.curr, self.alpha())
    }

    /// The currently-drawn body yaw, taking the shortest arc so a wrap across
    /// 360° (e.g. 350°→10°) turns +20° rather than −340°.
    fn render_yaw(&self) -> f32 {
        lerp_angle(self.prev_yaw, self.curr_yaw, self.alpha())
    }

    /// The currently-drawn head yaw, shortest-arc like the body yaw.
    fn render_head_yaw(&self) -> f32 {
        lerp_angle(self.prev_head_yaw, self.curr_head_yaw, self.alpha())
    }

    /// The currently-drawn head pitch. Pitch is bounded to ±90° and never wraps,
    /// so a plain linear ease is correct.
    fn render_pitch(&self) -> f32 {
        self.prev_pitch + (self.curr_pitch - self.prev_pitch) * self.alpha()
    }

    /// The animation drive for this frame.
    ///
    /// `partial_tick` is the fraction through the current 50 ms tick, used for
    /// the walk cycle exactly as vanilla's `WalkAnimationState` interpolation.
    /// The head yaw is clamped to the body (`Mob.clampHeadRotationToBody`) and
    /// then expressed *relative* to it, because that is what
    /// `LivingEntityRenderer` feeds `setupAnim` — passing the absolute value
    /// would spin every mob's head with its body.
    fn render_anim(&self, partial_tick: f32) -> AnimInput {
        let body = self.render_yaw();
        let head = clamp_head_to_body(body, self.render_head_yaw(), MAX_HEAD_YAW);
        AnimInput {
            head_yaw_deg: wrap_degrees(head - body),
            head_pitch_deg: self.render_pitch(),
            limb_swing: self.walk.position_lerp(partial_tick),
            limb_swing_amount: self.walk.speed_lerp(partial_tick),
            attack_anim: 0.0,
            age_ticks: self.age,
            // `Mob.isAggressive` rides a shared-flags bit nothing decodes yet.
            aggressive: false,
        }
    }
}

/// Wraps degrees into `(-180, 180]`, like `Mth.wrapDegrees`.
fn wrap_degrees(deg: f32) -> f32 {
    angle_diff(deg, 0.0)
}

/// Tracks and interpolates every visible entity between server ticks.
#[derive(Debug, Default)]
pub struct EntityInterpolator {
    tracks: HashMap<i32, Track>,
    /// Seconds accumulated toward the next 20 Hz animation tick.
    tick_accum: f32,
    /// Which item each dropped-item entity is carrying, keyed by entity id.
    /// Pruned alongside [`Self::tracks`], so a despawned drop leaves nothing
    /// behind.
    item_stacks: HashMap<i32, ResourceLocation>,
}

impl EntityInterpolator {
    /// A fresh interpolator with no tracked entities.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record which item a dropped-item entity is carrying, so its
    /// [`EntityDraw`] can name a model to draw.
    ///
    /// # Where the live path calls this
    ///
    /// [`Self::update`] does, from [`EntitySnapshot::item`], for every snapshot
    /// that carries a stack — the full live chain is
    /// `ITEM_STACK` metadata (index 8) → `EntityMetadataUpdate::item` →
    /// `EntityView::item` → `net::entity_snapshot` → here. It stays a public
    /// setter because it is also the direct seam for tests and for any caller
    /// that learns an item's identity outside the snapshot stream.
    ///
    /// An item entity with no entry here draws nothing, which is also what
    /// vanilla does with an empty stack (`ItemEntityRenderer.submit` returns
    /// early on `state.item.isEmpty()`).
    pub fn set_item_stack(&mut self, entity_id: i32, item: ResourceLocation) {
        self.item_stacks.insert(entity_id, item);
    }

    /// Forget the item recorded for `entity_id`, so it draws as an empty stack.
    ///
    /// Only reached when the server *explicitly* reports an empty stack; a
    /// snapshot that is merely silent about the item leaves the record alone.
    pub fn clear_item_stack(&mut self, entity_id: i32) {
        self.item_stacks.remove(&entity_id);
    }

    /// The item recorded for `entity_id`, if any.
    #[must_use]
    pub fn item_stack(&self, entity_id: i32) -> Option<&ResourceLocation> {
        self.item_stacks.get(&entity_id)
    }

    /// [`Self::update_with_view`] against [`OpenAir`] and a default
    /// [`PhysicsProfile`] — i.e. the pre-collision behaviour, kept as the
    /// default entry point for tests and any caller with no world to query.
    ///
    /// **Not what the live path uses.** [`crate::sim::Sim`] calls
    /// [`Self::update_with_view`] directly with a real [`CollisionView`], so a
    /// dropped item's fall actually stops at a floor — see that method's docs
    /// and the module docs on why an item needs its own physics at all.
    pub fn update(&mut self, snapshots: &[EntitySnapshot], dt: f32) {
        self.update_with_view(snapshots, dt, &OpenAir, &PhysicsProfile::mc_1_21());
    }

    /// Advance every track by `dt` seconds, then fold in this frame's snapshots.
    ///
    /// Entities absent from `snapshots` are dropped (despawned/out of range).
    /// A snapshot whose position or yaw differs from the current target starts a
    /// new interpolation *from the current render pose*, so the mob never jumps.
    /// A snapshot that matches the current target only lets the existing ease
    /// run to completion.
    ///
    /// `view`/`profile` feed only [`step_item_physics`] (every other entity is
    /// a pure position ease and never touches either). `view` should cover
    /// wherever a tracked item entity actually is — `Sim::live_collision`'s
    /// 3×3-column-around-the-player snapshot is the intended source, so a
    /// drop far outside that radius still free-falls with no floor until it
    /// re-enters range, same as before this method existed.
    pub fn update_with_view(
        &mut self,
        snapshots: &[EntitySnapshot],
        dt: f32,
        view: &dyn CollisionView,
        profile: &PhysicsProfile,
    ) {
        // Advance existing clocks first, so a snapshot that resets `t` to 0 this
        // frame starts the new window from exactly its previous render pose.
        for track in self.tracks.values_mut() {
            track.t = (track.t + dt).min(INTERP_WINDOW);
            track.age += dt * TICKS_PER_SECOND;
        }

        // Advance the walk cycle on a fixed 20 Hz clock rather than per frame:
        // vanilla's `WalkAnimationState` is a tick-rate state machine, and
        // driving it per frame would make the swing speed depend on frame rate.
        self.tick_accum += dt;
        while self.tick_accum >= TICK_SECONDS {
            self.tick_accum -= TICK_SECONDS;
            for track in self.tracks.values_mut() {
                // Step a dropped item's own ballistic physics once per real
                // tick — the server itself only *corrects* this roughly once a
                // second (`ItemEntity`'s `updateInterval(20)`), so the arc has
                // to come from here, not from easing toward a sparse packet.
                // See the module docs. Paused while the last report says the
                // item is resting (no world query here to know the floor) —
                // once `step_item_physics` derives its own authoritative
                // `on_ground` every tick, only the *server's* report still
                // gates whether the sim runs at all; the floor within a tick
                // is now real collision, not a frozen flag.
                if track.item_physics.as_ref().is_some_and(|p| !p.grounded) {
                    // Scoped so the mutable borrow of `item_physics` ends
                    // before `render_pos()` below needs to borrow all of
                    // `track` — ticking and reading `render_pos` can't happen
                    // in the same borrow.
                    let sim_pos = {
                        let physics = track
                            .item_physics
                            .as_mut()
                            .expect("checked Some above");
                        step_item_physics(&mut physics.sim, view, profile);
                        physics.sim.position
                    };
                    // Re-anchor exactly like a fresh authoritative snapshot
                    // would: ease from wherever this frame is currently drawn
                    // toward the freshly simulated point, so the simulation
                    // reads as continuous motion rather than a series of
                    // per-tick snaps.
                    track.prev = track.render_pos();
                    track.curr = to_glam_vec3(sim_pos);
                    track.t = 0.0;
                }

                // Vanilla's `updateWalkAnimation` measures `this.getX() - this.xo`
                // — how far the entity moved *this tick*. On the client that
                // entity has already been advanced by `InterpolationHandler`, so
                // the quantity is the per-tick step of the **interpolated**
                // position, which is what `render_pos` is here. Sampling it once
                // per 20 Hz tick reproduces vanilla under both dense and sparse
                // move packets, and needs no separate "has it stopped?" rule: a
                // mob that stops stops moving `render_pos`, so the distance goes
                // to zero and the amplitude decays on its own.
                let now = track.render_pos();
                let distance = (now - track.walk_pos).with_y(0.0).length();
                track.walk_pos = now;
                let limb_scale = if track.scale < 1.0 {
                    BABY_LIMB_SCALE
                } else {
                    ADULT_LIMB_SCALE
                };
                track
                    .walk
                    .update(walk_target_speed(distance), LIMB_SWING_SMOOTHING, limb_scale);
            }
        }

        for snap in snapshots {
            // Fold the reported identity first, so a drop is never drawn for a
            // frame as a placeholder before its item lands. `None` is "this
            // snapshot does not know", which must not clear what an earlier one
            // established; only an explicit empty stack clears.
            match &snap.item {
                Some(Some(item)) => self.set_item_stack(snap.id, item.clone()),
                Some(None) => self.clear_item_stack(snap.id),
                None => {}
            }
            let is_item = snap.type_path == ITEM_ENTITY_TYPE_PATH;
            match self.tracks.get_mut(&snap.id) {
                None => {
                    // A newly seen entity is drawn at rest at its reported pose.
                    self.tracks.insert(
                        snap.id,
                        Track {
                            type_path: snap.type_path.clone(),
                            scale: snap.scale,
                            prev: snap.feet,
                            curr: snap.feet,
                            prev_yaw: snap.yaw,
                            curr_yaw: snap.yaw,
                            prev_head_yaw: snap.head_yaw,
                            curr_head_yaw: snap.head_yaw,
                            prev_pitch: snap.pitch,
                            curr_pitch: snap.pitch,
                            t: INTERP_WINDOW,
                            walk: WalkAnimation::new(),
                            walk_pos: snap.feet,
                            age: 0.0,
                            item_physics: is_item.then(|| new_item_physics(snap)),
                            equipment: occupied_equipment(&snap.equipment),
                        },
                    );
                }
                Some(track) => {
                    track.type_path.clone_from(&snap.type_path);
                    track.scale = snap.scale;
                    // **Outside** the `moved || turned` gate below, deliberately.
                    // Equipment changes with no movement at all — a mob picking up
                    // a dropped sword, a plugin swapping a villager's hat, the
                    // player's own hotbar switch mirrored back — and gating this on
                    // motion would leave a stationary mob holding whatever it was
                    // holding when it last took a step.
                    track.equipment = occupied_equipment(&snap.equipment);
                    // A dropped item's own simulation moves `curr` every real
                    // tick (see the physics step above), so comparing against
                    // `track.curr` here would read as "moved" every single
                    // frame even when the server has said nothing new since
                    // the last poll. Compare against the last *authoritative*
                    // report instead — `track.curr` only for every other
                    // entity type, which the physics step never touches.
                    let moved = match &track.item_physics {
                        Some(physics) => (snap.feet - physics.last_reported).length() > POS_EPS,
                        None => (snap.feet - track.curr).length() > POS_EPS,
                    };
                    let turned = angle_diff(snap.yaw, track.curr_yaw).abs() > YAW_EPS;
                    let head_turned =
                        angle_diff(snap.head_yaw, track.curr_head_yaw).abs() > YAW_EPS;
                    let pitched = (snap.pitch - track.curr_pitch).abs() > YAW_EPS;
                    if moved || turned || head_turned || pitched {
                        // Re-anchor the ease at where the mob is drawn right now.
                        track.prev = track.render_pos();
                        track.prev_yaw = track.render_yaw();
                        track.prev_head_yaw = track.render_head_yaw();
                        track.prev_pitch = track.render_pitch();
                        track.curr = snap.feet;
                        track.curr_yaw = snap.yaw;
                        track.curr_head_yaw = snap.head_yaw;
                        track.curr_pitch = snap.pitch;
                        track.t = 0.0;

                        if is_item {
                            match &mut track.item_physics {
                                Some(physics) => {
                                    physics.last_reported = snap.feet;
                                    physics.grounded = snap.on_ground;
                                    // Correct the simulation to the
                                    // authoritative truth rather than fight
                                    // it — this is the "rare server
                                    // correction" vanilla's own local
                                    // simulation also just snaps onto.
                                    physics.sim.position = to_model_vec3(snap.feet);
                                    if let Some(v) = snap.velocity {
                                        physics.sim.velocity = to_model_vec3(v);
                                    }
                                    physics.sim.on_ground = snap.on_ground;
                                }
                                None => track.item_physics = Some(new_item_physics(snap)),
                            }
                        }
                    }
                }
            }
        }

        // Drop tracks for entities no longer reported — and the item stacks
        // recorded against them, or a long session leaks one entry per drop.
        let seen: std::collections::HashSet<i32> = snapshots.iter().map(|s| s.id).collect();
        self.tracks.retain(|id, _| seen.contains(id));
        self.item_stacks.retain(|id, _| seen.contains(id));
    }

    /// The interpolated draw list for this frame. Order is unspecified (grouped
    /// by model downstream), so no ordering guarantees are made here.
    #[must_use]
    pub fn draws(&self) -> Vec<EntityDraw> {
        self.tracks
            .iter()
            .map(|(id, t)| EntityDraw {
                id: *id,
                type_path: t.type_path.clone(),
                item: (t.type_path == ITEM_ENTITY_TYPE_PATH)
                    .then(|| self.item_stacks.get(id).cloned())
                    .flatten(),
                feet: t.render_pos(),
                yaw: t.render_yaw(),
                head_yaw: t.render_head_yaw(),
                pitch: t.render_pitch(),
                scale: t.scale,
                anim: t.render_anim((self.tick_accum / TICK_SECONDS).clamp(0.0, 1.0)),
                equipment: t.equipment.clone(),
            })
            .collect()
    }

    /// Number of entities currently tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Whether no entities are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

/// The signed shortest difference `a − b` mapped into `(−180, 180]` degrees.
fn angle_diff(a: f32, b: f32) -> f32 {
    let mut d = (a - b) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    d
}

/// Interpolate between two angles along the shortest arc.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    a + angle_diff(b, a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: i32, feet: Vec3, yaw: f32) -> EntitySnapshot {
        EntitySnapshot {
            id,
            type_path: "pig".into(),
            feet,
            yaw,
            head_yaw: yaw,
            pitch: 0.0,
            scale: 1.0,
            item: None,
            velocity: None,
            on_ground: false,
            equipment: Vec::new(),
        }
    }

    #[test]
    fn a_new_entity_is_drawn_at_its_reported_pose() {
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::new(3.0, 64.0, -2.0), 90.0)], 0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1);
        assert_eq!(draws[0].feet, Vec3::new(3.0, 64.0, -2.0));
        assert_eq!(draws[0].yaw, 90.0);
    }

    #[test]
    fn movement_interpolates_rather_than_snapping() {
        let mut interp = EntityInterpolator::new();
        // Establish the entity at the origin, its ease already complete.
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], INTERP_WINDOW);
        // A new position arrives 4 blocks along +X.
        let target = Vec3::new(4.0, 0.0, 0.0);
        interp.update(&[snap(1, target, 0.0)], 0.0);

        // At t≈0 the mob must still be at (near) the old pose — NOT snapped to
        // the target. This is the anti-vacuity guard: a renderer that ignored
        // interpolation and drew the latest snapshot would already be at x=4.
        let x0 = interp.draws()[0].feet.x;
        assert!(
            x0 < 0.5,
            "on a fresh snapshot the mob must start from its old pose, was x={x0}"
        );

        // Half the window later it must be strictly between old and new — a snap
        // (jump straight to 4) or a freeze (stuck at 0) both fail this.
        interp.update(&[snap(1, target, 0.0)], INTERP_WINDOW / 2.0);
        let xm = interp.draws()[0].feet.x;
        assert!(
            xm > 0.5 && xm < 3.5,
            "half the window in, the mob should be mid-way, was x={xm}"
        );

        // A full window after the snapshot it reaches the target.
        interp.update(&[snap(1, target, 0.0)], INTERP_WINDOW);
        let xf = interp.draws()[0].feet.x;
        assert!(
            (xf - 4.0).abs() < 1.0e-3,
            "after the window it arrives, was x={xf}"
        );
    }

    #[test]
    fn a_single_tick_move_keeps_gliding_between_sparse_packets() {
        // The bug this module was fixed for: with a one-tick ease window, a mob
        // whose move packets arrive less often than every tick reaches its target
        // in 50 ms and then freezes until the next packet — a visible stutter.
        // With vanilla's three-tick window it must still be advancing a full tick
        // after the last packet. This is the regression guard on INTERP_STEPS.
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], INTERP_WINDOW);
        // One packet: the mob steps one block. No further packets arrive.
        interp.update(&[snap(1, Vec3::new(1.0, 0.0, 0.0), 0.0)], 0.0);

        // Sample the drawn x each render frame for the next three ticks at 60 fps
        // and require it to keep increasing well past the first tick — a one-tick
        // window would have plateaued at x=1 by 50 ms.
        let frame = 1.0 / 60.0;
        let mut last = interp.draws()[0].feet.x;
        let mut advanced_after_one_tick = false;
        let mut elapsed = 0.0;
        while elapsed < INTERP_WINDOW - 1.0e-4 {
            interp.update(&[snap(1, Vec3::new(1.0, 0.0, 0.0), 0.0)], frame);
            elapsed += frame;
            let x = interp.draws()[0].feet.x;
            assert!(
                x + 1.0e-4 >= last,
                "drawn x must never step backwards, {last} -> {x}"
            );
            if elapsed > TICK + frame && x > last + 1.0e-5 {
                advanced_after_one_tick = true;
            }
            last = x;
        }
        assert!(
            advanced_after_one_tick,
            "the mob must still be moving after the first tick (was it, x plateaued at {last}?)"
        );
        assert!(
            (last - 1.0).abs() < 1.0e-3,
            "after the full window the mob should have reached the target, was {last}"
        );
    }

    #[test]
    fn a_despawned_entity_stops_being_drawn() {
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::ZERO, 0.0), snap(2, Vec3::X, 0.0)], 0.016);
        assert_eq!(interp.len(), 2);
        // Entity 2 vanishes from the report.
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], 0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1, "the despawned entity must be gone");
    }

    #[test]
    fn yaw_interpolates_along_the_shortest_arc_across_the_wrap() {
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::ZERO, 350.0)], INTERP_WINDOW);
        // Turn to 10°: the short way is +20° through 360/0, not −340° through 180.
        interp.update(&[snap(1, Vec3::ZERO, 10.0)], 0.0);
        interp.update(&[snap(1, Vec3::ZERO, 10.0)], INTERP_WINDOW / 2.0);
        let y = interp.draws()[0].yaw;
        // Halfway along the +20° arc from 350° is 360° ≡ 0°. Reject the long-way
        // answer (~180°), which is what naive linear lerp would give.
        let near_zero = y.rem_euclid(360.0);
        let dist = near_zero.min(360.0 - near_zero);
        assert!(dist < 5.0, "yaw should pass through ~0°, was {y}");
    }

    #[test]
    fn head_yaw_interpolates_independently_of_the_body() {
        // A mob can turn its head without turning its body; the interpolator must
        // ease head yaw separately and along the shortest arc. A snapshot that
        // changes only head yaw (body and position unchanged) must still animate.
        let mut interp = EntityInterpolator::new();
        let mut s = snap(1, Vec3::ZERO, 0.0);
        s.head_yaw = 350.0;
        interp.update(std::slice::from_ref(&s), INTERP_WINDOW);
        // Head turns to 10° (short arc +20° through 0), body stays at 0.
        s.head_yaw = 10.0;
        interp.update(std::slice::from_ref(&s), 0.0);
        interp.update(std::slice::from_ref(&s), INTERP_WINDOW / 2.0);
        let d = &interp.draws()[0];
        assert!(
            d.yaw.abs() < 1.0e-3,
            "body yaw must stay put while only the head turns, was {}",
            d.yaw
        );
        let near_zero = d.head_yaw.rem_euclid(360.0);
        let dist = near_zero.min(360.0 - near_zero);
        assert!(dist < 5.0, "head yaw should pass through ~0°, was {}", d.head_yaw);
    }

    /// Drive a mob at a steady `v` blocks/tick for `ticks` server ticks, one
    /// packet per tick and one render frame per tick, and report the walk
    /// amplitude and the phase advanced over the last ten ticks.
    fn walk_at(v: f32, ticks: usize) -> (f32, f32) {
        let mut interp = EntityInterpolator::new();
        let mut pos = Vec3::ZERO;
        interp.update(&[snap(1, pos, 0.0)], INTERP_WINDOW);
        let mut phase_at_mark = 0.0;
        let mark = ticks.saturating_sub(10);
        for i in 0..ticks {
            pos.x += v;
            interp.update(&[snap(1, pos, 0.0)], TICK);
            if i == mark {
                phase_at_mark = interp.draws()[0].anim.limb_swing;
            }
        }
        let d = &interp.draws()[0].anim;
        (d.limb_swing_amount, d.limb_swing - phase_at_mark)
    }

    /// The reported defect: legs swing far too fast.
    ///
    /// Vanilla's amplitude is `min(distance * 4, 1)` on the **per-tick** travel.
    /// This walks a mob at a fixed speed and checks the amplitude the animator
    /// actually receives against that closed form. The measure this replaced
    /// used the interpolation *gap*, which settles at `3 * v` (see the module
    /// docs) — at `v = 0.05` that is `min(0.6, 1) = 0.6` instead of `0.2`, and
    /// because `WalkAnimation::position` accumulates the amplitude every tick,
    /// the phase ran 3× fast as well. Both halves are asserted, since fixing the
    /// amplitude without the phase would leave the legs still visibly quick.
    #[test]
    fn limb_swing_tracks_per_tick_travel_not_the_interpolation_gap() {
        for v in [0.02f32, 0.05, 0.1] {
            let (amount, phase_10) = walk_at(v, 120);
            let want = walk_target_speed(v);
            assert!(
                (amount - want).abs() < 0.05,
                "at {v} blocks/tick the amplitude should settle near vanilla's {want}, got \
                 {amount}. The old gap-based measure gives {} — a factor of {INTERP_STEPS}",
                walk_target_speed(v * INTERP_STEPS)
            );
            // Phase advances by `speed` per tick, so ten ticks is ~10 * amount.
            let want_phase = want * 10.0;
            assert!(
                (phase_10 - want_phase).abs() < want_phase * 0.25 + 0.05,
                "at {v} blocks/tick the phase advanced {phase_10} over ten ticks, expected \
                 ~{want_phase} — the leg cycle frequency is wrong, not just its amplitude"
            );
        }
    }

    /// The control the assertion above needs: at a walking speed *below*
    /// vanilla's saturation point the amplitude must be strictly less than 1.
    /// The old measure saturated at a third of the travel, so every mob that
    /// moved at all swung its legs at full throw — which is why the test above
    /// cannot be satisfied by simply clamping.
    #[test]
    fn a_slow_walk_does_not_saturate_the_limb_swing() {
        let (slow, _) = walk_at(0.05, 120);
        let (fast, _) = walk_at(0.30, 120);
        assert!(
            slow < 0.5,
            "a 0.05 blocks/tick amble swung at amplitude {slow}; vanilla gives 0.2"
        );
        assert!(
            fast > 0.95,
            "a 0.30 blocks/tick sprint should still saturate, got {fast}"
        );
        assert!(slow < fast);
    }

    #[test]
    fn a_mob_that_stops_walking_decays_to_standing() {
        let mut interp = EntityInterpolator::new();
        let mut pos = Vec3::ZERO;
        interp.update(&[snap(1, pos, 0.0)], INTERP_WINDOW);
        for _ in 0..40 {
            pos.x += 0.1;
            interp.update(&[snap(1, pos, 0.0)], TICK);
        }
        assert!(interp.draws()[0].anim.limb_swing_amount > 0.2, "was walking");
        // The mob stops: same position reported for two seconds.
        for _ in 0..40 {
            interp.update(&[snap(1, pos, 0.0)], TICK);
        }
        let amount = interp.draws()[0].anim.limb_swing_amount;
        assert!(
            amount < 0.01,
            "a standing mob still swings at {amount} — it will moonwalk on the spot"
        );
    }

    #[test]
    fn pitch_interpolates_linearly() {
        let mut interp = EntityInterpolator::new();
        let mut s = snap(1, Vec3::ZERO, 0.0);
        s.pitch = -30.0;
        interp.update(std::slice::from_ref(&s), INTERP_WINDOW);
        s.pitch = 30.0;
        interp.update(std::slice::from_ref(&s), 0.0);
        interp.update(std::slice::from_ref(&s), INTERP_WINDOW / 2.0);
        let p = interp.draws()[0].pitch;
        assert!(p.abs() < 1.0, "half the window from -30 to 30 is ~0, was {p}");
    }

    // ---- equipment -------------------------------------------------------

    fn sword() -> ResourceLocation {
        "minecraft:diamond_sword".parse().expect("valid item id")
    }

    fn shield() -> ResourceLocation {
        "minecraft:shield".parse().expect("valid item id")
    }

    #[test]
    fn equipment_reaches_the_draw_and_drops_the_empty_slots() {
        let mut s = snap(1, Vec3::ZERO, 0.0);
        s.equipment = vec![
            (EquipmentSlot::MainHand, Some(sword())),
            (EquipmentSlot::OffHand, Some(shield())),
            // Explicitly empty: reported, but there is nothing to draw, so the
            // draw list must not carry it.
            (EquipmentSlot::Head, None),
        ];
        let mut interp = EntityInterpolator::new();
        interp.update(std::slice::from_ref(&s), 0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1);
        let eq = &draws[0].equipment;
        assert_eq!(eq.len(), 2, "only the occupied slots reach the draw: {eq:?}");
        assert!(eq.contains(&(EquipmentSlot::MainHand, sword())));
        assert!(eq.contains(&(EquipmentSlot::OffHand, shield())));
        assert!(
            eq.iter().all(|(slot, _)| *slot != EquipmentSlot::Head),
            "an explicitly-empty slot must not reach the draw"
        );
    }

    #[test]
    fn equipment_updates_on_a_mob_that_has_not_moved() {
        // The specific defect this rules out: `type_path`/`scale` are folded
        // unconditionally while position/yaw only re-anchor when the mob actually
        // moved. Putting equipment inside that gate would mean a stationary mob
        // handed a sword keeps empty hands until it takes a step, which is the
        // common case for a `/give`-style test and for any mob standing still.
        let mut s = snap(1, Vec3::new(4.0, 64.0, 4.0), 90.0);
        let mut interp = EntityInterpolator::new();
        interp.update(std::slice::from_ref(&s), 0.016);
        assert!(interp.draws()[0].equipment.is_empty());

        // Identical pose, new equipment.
        s.equipment = vec![(EquipmentSlot::MainHand, Some(sword()))];
        interp.update(std::slice::from_ref(&s), 0.016);
        assert_eq!(
            interp.draws()[0].equipment,
            vec![(EquipmentSlot::MainHand, sword())],
            "equipment must not be gated on movement"
        );

        // ...and a wholesale replacement can take it away again, still without
        // moving. This is safe precisely because `EntityView::equipment` is the
        // accumulated set, never a delta.
        s.equipment = vec![(EquipmentSlot::MainHand, None)];
        interp.update(std::slice::from_ref(&s), 0.016);
        assert!(
            interp.draws()[0].equipment.is_empty(),
            "an explicit clear must disarm the mob"
        );
    }

    #[test]
    fn a_despawned_mob_leaves_no_equipment_behind() {
        let mut s = snap(1, Vec3::ZERO, 0.0);
        s.equipment = vec![(EquipmentSlot::MainHand, Some(sword()))];
        let mut interp = EntityInterpolator::new();
        interp.update(std::slice::from_ref(&s), 0.016);
        assert_eq!(interp.draws()[0].equipment.len(), 1);
        interp.update(&[], 0.016);
        assert!(interp.is_empty(), "the track itself must be pruned");
        assert!(interp.draws().is_empty());
    }

    // ---- dropped items ---------------------------------------------------

    /// An item entity whose stack the server has not (yet) reported, and
    /// which has never reported a velocity — the pre-physics fallback path.
    fn item_snap(id: i32, feet: Vec3) -> EntitySnapshot {
        EntitySnapshot {
            id,
            type_path: ITEM_ENTITY_TYPE_PATH.into(),
            feet,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            item: None,
            velocity: None,
            on_ground: false,
            equipment: Vec::new(),
        }
    }

    /// The same, carrying a reported stack, as the live path builds it.
    fn item_snap_with(id: i32, feet: Vec3, item: Option<ResourceLocation>) -> EntitySnapshot {
        EntitySnapshot {
            item: Some(item),
            ..item_snap(id, feet)
        }
    }

    /// A dropped item's snapshot as the live path actually builds it once
    /// `add_entity`/`set_entity_motion` have been decoded: position, velocity
    /// (when the server has reported one) and ground state.
    fn item_snap_moving(
        id: i32,
        feet: Vec3,
        velocity: Option<Vec3>,
        on_ground: bool,
    ) -> EntitySnapshot {
        EntitySnapshot {
            velocity,
            on_ground,
            ..item_snap(id, feet)
        }
    }

    fn stone() -> ResourceLocation {
        "minecraft:stone".parse().expect("valid item id")
    }

    #[test]
    fn an_item_entity_without_a_reported_stack_names_no_model() {
        // The current live state, asserted rather than assumed: the entity is
        // tracked and interpolated like any other, but nothing knows what it is,
        // so the renderer draws nothing — vanilla's own empty-stack behaviour.
        let mut interp = EntityInterpolator::new();
        interp.update(&[item_snap(9, Vec3::new(1.0, 64.0, 2.0))], 0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1, "an item entity must still be tracked");
        assert_eq!(draws[0].type_path, ITEM_ENTITY_TYPE_PATH);
        assert_eq!(draws[0].item, None);
        assert_eq!(draws[0].id, 9);
    }

    #[test]
    fn a_reported_stack_reaches_the_draw() {
        let mut interp = EntityInterpolator::new();
        interp.set_item_stack(9, stone());
        interp.update(&[item_snap(9, Vec3::new(1.0, 64.0, 2.0))], 0.016);
        assert_eq!(interp.draws()[0].item, Some(stone()));
    }

    #[test]
    fn a_stack_is_only_attached_to_the_entity_it_was_reported_for() {
        // The failure this rules out: keying the lookup on anything but the
        // entity id (position, insertion order) makes every drop in a pile show
        // the first one's model.
        let mut interp = EntityInterpolator::new();
        interp.set_item_stack(9, stone());
        interp.update(
            &[item_snap(9, Vec3::ZERO), item_snap(10, Vec3::X)],
            0.016,
        );
        let draws = interp.draws();
        let with = draws.iter().filter(|d| d.item.is_some()).count();
        assert_eq!(with, 1, "only entity 9 was told what it is carrying");
        assert_eq!(
            draws.iter().find(|d| d.id == 9).unwrap().item,
            Some(stone())
        );
        assert_eq!(draws.iter().find(|d| d.id == 10).unwrap().item, None);
    }

    #[test]
    fn a_non_item_entity_never_carries_a_stack() {
        // A stale entry for a recycled id must not turn a pig into a stone.
        // Servers reuse entity ids freely, so the type-path guard is what stops
        // one drop's identity leaking onto the mob that inherits its id.
        let mut interp = EntityInterpolator::new();
        interp.set_item_stack(1, stone());
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], 0.016);
        assert_eq!(interp.draws()[0].item, None);
    }

    #[test]
    fn a_despawned_drop_takes_its_stack_with_it() {
        // Item entities are the highest-churn entity there is (every broken
        // block makes one, every one despawns after five minutes), so a stack
        // table that only grows is a real leak, not a theoretical one.
        let mut interp = EntityInterpolator::new();
        interp.set_item_stack(9, stone());
        interp.update(&[item_snap(9, Vec3::ZERO)], 0.016);
        assert!(interp.item_stack(9).is_some());
        interp.update(&[], 0.016);
        assert!(
            interp.item_stack(9).is_none(),
            "the stack must be pruned with the track it belonged to"
        );
    }

    #[test]
    fn a_snapshot_that_carries_a_stack_needs_no_setter_call() {
        // The live wiring: nothing calls `set_item_stack` by hand any more, the
        // identity rides the snapshot from the metadata decode.
        let mut interp = EntityInterpolator::new();
        interp.update(&[item_snap_with(9, Vec3::ZERO, Some(stone()))], 0.016);
        assert_eq!(interp.draws()[0].item, Some(stone()));
    }

    #[test]
    fn a_snapshot_silent_about_the_item_keeps_the_known_one() {
        // The regression this rules out is the whole reason `EntitySnapshot`'s
        // item is nested: a drop reports its stack once at spawn and is silent
        // in every later metadata packet. Reading that silence as "empty" makes
        // the drop flicker into a placeholder one frame after it appeared.
        let mut interp = EntityInterpolator::new();
        interp.update(&[item_snap_with(9, Vec3::ZERO, Some(stone()))], 0.016);
        interp.update(&[item_snap(9, Vec3::ZERO)], 0.016);
        assert_eq!(
            interp.draws()[0].item,
            Some(stone()),
            "an unknowing snapshot must not erase a reported stack"
        );
    }

    #[test]
    fn an_explicitly_empty_stack_clears_the_known_one() {
        // The other half of the nesting: `Some(None)` is the server saying the
        // stack is empty, which vanilla draws as nothing.
        let mut interp = EntityInterpolator::new();
        interp.update(&[item_snap_with(9, Vec3::ZERO, Some(stone()))], 0.016);
        interp.update(&[item_snap_with(9, Vec3::ZERO, None)], 0.016);
        assert_eq!(interp.draws()[0].item, None);
    }

    #[test]
    fn a_drop_interpolates_and_ages_like_any_other_entity() {
        // The bob and spin are driven by `anim.age_ticks`, so an item whose age
        // never advanced would hang motionless in the air.
        let mut interp = EntityInterpolator::new();
        interp.update(&[item_snap(9, Vec3::ZERO)], 0.0);
        let first = interp.draws()[0].anim.age_ticks;
        interp.update(&[item_snap(9, Vec3::ZERO)], 0.5);
        let later = interp.draws()[0].anim.age_ticks;
        assert!(
            later > first + 9.0,
            "half a second must advance the age by ~10 ticks; {first} -> {later}"
        );
    }

    // ---- ballistic item drops (the reported defect) ----------------------
    //
    // Vanilla's `ItemEntity` registers `updateInterval(20)`
    // (`EntityTypes.ITEM`), so `ServerEntity.sendChanges` only re-evaluates a
    // position/motion send once every 20 ticks while the item is airborne —
    // roughly one correction per second, not one per tick. These tests feed a
    // spawn (with vanilla's real pop velocity, `ItemEntity`'s zero-arg
    // constructor: `vy = 0.2`, `vx/vz` up to `±0.1` blocks/tick) and then keep
    // driving the clock **without** any further position snapshot, exactly
    // matching that sparse-correction reality, and check the render position
    // for a real parabola.

    #[test]
    fn item_pop_follows_a_ballistic_arc_not_a_flat_ease() {
        // The discriminating assertion: an apex strictly above spawn height,
        // plus real horizontal displacement. A straight-line position ease
        // cannot produce an apex — it can only ever move monotonically toward
        // (or sit frozen at) the one target it has, which is exactly the
        // "pops out right, then teleports down" defect being fixed here.
        let mut interp = EntityInterpolator::new();
        let spawn = Vec3::new(10.0, 64.0, -5.0);
        let vel = Vec3::new(0.08, 0.2, 0.0);
        interp.update(
            &[item_snap_moving(9, spawn, Some(vel), false)],
            0.0,
        );

        let mut max_y = interp.draws()[0].feet.y;
        // 40 ticks (2s) of real flight time with no further server packet —
        // matching the ~1/s correction cadence, this window has none at all.
        for _ in 0..40 {
            interp.update(&[item_snap_moving(9, spawn, Some(vel), false)], TICK);
            max_y = max_y.max(interp.draws()[0].feet.y);
        }
        let final_feet = interp.draws()[0].feet;

        assert!(
            max_y > spawn.y + 0.05,
            "expected a real apex above the spawn height {}; got max_y={max_y}",
            spawn.y
        );
        assert!(
            (final_feet.x - spawn.x).abs() > 0.5,
            "expected real horizontal displacement from the popped velocity; dx={}",
            final_feet.x - spawn.x
        );
    }

    #[test]
    fn item_pop_without_velocity_never_rises_above_spawn_apex_control() {
        // The negative control the apex assertion above needs: with no
        // velocity ever reported, `Track::item_physics` still exists (gravity
        // alone still applies — see `new_item_physics`) but there is nothing
        // to arc with, so the render position must never rise. This is the
        // discriminator actually firing, not just described: an assertion
        // that can't fail proves nothing.
        let mut interp = EntityInterpolator::new();
        let spawn = Vec3::new(0.0, 64.0, 0.0);
        interp.update(&[item_snap_moving(9, spawn, None, false)], 0.0);

        let mut max_y = interp.draws()[0].feet.y;
        for _ in 0..40 {
            interp.update(&[item_snap_moving(9, spawn, None, false)], TICK);
            max_y = max_y.max(interp.draws()[0].feet.y);
        }
        assert!(
            max_y <= spawn.y + 1.0e-3,
            "no reported velocity means no apex is possible; got max_y={max_y}"
        );
    }

    #[test]
    fn item_pop_position_only_snapshots_produce_no_apex_either() {
        // A second negative control: a spawn with no reported velocity,
        // followed by a single late position correction that reports the item
        // now grounded — no snapshot in this test ever carries a velocity, so
        // there is still nothing to arc with (only gravity, which cannot
        // rise). The render position must never exceed the spawn height, and
        // the eventual correction must still be a smooth ease onto the
        // reported position, not a snap.
        let mut interp = EntityInterpolator::new();
        let spawn = Vec3::new(0.0, 64.0, 0.0);
        interp.update(&[item_snap_moving(9, spawn, None, false)], INTERP_WINDOW);

        let mut max_y = interp.draws()[0].feet.y;
        for _ in 0..16 {
            interp.update(&[item_snap_moving(9, spawn, None, false)], TICK);
            max_y = max_y.max(interp.draws()[0].feet.y);
        }
        // The one late correction a real server would send once the item has
        // fallen under its own (server-side) gravity for about a second.
        let landed = Vec3::new(0.3, 63.2, 0.0);
        interp.update(&[item_snap_moving(9, landed, None, true)], TICK);
        for _ in 0..10 {
            interp.update(&[item_snap_moving(9, landed, None, true)], TICK);
            max_y = max_y.max(interp.draws()[0].feet.y);
        }
        assert!(
            max_y <= spawn.y + 1.0e-3,
            "a position-only path has no apex to give; got max_y={max_y}"
        );
        assert!(
            (interp.draws()[0].feet.y - landed.y).abs() < 1.0e-3,
            "the ease should still land on the reported position"
        );
    }

    #[test]
    fn item_physics_is_paused_while_the_server_reports_it_grounded() {
        // Once a snapshot says the item is resting, the simulation must not
        // keep integrating gravity — a resting item should simply hold still
        // rather than be resimulated (and possibly drift) every tick. See
        // `item_pop_stops_at_a_real_floor_instead_of_sinking_through_it` below
        // for the airborne, collision-aware case this is deliberately not.
        let mut interp = EntityInterpolator::new();
        let resting = Vec3::new(2.0, 63.0, 4.0);
        interp.update(
            &[item_snap_moving(9, resting, Some(Vec3::ZERO), true)],
            INTERP_WINDOW,
        );
        for _ in 0..40 {
            interp.update(
                &[item_snap_moving(9, resting, Some(Vec3::ZERO), true)],
                TICK,
            );
        }
        let feet = interp.draws()[0].feet;
        assert!(
            (feet.y - resting.y).abs() < 1.0e-3,
            "a grounded item must hold its reported height, was {}",
            feet.y
        );
    }

    // ---- item collision (the second reported defect) ---------------------

    /// A single-block-thick floor at `y == floor_y`, everywhere in X/Z, and
    /// nothing else — the minimal [`CollisionView`] needed to prove
    /// [`step_item_physics`] actually stops a fall instead of free-falling
    /// through it. Reuses `lodestone_physics`'s own `Aabb`/`collide`, not a
    /// second collider: this only *describes* geometry, the sweep in
    /// `step_item_physics` (via `move_entity`) does the resolving.
    struct FlatFloor {
        floor_y: i32,
    }

    impl CollisionView for FlatFloor {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<lodestone_physics::Aabb>) {
            if y == self.floor_y {
                out.push(lodestone_physics::Aabb::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(z),
                    f64::from(x) + 1.0,
                    f64::from(y) + 1.0,
                    f64::from(z) + 1.0,
                ));
            }
        }
    }

    /// The reported defect: an item popped above a floor must come to rest
    /// *on* that floor, not sink through it while waiting for the server's
    /// next once-a-second correction (which, in this test, never arrives —
    /// exactly the sparse-correction reality the module docs describe).
    ///
    /// This is the discriminating case `item_pop_follows_a_ballistic_arc_not_a_flat_ease`
    /// cannot cover: that test asserts an apex exists in open air, never
    /// asserting anything about a floor, so a build that regressed
    /// `step_item_physics` back to `ItemMotion::tick`'s bare `position +=
    /// velocity` would still pass it while items fell through every floor in
    /// the game.
    #[test]
    fn item_pop_stops_at_a_real_floor_instead_of_sinking_through_it() {
        let mut interp = EntityInterpolator::new();
        let floor = FlatFloor { floor_y: 63 };
        let profile = PhysicsProfile::mc_1_21();
        let spawn = Vec3::new(0.5, 66.0, 0.5);
        // A real pop velocity (`ItemEntity`'s zero-arg constructor draws
        // `vy = 0.2`, small horizontal jitter), reported once and never again
        // — no further snapshot arrives for the rest of the test, matching
        // `updateInterval(20)`'s roughly-one-correction-per-second reality.
        let vel = Vec3::new(0.02, 0.2, 0.0);
        interp.update_with_view(
            &[item_snap_moving(9, spawn, Some(vel), false)],
            0.0,
            &floor,
            &profile,
        );

        let mut min_y = interp.draws()[0].feet.y;
        // Two seconds of real flight time at 20 Hz, well past both the apex
        // and the moment gravity alone would have carried an unresolved item
        // through `floor_y` and out the bottom of the world. Re-sending the
        // same stale snapshot every tick (rather than a fresh one) is what
        // "no further correction arrives" looks like here — the track must
        // not be dropped for want of a snapshot, and `last_reported` staying
        // put is exactly what lets the physics-driven `curr` keep moving
        // without a spurious "server moved it" re-anchor each frame.
        for _ in 0..40 {
            interp.update_with_view(
                &[item_snap_moving(9, spawn, Some(vel), false)],
                TICK,
                &floor,
                &profile,
            );
            min_y = min_y.min(interp.draws()[0].feet.y);
        }
        let final_feet = interp.draws()[0].feet;

        assert!(
            min_y >= floor.floor_y as f32 + 1.0 - 1.0e-3,
            "the item's feet must never read below the floor's top surface \
             ({}), got a minimum of {min_y} — it sank through",
            floor.floor_y + 1
        );
        assert!(
            (final_feet.y - (floor.floor_y as f32 + 1.0)).abs() < 1.0e-2,
            "the item must come to rest sitting on the floor, was {}",
            final_feet.y
        );
    }

    /// Negative control for the test above: with [`OpenAir`] (what plain
    /// [`EntityInterpolator::update`] uses) instead of a real floor, the same
    /// pop must fall straight through `floor_y` — proving the floor in the
    /// positive test is actually doing the stopping, not some incidental
    /// damping in `step_item_physics` itself.
    #[test]
    fn without_a_collision_view_the_same_pop_falls_through_the_floor_height() {
        let mut interp = EntityInterpolator::new();
        let spawn = Vec3::new(0.5, 66.0, 0.5);
        let vel = Vec3::new(0.02, 0.2, 0.0);
        interp.update(&[item_snap_moving(9, spawn, Some(vel), false)], 0.0);
        for _ in 0..40 {
            interp.update(&[item_snap_moving(9, spawn, Some(vel), false)], TICK);
        }
        let final_y = interp.draws()[0].feet.y;
        assert!(
            final_y < 63.0,
            "the control must actually fall past the floor height (63) to \
             prove the positive test's floor is load-bearing; got {final_y}"
        );
    }
}
